//! Structures de désérialisation des réponses de l'API Proxmox Mail Gateway.
//!
//! Mêmes principes que pour Proxmox VE et PBS : les nombres passent par [`Num`],
//! qui accepte aussi bien un entier qu'un flottant, un booléen ou une chaîne
//! numérique — PMG renvoie volontiers `"3287027"` pour un nombre de signatures et
//! `"0.82"` pour une charge —, et tous les champs sont optionnels. Une clé absente
//! — parce que la version de PMG ne la connaît pas encore, ou parce que le paquet
//! n'est pas installé — doit produire une métrique en moins, jamais une erreur.
//!
//! Les clés de l'API PMG sont en `snake_case` pour les statistiques
//! (`spamcount_in`) et en `kebab-case` pour ce qui vient des bibliothèques
//! communes à Proxmox (`active-state`, `public-key-bits`) : on renomme champ par
//! champ plutôt que de faire confiance à une convention globale.
//!
//! `Num` est volontairement recopié depuis les autres intégrations et non
//! partagé : chaque intégration reste autonome, c'est ce qui permet de la faire
//! évoluer ou de la retirer sans toucher aux autres.

use std::collections::BTreeMap;
use std::fmt;

use serde::Deserialize;
use serde::de::{self, Deserializer, Visitor};

/// Enveloppe commune à toutes les réponses de l'API : `{"data": ...}`.
#[derive(Debug, Deserialize)]
pub struct Envelope<T> {
    pub data: T,
}

/// Nombre tolérant : accepte entier, flottant, booléen ou chaîne numérique.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Num(pub f64);

impl<'de> Deserialize<'de> for Num {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct NumVisitor;

        impl Visitor<'_> for NumVisitor {
            type Value = Num;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a number or a numeric string")
            }

            fn visit_f64<E: de::Error>(self, v: f64) -> Result<Num, E> {
                Ok(Num(v))
            }

            fn visit_i64<E: de::Error>(self, v: i64) -> Result<Num, E> {
                Ok(Num(v as f64))
            }

            fn visit_u64<E: de::Error>(self, v: u64) -> Result<Num, E> {
                Ok(Num(v as f64))
            }

            fn visit_bool<E: de::Error>(self, v: bool) -> Result<Num, E> {
                Ok(Num(if v { 1.0 } else { 0.0 }))
            }

            fn visit_str<E: de::Error>(self, v: &str) -> Result<Num, E> {
                v.trim().parse::<f64>().map(Num).map_err(|_| {
                    de::Error::invalid_value(de::Unexpected::Str(v), &"a numeric string")
                })
            }
        }

        deserializer.deserialize_any(NumVisitor)
    }
}

/// `POST /api2/json/access/ticket`
#[derive(Debug, Deserialize)]
pub struct TicketResponse {
    pub ticket: String,
}

/// `GET /api2/json/version`
#[derive(Debug, Default, Deserialize)]
pub struct Version {
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub release: Option<String>,
    #[serde(default)]
    pub repoid: Option<String>,
}

/// Entrée de `GET /api2/json/nodes`.
#[derive(Debug, Default, Deserialize)]
pub struct NodeEntry {
    #[serde(default)]
    pub node: Option<String>,
}

