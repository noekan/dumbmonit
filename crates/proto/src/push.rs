//! Contrat de poussée entre l'agent système et le serveur.
//!
//! L'agent se connecte au serveur, et jamais l'inverse : ce sens traverse les NAT
//! et les pare-feux domestiques, et n'impose d'ouvrir aucun port sur la machine
//! surveillée. Tout ce qui transite est défini ici, et une seule fois, pour que
//! l'agent et le serveur ne puissent pas diverger sans que le compilateur le voie.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::Sample;

/// Version du dialogue de poussée.
///
/// Envoyée dans chaque lot : un serveur récent doit pouvoir refuser proprement un
/// agent trop ancien plutôt que d'interpréter de travers ses mesures.
pub const PUSH_PROTOCOL_VERSION: u32 = 1;

/// Chemin de la route de réception, partagé pour que l'agent n'ait pas à le
/// recopier — une URL en dur des deux côtés finit toujours par se désynchroniser.
pub const INGEST_PATH: &str = "/api/ingest";

/// Nombre maximal d'échantillons par lot.
///
/// Sert de garde-fou des deux côtés : l'agent découpe ses envois, le serveur refuse
/// ce qui dépasse. Une machine ordinaire produit de l'ordre de 150 échantillons par
/// cycle, cette borne laisse donc largement la place au rattrapage après coupure.
pub const MAX_BATCH_SAMPLES: usize = 10_000;

/// Préfixe des jetons d'enregistrement, à des fins de lisibilité dans l'interface
/// et de détection accidentelle dans un dépôt de code.
///
/// Purement cosmétique : le serveur retrouve un jeton par son empreinte, jamais
/// par son préfixe. Les jetons `ezym_` émis avant le renommage du produit restent
/// donc valables tels quels.
pub const TOKEN_PREFIX: &str = "dmon_";

/// Préfixe du secret de liaison propre à une machine.
///
/// Distinct de [`TOKEN_PREFIX`] à dessein : les deux secrets voyagent dans la
/// même requête, et une trace de journal doit dire du premier coup d'œil lequel
/// a fuité.
pub const AGENT_SECRET_PREFIX: &str = "dmab_";

/// En-tête portant le secret de liaison de la machine.
///
/// Pas l'en-tête `Authorization` : celui-ci porte le jeton d'enregistrement,
/// partagé par tout un parc. Le secret de liaison, lui, ne vaut que pour une
/// machine — les confondre reviendrait à reperdre la distinction qu'ils
/// existent pour établir.
pub const AGENT_SECRET_HEADER: &str = "x-dumbmonit-agent-secret";

/// Ce que la machine surveillée déclare d'elle-même.
///
/// C'est cette identité qui permet l'enregistrement automatique : un agent qui
/// présente un jeton valide et un nom d'hôte inconnu devient une cible, sans que
/// personne n'ait rien saisi dans l'interface.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentIdentity {
    /// Nom d'hôte tel que la machine se voit elle-même.
    pub hostname: String,
    /// Famille de système : `linux`, `windows`, `macos`.
    pub os: String,
    /// Version lisible du système : « Debian GNU/Linux 12 », « Windows 11 Pro ».
    #[serde(default)]
    pub os_version: Option<String>,
    #[serde(default)]
    pub kernel_version: Option<String>,
    /// Architecture : `x86_64`, `aarch64`.
    #[serde(default)]
    pub arch: Option<String>,
    /// Version du binaire agent, pour repérer un parc à mettre à jour.
    pub agent_version: String,
    /// Vrai si l'agent vient chercher les commandes du serveur (`commands: true`
    /// dans sa configuration). Absent chez un agent antérieur au canal de
    /// commandes : le serveur en déduit qu'il ne les exécutera pas.
    #[serde(default)]
    pub commands_enabled: Option<bool>,
    /// Vrai si l'agent accepte de relayer des interrogations (`relay: true`) :
    /// le serveur peut alors lui déléguer les sondes des équipements de son site.
    #[serde(default)]
    pub relay: bool,
    /// Site où l'agent est posé (« agence de Lyon »), déclaré dans sa
    /// configuration ; sert à regrouper les équipements relayés dans l'interface.
    #[serde(default)]
    pub site: Option<String>,
    /// Identifiant stable de la machine, indépendant du nom d'hôte.
    ///
    /// Renommer une machine ne doit pas créer une seconde cible et couper ses
    /// graphes en deux ; quand cet identifiant existe, il prime sur le nom d'hôte.
    #[serde(default)]
    pub machine_id: Option<String>,
    /// Étiquettes libres déclarées dans la configuration de l'agent.
    #[serde(default)]
    pub tags: BTreeMap<String, String>,
    /// Vrai si ce binaire sait recevoir et représenter un secret de liaison.
    ///
    /// Absent chez un agent antérieur à la liaison : le serveur s'interdit alors
    /// de lui en attribuer un, car il l'ignorerait et se verrait refuser son lot
    /// suivant. C'est ce drapeau, et lui seul, qui rend la mise à niveau
    /// progressive possible dans les deux sens.
    #[serde(default)]
    pub binding_supported: bool,
}

