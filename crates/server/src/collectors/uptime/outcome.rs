//! Rapport d'une sonde de disponibilité, et métriques communes aux cinq sondes.
//!
//! # Pourquoi `probe_success` en plus de `up`
//!
//! `Registry::probe` pose `up = 1` lorsqu'un collecteur renvoie `Ok`, et n'écrit
//! rien lorsqu'il renvoie `Err` : la règle « équipement injoignable » détecte
//! l'interruption de la série. Ce modèle est juste pour du matériel — un switch
//! éteint ne répond pas, point.
//!
//! Il ne l'est pas pour un moniteur de disponibilité, pour deux raisons.
//!
//! 1. **Un service peut répondre et être en panne.** Un `500`, un mot-clé disparu,
//!    un certificat périmé : le serveur répond, donc `Err` serait mensonger, mais
//!    le service ne rend pas son office. Renvoyer `Ok` sans plus le rendrait
//!    invisible.
//! 2. **Un taux de disponibilité se calcule sur des points, pas sur des trous.**
//!    `avg_over_time(ezymonit_probe_success[30d])` ignore purement et simplement
//!    les intervalles sans échantillon. Si une panne se traduisait par l'absence
//!    d'écriture, une coupure de trois jours ne ferait pas bouger le pourcentage
//!    d'un iota — exactement le chiffre que l'utilisateur vient chercher.
//!
//! D'où la règle appliquée par toutes les sondes de ce module :
//!
//! * la sonde renvoie `Err(ProbeError::Config)` **uniquement** quand l'utilisateur
//!   s'est trompé (adresse illisible, option invalide, `CAP_NET_RAW` absent). Rien
//!   n'est écrit, l'interface affiche l'erreur, aucune alerte de panne ne part :
//!   un moniteur mal réglé ne doit pas se faire passer pour une panne ;
//! * dans **tous** les autres cas — y compris connexion refusée, délai dépassé,
//!   poignée de main TLS échouée — la sonde renvoie `Ok` avec
//!   `probe_success = 0` et une raison d'échec. La mesure a réussi ; c'est son
//!   résultat qui est négatif.
//!
//! Conséquence assumée : sur ces cibles, `up = 1` signifie « le moniteur a tourné »
//! et non « le service va bien ». C'est cohérent — le collecteur a bien fait son
//! travail — et cela donne deux signaux distincts au lieu d'un seul confus :
//! `probe_success = 0` dit que le service est tombé, l'interruption de `ezymonit_up`
//! dit que c'est la surveillance elle-même qui est tombée. L'interface et les
//! règles d'alerte doivent donc s'appuyer sur `probe_success` pour ces types de
//! cibles, jamais sur `up`.
//!
//! C'est aussi pourquoi chaque sonde applique son propre délai, plus court que
//! celui du planificateur : interrompue par le registre, elle n'écrirait pas son
//! zéro.

use std::time::Instant;

use ezymonit_proto::{MetricKind, Sample};

/// Préfixe commun à toutes les métriques de disponibilité.
///
/// Il est volontairement identique d'une sonde à l'autre : une règle unique
/// (`ezymonit_probe_success == 0`) couvre alors HTTP, TCP, DNS, ICMP et TLS, et
/// l'étiquette `probe` permet de restreindre quand c'est utile.
pub const PREFIX: &str = "probe_";

/// Raison d'un échec, en jeu fermé.
///
/// Le libellé part en étiquette : le jeu doit rester petit et stable, faute de
/// quoi la cardinalité de `probe_failure_info` suivrait la créativité des messages
/// d'erreur. Les valeurs sont sans accent ni espace : ce sont des jetons de
/// requête, pas de la prose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    /// Le nom n'a pas pu être résolu.
    Dns,
    /// Connexion refusée, réseau injoignable, hôte inconnu.
    Connect,
    /// Délai propre à la sonde dépassé.
    Timeout,
    /// Poignée de main TLS impossible, ou certificat refusé.
    Tls,
    /// Certificat présenté mais périmé.
    CertExpired,
    /// Code de statut HTTP hors des codes acceptés.
    Status,
    /// Mot-clé attendu absent, ou mot-clé interdit présent.
    Keyword,
    /// Chemin JSON absent, ou valeur différente de celle attendue.
    Json,
    /// Réponse illisible : corps tronqué, JSON invalide.
    Body,
    /// Perte de paquets au-delà du seuil toléré.
    PacketLoss,
    /// Réponse DNS reçue mais sans l'enregistrement attendu.
    Record,
}