/// `GET /api2/json/nodes/{node}/status`
#[derive(Debug, Default, Deserialize)]
pub struct NodeStatus {
    #[serde(default)]
    pub uptime: Option<Num>,
    /// Charge processeur, ratio 0..1.
    #[serde(default)]
    pub cpu: Option<Num>,
    /// Part du temps passé en attente d'entrées-sorties, ratio 0..1.
    #[serde(default)]
    pub wait: Option<Num>,
    /// Les trois moyennes de charge, données en chaînes (`"0.82"`).
    #[serde(default)]
    pub loadavg: Vec<Num>,
    #[serde(default)]
    pub cpuinfo: Option<CpuInfo>,
    #[serde(default)]
    pub memory: Option<Usage>,
    #[serde(default)]
    pub swap: Option<Usage>,
    #[serde(default, alias = "root")]
    pub rootfs: Option<Usage>,
    #[serde(default)]
    pub kversion: Option<String>,
    /// Base de règles synchronisée avec les autres nœuds de la grappe.
    #[serde(default)]
    pub insync: Option<Num>,
    /// Horloge du serveur, en secondes Unix.
    #[serde(default)]
    pub time: Option<Num>,
    /// `pmg-api/9.1.2/42245585286a`.
    #[serde(default)]
    pub pmgversion: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct CpuInfo {
    #[serde(default)]
    pub cpus: Option<Num>,
}

#[derive(Debug, Default, Deserialize)]
pub struct Usage {
    #[serde(default)]
    pub total: Option<Num>,
    #[serde(default)]
    pub used: Option<Num>,
    #[serde(default)]
    pub free: Option<Num>,
    #[serde(default)]
    pub avail: Option<Num>,
}

/// Entrée de `GET /api2/json/nodes/{node}/services`.
///
/// `state` reprend le `SubState` de systemd (`running`, `dead`, `exited`,
/// `failed`), `active-state` son `ActiveState` (`active`, `inactive`, `failed`)
/// et `unit-state` son `UnitFileState` (`enabled`, `disabled`, `static`) ou
/// `not-found` quand le paquet n'est pas installé.
#[derive(Debug, Default, Deserialize)]
pub struct ServiceEntry {
    #[serde(default)]
    pub service: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub desc: Option<String>,
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default, rename = "active-state")]
    pub active_state: Option<String>,
    #[serde(default, rename = "unit-state")]
    pub unit_state: Option<String>,
}

impl ServiceEntry {
    /// Nom du service, `service` étant la clé et `name` son doublon.
    pub fn id(&self) -> Option<&str> {
        self.service.as_deref().or(self.name.as_deref())
    }

    /// Vrai quand l'unité n'existe pas sur ce nœud : le paquet n'est pas
    /// installé. Aucune série n'est produite, ce n'est pas une panne.
    pub fn absent(&self) -> bool {
        self.unit_state.as_deref() == Some("not-found")
    }

    /// Vrai quand le service tourne. `active-state` est la source de vérité :
    /// une unité « oneshot » terminée correctement (`exited`) reste `active`.
    pub fn running(&self) -> bool {
        match self.active_state.as_deref() {
            Some(state) => state.eq_ignore_ascii_case("active"),
            // Avant que `active-state` existe, seul `state` était renvoyé.
            None => matches!(self.state.as_deref(), Some("running") | Some("exited")),
        }
    }
}

/// Une ligne de `GET /api2/json/nodes/{node}/postfix/qshape`.
///
/// Les colonnes sont dynamiques : `domain`, `total`, puis une tranche d'âge par
/// clé (`5m`, `10m`, … `1280m`, `1280m+`), toutes en chaînes. La première ligne
/// porte `domain = "TOTAL"` et agrège les autres.
#[derive(Debug, Default, Deserialize)]
pub struct QshapeRow {
    #[serde(default)]
    pub domain: Option<String>,
    #[serde(flatten)]
    pub columns: BTreeMap<String, Num>,
}

impl QshapeRow {
    pub fn is_total(&self) -> bool {
        self.domain.as_deref() == Some("TOTAL")
    }

    pub fn total(&self) -> Option<f64> {
        self.columns.get("total").map(|n| n.0)
    }
}