impl AgentIdentity {
    /// Clé d'identité utilisée pour retrouver la cible d'un envoi à l'autre.
    ///
    /// L'identifiant machine est préféré au nom d'hôte précisément parce qu'il
    /// survit à un renommage.
    pub fn key(&self) -> String {
        match self.machine_id.as_deref().map(str::trim).filter(|id| !id.is_empty()) {
            Some(id) => id.to_string(),
            None => self.hostname.trim().to_lowercase(),
        }
    }
}

/// Un envoi de l'agent vers le serveur.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PushBatch {
    /// Toujours [`PUSH_PROTOCOL_VERSION`] au moment de l'émission.
    pub protocol: u32,
    pub identity: AgentIdentity,
    /// Horodatage d'émission, en millisecondes depuis l'époque Unix.
    ///
    /// Distinct de l'horodatage des échantillons : un lot rattrapé après une
    /// coupure porte des mesures anciennes mais une émission récente, et c'est
    /// l'écart entre les deux qui révèle le retard.
    pub sent_at_ms: i64,
    pub samples: Vec<Sample>,
}

impl PushBatch {
    pub fn new(identity: AgentIdentity, samples: Vec<Sample>, sent_at_ms: i64) -> Self {
        Self { protocol: PUSH_PROTOCOL_VERSION, identity, sent_at_ms, samples }
    }
}

/// Réponse du serveur à un lot accepté.
///
/// Le serveur en profite pour piloter l'agent : c'est ce qui permet de changer la
/// période d'échantillonnage d'une machine depuis l'interface, sans se connecter
/// dessus ni redémarrer quoi que ce soit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PushAck {
    /// Identifiant de la cible sous laquelle les mesures ont été rangées.
    pub target_id: i64,
    /// Nombre d'échantillons retenus.
    pub accepted: usize,
    /// Période d'échantillonnage souhaitée, en secondes.
    pub interval_secs: u64,
    /// Vrai si ce lot vient de créer la cible.
    #[serde(default)]
    pub registered: bool,
    /// Secret de liaison attribué à cette machine, la seule et unique fois.
    ///
    /// Présent uniquement au moment de la liaison ; le serveur n'en garde que
    /// l'empreinte. L'agent l'écrit sur son disque en `0600` et le présente dans
    /// [`AGENT_SECRET_HEADER`] à chaque requête suivante.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_secret: Option<String>,
    /// Vrai quand le serveur considère cette machine comme liée.
    ///
    /// Faux pour un agent antérieur à la liaison : c'est ce que l'interface
    /// affiche sous « not bound yet ».
    #[serde(default)]
    pub bound: bool,
}

#[cfg(test)]
mod tests {
    use crate::MetricKind;

    use super::*;

    fn identity() -> AgentIdentity {
        AgentIdentity {
            hostname: "nas-salon".into(),
            os: "linux".into(),
            os_version: Some("Debian GNU/Linux 12".into()),
            kernel_version: Some("6.1.0".into()),
            arch: Some("x86_64".into()),
            agent_version: "0.1.0".into(),
            commands_enabled: Some(true),
            relay: false,
            site: None,
            machine_id: Some("9f4c…".into()),
            tags: BTreeMap::from([("salle".to_string(), "cave".to_string())]),
            binding_supported: true,
        }
    }