impl Failure {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Dns => "dns",
            Self::Connect => "connect",
            Self::Timeout => "timeout",
            Self::Tls => "tls",
            Self::CertExpired => "cert_expired",
            Self::Status => "status",
            Self::Keyword => "keyword",
            Self::Json => "json",
            Self::Body => "body",
            Self::PacketLoss => "packet_loss",
            Self::Record => "record",
        }
    }
}

/// Ce qu'une sonde a mesuré, en cours de construction.
///
/// Le chronomètre démarre à la création : les options sont donc analysées avant,
/// et `probe_duration_seconds` ne mesure que le travail réseau.
pub struct Report {
    started: Instant,
    ts_ms: i64,
    /// Étiquettes d'identité de la sonde (`probe`, `url`, `port`…), appliquées à
    /// tous les échantillons pour qu'aucune sonde ne puisse en oublier une.
    labels: Vec<(String, String)>,
    samples: Vec<Sample>,
    failure: Option<(Failure, String)>,
}

impl Report {
    /// `kind` est le type de sonde (`http`, `tcp`, `dns`, `ping`, `tls`).
    pub fn new(kind: &'static str) -> Self {
        Self {
            started: Instant::now(),
            ts_ms: chrono::Utc::now().timestamp_millis(),
            labels: vec![("probe".to_string(), kind.to_string())],
            samples: Vec::new(),
            failure: None,
        }
    }

    /// Ajoute une étiquette d'identité, portée par tous les échantillons.
    pub fn label(mut self, key: &str, value: impl Into<String>) -> Self {
        self.labels.push((key.to_string(), value.into()));
        self
    }

    pub fn gauge(&mut self, metric: &str, value: f64) {
        self.samples.push(Sample::new(
            format!("{PREFIX}{metric}"),
            value,
            MetricKind::Gauge,
            self.ts_ms,
        ));
    }

    /// Même chose, mais avec une étiquette supplémentaire propre à cet échantillon
    /// — utilisée pour les séries `*_info`, dont seule l'étiquette porte le sens.
    pub fn gauge_with(&mut self, metric: &str, value: f64, key: &str, label_value: &str) {
        self.samples.push(
            Sample::new(format!("{PREFIX}{metric}"), value, MetricKind::Gauge, self.ts_ms)
                .with_label(key, label_value),
        );
    }

    /// Déclare la sonde en échec. Le premier échec constaté l'emporte : c'est celui
    /// qui a interrompu le déroulement, les suivants n'en seraient que la
    /// conséquence.
    pub fn fail(&mut self, reason: Failure, detail: impl Into<String>) {
        if self.failure.is_none() {
            self.failure = Some((reason, detail.into()));
        }
    }

    pub fn is_up(&self) -> bool {
        self.failure.is_none()
    }

    /// Détail lisible du premier échec, pour le journal. Jamais mis en étiquette :
    /// il contient des messages système de cardinalité non bornée.
    pub fn detail(&self) -> Option<&str> {
        self.failure.as_ref().map(|(_, detail)| detail.as_str())
    }

    pub fn timestamp_ms(&self) -> i64 {
        self.ts_ms
    }