/// `GET /api2/json/statistics/mail`
///
/// Totaux du jour courant : PMG agrège par journée locale et ignore la fin de la
/// fenêtre demandée, seul `starttime` choisit le jour (voir `options.rs`).
#[derive(Debug, Default, Deserialize)]
pub struct MailStats {
    #[serde(default)]
    pub count_in: Option<Num>,
    #[serde(default)]
    pub count_out: Option<Num>,
    #[serde(default)]
    pub bytes_in: Option<Num>,
    #[serde(default)]
    pub bytes_out: Option<Num>,
    #[serde(default)]
    pub spamcount_in: Option<Num>,
    #[serde(default)]
    pub spamcount_out: Option<Num>,
    #[serde(default)]
    pub viruscount_in: Option<Num>,
    #[serde(default)]
    pub viruscount_out: Option<Num>,
    #[serde(default)]
    pub bounces_in: Option<Num>,
    #[serde(default)]
    pub bounces_out: Option<Num>,
    #[serde(default)]
    pub junk_in: Option<Num>,
    #[serde(default)]
    pub junk_out: Option<Num>,
    #[serde(default)]
    pub glcount: Option<Num>,
    #[serde(default)]
    pub spfcount: Option<Num>,
    #[serde(default)]
    pub rbl_rejects: Option<Num>,
    #[serde(default)]
    pub pregreet_rejects: Option<Num>,
    /// Temps de traitement moyen, en secondes.
    #[serde(default)]
    pub avptime: Option<Num>,
}

/// Une tranche de `GET /api2/json/statistics/recent`.
#[derive(Debug, Default, Deserialize)]
pub struct RecentPoint {
    #[serde(default)]
    pub time: Option<Num>,
    #[serde(default)]
    pub timespan: Option<Num>,
    #[serde(default)]
    pub count_in: Option<Num>,
    #[serde(default)]
    pub count_out: Option<Num>,
    #[serde(default)]
    pub spam_in: Option<Num>,
    #[serde(default)]
    pub virus_in: Option<Num>,
}

/// Une entrée de `GET /api2/json/statistics/spamscores`.
#[derive(Debug, Default, Deserialize)]
pub struct SpamScore {
    #[serde(default)]
    pub level: Option<String>,
    #[serde(default)]
    pub count: Option<Num>,
    /// Part du volume total, entre 0 et 1.
    #[serde(default)]
    pub ratio: Option<Num>,
}

/// Une entrée de `GET /api2/json/statistics/virus`.
#[derive(Debug, Default, Deserialize)]
pub struct VirusStat {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub count: Option<Num>,
}

/// `GET /api2/json/quarantine/spamstatus` et `/quarantine/virusstatus`.
#[derive(Debug, Default, Deserialize)]
pub struct QuarantineStatus {
    #[serde(default)]
    pub count: Option<Num>,
    /// Occupation estimée, en mébioctets.
    #[serde(default)]
    pub mbytes: Option<Num>,
    /// Niveau de spam moyen ; absent de la quarantaine antivirus.
    #[serde(default)]
    pub avgspam: Option<Num>,
}

/// Une entrée de `GET /api2/json/nodes/{node}/clamav/database`.
///
/// `build_time` n'est pas une date ISO : ClamAV écrit `16 Dec 2025 23-18 +0000`
/// dans l'en-tête de ses fichiers `.cvd`, et PMG le recopie tel quel.
#[derive(Debug, Default, Deserialize)]
pub struct ClamavDatabase {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default, rename = "type")]
    pub kind: Option<String>,
    #[serde(default)]
    pub build_time: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub nsigs: Option<Num>,
}

/// Une entrée de `GET /api2/json/nodes/{node}/spamassassin/rules`.
#[derive(Debug, Default, Deserialize)]
pub struct SpamassassinChannel {
    #[serde(default)]
    pub channel: Option<String>,
    /// Secondes Unix ; absent tant que le canal n'a jamais été mis à jour.
    #[serde(default)]
    pub last_updated: Option<Num>,
    #[serde(default)]
    pub update_avail: Option<Num>,
    #[serde(default)]
    pub version: Option<String>,
}

/// Une entrée de `GET /api2/json/config/cluster/status`.
///
/// Une installation autonome renvoie une liste vide : ce n'est pas une grappe
/// dégradée, c'est une grappe absente.
#[derive(Debug, Default, Deserialize)]
pub struct ClusterNode {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub ip: Option<String>,
    /// `master` ou `node`.
    #[serde(default, rename = "type")]
    pub kind: Option<String>,
    #[serde(default)]
    pub insync: Option<Num>,
    /// Message d'erreur du dernier échange ; vide quand tout va bien.
    #[serde(default)]
    pub conn_error: Option<String>,
}