    #[test]
    fn a_batch_survives_a_round_trip_through_json() {
        let batch = PushBatch::new(
            identity(),
            vec![Sample::new("cpu_usage_percent", 12.5, MetricKind::Gauge, 1_700_000_000_000)],
            1_700_000_000_123,
        );
        let json = serde_json::to_string(&batch).expect("sérialisation");
        let decoded: PushBatch = serde_json::from_str(&json).expect("désérialisation");
        assert_eq!(decoded, batch);
        assert_eq!(decoded.protocol, PUSH_PROTOCOL_VERSION);
    }

    #[test]
    fn optional_identity_fields_may_be_omitted() {
        // Un agent minimal — ou plus ancien — n'envoie que l'indispensable ; le
        // serveur doit l'accepter sans renvoyer une erreur de désérialisation.
        let json = r#"{
            "protocol": 1,
            "identity": { "hostname": "pi", "os": "linux", "agent_version": "0.1.0" },
            "sent_at_ms": 0,
            "samples": []
        }"#;
        let batch: PushBatch = serde_json::from_str(json).expect("désérialisation");
        assert_eq!(batch.identity.hostname, "pi");
        assert!(batch.identity.machine_id.is_none());
        assert!(batch.identity.tags.is_empty());
        // Un agent d'avant le canal de commandes ne dit rien de ses capacités.
        assert_eq!(batch.identity.commands_enabled, None);
        // Ni d'avant la liaison : le serveur ne doit surtout pas lui attribuer un
        // secret qu'il jetterait, ce qui le verrouillerait au lot suivant.
        assert!(!batch.identity.binding_supported);
    }

    #[test]
    fn an_older_agent_reads_an_ack_that_carries_a_binding() {
        // Le champ est ajouté : un agent antérieur doit continuer à lire l'accusé
        // de réception sans erreur, et un serveur antérieur à en produire un que
        // l'agent récent accepte.
        let ack: PushAck =
            serde_json::from_str(r#"{"target_id":7,"accepted":3,"interval_secs":30}"#)
                .expect("désérialisation");
        assert!(ack.agent_secret.is_none());
        assert!(!ack.bound);

        let issued = PushAck {
            target_id: 7,
            accepted: 3,
            interval_secs: 30,
            registered: true,
            agent_secret: Some(format!("{AGENT_SECRET_PREFIX}abcdef")),
            bound: true,
        };
        let json = serde_json::to_string(&issued).expect("sérialisation");
        assert!(json.contains(AGENT_SECRET_PREFIX));
        // Le secret n'occupe la réponse qu'au moment où il est attribué.
        let plain = PushAck { agent_secret: None, ..issued.clone() };
        assert!(!serde_json::to_string(&plain).unwrap().contains("agent_secret"));
        assert_eq!(serde_json::from_str::<PushAck>(&json).unwrap(), issued);
    }

    #[test]
    fn the_two_secrets_of_the_agent_channel_never_look_alike() {
        // Un secret de liaison égaré dans un journal doit se distinguer d'un
        // jeton d'enregistrement au premier coup d'œil.
        assert_ne!(TOKEN_PREFIX, AGENT_SECRET_PREFIX);
        assert!(AGENT_SECRET_HEADER.chars().all(|c| c.is_ascii_lowercase() || c == '-'));
    }

    #[test]
    fn the_machine_id_takes_precedence_over_the_hostname() {
        let identity = identity();
        assert_eq!(identity.key(), "9f4c…");
    }

    #[test]
    fn without_a_machine_id_the_key_falls_back_to_a_normalised_hostname() {
        let mut identity = identity();
        identity.machine_id = None;
        identity.hostname = "  NAS-Salon ".into();
        assert_eq!(identity.key(), "nas-salon");

        // Une chaîne vide ne doit pas passer pour un identifiant machine valide,
        // sans quoi toutes les machines mal configurées se partageraient une cible.
        identity.machine_id = Some("   ".into());
        assert_eq!(identity.key(), "nas-salon");
    }

    #[test]
    fn an_acknowledgement_survives_a_round_trip() {
        let ack = PushAck {
            target_id: 7,
            accepted: 42,
            interval_secs: 30,
            registered: true,
            agent_secret: None,
            bound: true,
        };
        let decoded: PushAck =
            serde_json::from_str(&serde_json::to_string(&ack).unwrap()).expect("désérialisation");
        assert_eq!(decoded, ack);
    }
}