    /// Clôt la mesure et produit les échantillons, métriques communes comprises.
    pub fn finish(mut self) -> Vec<Sample> {
        let elapsed = self.started.elapsed().as_secs_f64();
        let up = self.is_up();

        self.gauge("success", if up { 1.0 } else { 0.0 });
        self.gauge("duration_seconds", elapsed);
        if let Some((reason, _)) = self.failure {
            // Série de présence : seule l'étiquette compte, la valeur vaut toujours 1.
            // Elle n'est émise qu'en cas d'échec, pour que l'interface puisse
            // afficher « pourquoi » sans avoir à interpréter dix métriques.
            self.gauge_with("failure_info", 1.0, "reason", reason.as_str());
        }

        for sample in &mut self.samples {
            for (key, value) in &self.labels {
                sample.labels.insert(key.clone(), value.clone());
            }
        }
        self.samples
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn une_sonde_reussie_produit_un_succes_et_une_duree() {
        let report = Report::new("tcp").label("port", "22");
        let samples = report.finish();

        let success = samples.iter().find(|s| s.metric == "probe_success").expect("probe_success");
        assert_eq!(success.value, 1.0);
        assert_eq!(success.labels.get("probe").map(String::as_str), Some("tcp"));
        assert_eq!(success.labels.get("port").map(String::as_str), Some("22"));
        assert!(samples.iter().any(|s| s.metric == "probe_duration_seconds"));
        assert!(
            !samples.iter().any(|s| s.metric == "probe_failure_info"),
            "aucune raison d'échec quand tout va bien"
        );
    }

    /// Le point central du module : un service injoignable écrit bel et bien un
    /// zéro. Sans lui, `avg_over_time` ignorerait la panne et le taux de
    /// disponibilité resterait à 100 % pendant la coupure.
    #[test]
    fn un_service_injoignable_ecrit_un_zero_et_non_un_trou() {
        let mut report = Report::new("http");
        report.fail(Failure::Connect, "connexion refusée");
        let samples = report.finish();

        let success = samples.iter().find(|s| s.metric == "probe_success").expect("probe_success");
        assert_eq!(success.value, 0.0);

        let info = samples.iter().find(|s| s.metric == "probe_failure_info").expect("raison");
        assert_eq!(info.labels.get("reason").map(String::as_str), Some("connect"));
        assert_eq!(info.value, 1.0);
    }

    #[test]
    fn le_premier_echec_constate_lemporte() {
        let mut report = Report::new("http");
        report.fail(Failure::Status, "503");
        report.fail(Failure::Keyword, "mot-clé absent");
        assert_eq!(report.detail(), Some("503"));
        let samples = report.finish();
        let info = samples.iter().find(|s| s.metric == "probe_failure_info").unwrap();
        assert_eq!(info.labels.get("reason").map(String::as_str), Some("status"));
    }

    #[test]
    fn les_etiquettes_didentite_sont_posees_sur_tous_les_echantillons() {
        let mut report = Report::new("dns").label("record_type", "A");
        report.gauge("dns_answer_records", 2.0);
        for sample in report.finish() {
            assert_eq!(sample.labels.get("probe").map(String::as_str), Some("dns"));
            assert_eq!(sample.labels.get("record_type").map(String::as_str), Some("A"));
            assert!(sample.metric.starts_with("probe_"), "{}", sample.metric);
        }
    }

    #[test]
    fn les_raisons_dechec_sont_des_jetons_de_requete() {
        let toutes = [
            Failure::Dns,
            Failure::Connect,
            Failure::Timeout,
            Failure::Tls,
            Failure::CertExpired,
            Failure::Status,
            Failure::Keyword,
            Failure::Json,
            Failure::Body,
            Failure::PacketLoss,
            Failure::Record,
        ];
        let mut vus: Vec<&str> = Vec::new();
        for raison in toutes {
            let libelle = raison.as_str();
            assert!(
                libelle.chars().all(|c| c.is_ascii_lowercase() || c == '_'),
                "« {libelle} » n'est pas un jeton utilisable en requête"
            );
            assert!(!vus.contains(&libelle), "raison dupliquée : {libelle}");
            vus.push(libelle);
        }
    }
}