/// `GET /api2/json/nodes/{node}/subscription`
#[derive(Debug, Default, Deserialize)]
pub struct Subscription {
    /// `new`, `active`, `invalid`, `expired`, `suspended`, `notfound`.
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub level: Option<String>,
    #[serde(default)]
    pub nextduedate: Option<String>,
}

/// Une entrée de `GET /api2/json/nodes/{node}/certificates/info`.
#[derive(Debug, Default, Deserialize)]
pub struct CertificateInfo {
    #[serde(default)]
    pub filename: Option<String>,
    #[serde(default)]
    pub subject: Option<String>,
    #[serde(default)]
    pub issuer: Option<String>,
    #[serde(default)]
    pub notafter: Option<Num>,
    #[serde(default)]
    pub san: Vec<String>,
}

/// Une entrée de `GET /api2/json/nodes/{node}/apt/update`.
#[derive(Debug, Default, Deserialize)]
pub struct AptUpdate {
    #[serde(default, rename = "Origin")]
    pub origin: Option<String>,
}

impl AptUpdate {
    /// Vrai pour une mise à jour issue d'un dépôt de sécurité Debian.
    pub fn is_security(&self) -> bool {
        self.origin.as_deref().is_some_and(|origin| {
            let origin = origin.to_ascii_lowercase();
            origin.contains("security")
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn un_nombre_se_lit_en_chaine_comme_en_entier() {
        #[derive(Deserialize)]
        struct T {
            v: Num,
        }
        for json in [r#"{"v":3287027}"#, r#"{"v":"3287027"}"#] {
            assert_eq!(serde_json::from_str::<T>(json).unwrap().v.0, 3_287_027.0);
        }
        assert_eq!(serde_json::from_str::<T>(r#"{"v":"0.82"}"#).unwrap().v.0, 0.82);
        assert_eq!(serde_json::from_str::<T>(r#"{"v":true}"#).unwrap().v.0, 1.0);
    }

    /// Relevé réel d'une passerelle 9.1 : les moyennes de charge sont des
    /// chaînes, `insync` un booléen déguisé en entier.
    const STATUS: &str = r#"{
      "cpu": 0, "wait": 0, "insync": 1, "uptime": 716766, "time": 1790078488,
      "loadavg": ["0.82", "1.94", "1.70"],
      "memory": {"free": 3148816384, "total": 16285110272, "used": 5851820032},
      "swap": {"used": 2867380224, "total": 8589930496, "free": 5722550272},
      "rootfs": {"free": 39514324992, "avail": 39514324992, "used": 207697952768, "total": 247212277760},
      "kversion": "Linux 7.2.5", "pmgversion": "pmg-api/9.1.2/42245585286a",
      "cpuinfo": {"cpus": 8, "cores": 6, "sockets": 1, "model": "Core i3-1315U"}
    }"#;

    #[test]
    fn letat_du_noeud_se_lit_tel_que_pmg_lecrit() {
        let status: NodeStatus = serde_json::from_str(STATUS).unwrap();
        assert_eq!(status.uptime.unwrap().0, 716_766.0);
        assert_eq!(status.loadavg.len(), 3);
        assert_eq!(status.loadavg[0].0, 0.82);
        assert_eq!(status.memory.unwrap().total.unwrap().0, 16_285_110_272.0);
        assert_eq!(status.rootfs.unwrap().avail.unwrap().0, 39_514_324_992.0);
        assert_eq!(status.cpuinfo.unwrap().cpus.unwrap().0, 8.0);
        assert_eq!(status.insync.unwrap().0, 1.0);
    }

    #[test]
    fn une_cle_inconnue_ou_absente_ne_fait_pas_echouer_la_lecture() {
        let status: NodeStatus = serde_json::from_str(r#"{"cle-du-futur": 1}"#).unwrap();
        assert!(status.uptime.is_none());
        assert!(status.loadavg.is_empty());
    }

    /// Relevé réel : la première ligne agrège, les colonnes sont des chaînes.
    const QSHAPE: &str = r#"[
      {"domain":"TOTAL","total":"7","5m":"5","10m":"0","20m":"0","40m":"2",
       "80m":"0","160m":"0","320m":"0","640m":"0","1280m":"0","1280m+":"0"},
      {"domain":"home.arpa","total":"7","5m":"5","40m":"2"}
    ]"#;

    #[test]
    fn une_ligne_de_qshape_expose_ses_tranches_dage() {
        let rows: Vec<QshapeRow> = serde_json::from_str(QSHAPE).unwrap();
        assert_eq!(rows.len(), 2);
        assert!(rows[0].is_total());
        assert_eq!(rows[0].total(), Some(7.0));
        assert_eq!(rows[0].columns.get("40m").unwrap().0, 2.0);
        assert!(!rows[1].is_total());
        assert_eq!(rows[1].domain.as_deref(), Some("home.arpa"));
    }

    #[test]
    fn un_service_absent_se_distingue_dun_service_arrete() {
        let entries: Vec<ServiceEntry> = serde_json::from_str(
            r#"[
              {"service":"postfix","name":"postfix","desc":"Postfix","state":"running",
               "active-state":"active","unit-state":"enabled"},
              {"service":"pmg-smtp-filter","name":"pmg-smtp-filter","desc":"filter",
               "state":"dead","active-state":"inactive","unit-state":"enabled"},
              {"service":"chrony","name":"chrony","desc":"","state":"unknown",
               "active-state":"unknown","unit-state":"not-found"},
              {"service":"pmg-daily","name":"pmg-daily","desc":"","state":"exited",
               "active-state":"active","unit-state":"static"}
            ]"#,
        )
        .unwrap();
        assert!(entries[0].running() && !entries[0].absent());
        assert!(!entries[1].running() && !entries[1].absent());
        assert!(entries[2].absent(), "un paquet non installé n'est pas une panne");
        assert!(entries[3].running(), "une unité oneshot terminée reste active");
        assert_eq!(entries[0].id(), Some("postfix"));
    }

    #[test]
    fn une_base_clamav_se_lit_avec_ses_nombres_en_chaine() {
        let bases: Vec<ClamavDatabase> = serde_json::from_str(
            r#"[{"nsigs":"3287027","name":"main","build_time":"16 Dec 2025 23-18 +0000",
                 "type":"ClamAV-VDB","version":"63"}]"#,
        )
        .unwrap();
        assert_eq!(bases[0].nsigs.unwrap().0, 3_287_027.0);
        assert_eq!(bases[0].name.as_deref(), Some("main"));
        assert_eq!(bases[0].build_time.as_deref(), Some("16 Dec 2025 23-18 +0000"));
    }

    #[test]
    fn un_canal_spamassassin_jamais_mis_a_jour_na_pas_de_date() {
        let channels: Vec<SpamassassinChannel> = serde_json::from_str(
            r#"[{"channel":"updates.spamassassin.org","update_avail":0,"version":"1938404",
                 "last_updated":1790078571},
                {"channel":"kam.sa-channels.mcgrail.com","update_avail":0}]"#,
        )
        .unwrap();
        assert_eq!(channels[0].last_updated.unwrap().0, 1_790_078_571.0);
        assert!(channels[1].last_updated.is_none());
    }

    #[test]
    fn une_mise_a_jour_de_securite_se_reconnait_a_son_origine() {
        let update = AptUpdate { origin: Some("Debian-Security".into()) };
        assert!(update.is_security());
        let update = AptUpdate { origin: Some("Proxmox".into()) };
        assert!(!update.is_security());
        assert!(!AptUpdate::default().is_security());
    }
}
