//! Options de collecte lues sur la cible.
//!
//! Le modèle `Target` n'a pas de champs propres à Proxmox : les réglages passent
//! donc par `Target::tags`, qui est déjà éditable dans l'interface et sauvegardé
//! avec la cible. Chaque option a une valeur par défaut sûre, de sorte qu'une
//! cible sans aucune étiquette fonctionne sur une installation standard.

use std::time::Duration;

use dumbmonit_proto::{ProbeError, Target};

/// Port d'écoute de l'interface d'administration de Proxmox VE.
const DEFAULT_PORT: u16 = 8006;

/// Délai appliqué à chaque requête HTTP.
///
/// Il est volontairement bien plus court que le délai global du planificateur :
/// c'est ce qui permet à un nœud injoignable d'échouer vite et de laisser le
/// temps aux autres nœuds du cluster de répondre.
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// Fenêtre d'examen des tâches `vzdump`. Un mois couvre les rotations
/// hebdomadaires et mensuelles sans ramener un historique inutilement long.
const DEFAULT_BACKUP_LOOKBACK_DAYS: u32 = 31;

/// Nombre maximal de tâches ramenées par nœud.
const TASK_LIMIT: u32 = 500;

/// Nombre d'invités dont les instantanés sont inventoriés à chaque collecte.
///
/// Un appel par invité : au-delà de deux cents machines, on préfère en sauter
/// quelques-unes — et le dire — plutôt que d'allonger chaque collecte.
const DEFAULT_MAX_SNAPSHOT_GUESTS: u32 = 200;

#[derive(Debug, Clone)]
pub struct Options {
    /// Racine de l'API, sans barre oblique finale : `https://pve.lan:8006`.
    pub base_url: String,
    /// Acceptation d'un certificat non vérifiable. Toujours un choix explicite.
    pub insecure_tls: bool,
    pub request_timeout: Duration,
    /// Ancienneté maximale des tâches `vzdump` examinées, en secondes.
    pub backup_lookback_seconds: i64,
    pub task_limit: u32,
    /// Datation des sauvegardes par inventaire des archives sur les stockages.
    /// C'est la seule source fiable par machine, mais elle coûte une requête par
    /// stockage de sauvegarde : on laisse la possibilité de la couper.
    pub scan_backup_storage: bool,
    /// Restriction à un sous-ensemble de nœuds. Vide signifie « tous ».
    pub nodes: Vec<String>,
    /// État de la haute disponibilité (`/cluster/ha/status/current`).
    pub ha: bool,
    /// Travaux de sauvegarde planifiés et invités non couverts (`/cluster/backup`).
    pub backup_jobs: bool,
    /// Inventaire des instantanés, un appel par invité.
    pub scan_snapshots: bool,
    /// Plafond d'invités inventoriés pour les instantanés, par collecte.
    pub max_snapshot_guests: u32,
    /// État des travaux de réplication (`/nodes/{node}/replication`).
    pub replication: bool,
    /// Santé Ceph (`/cluster/ceph/status`), silencieux si Ceph n'est pas installé.
    pub ceph: bool,
    /// Mises à jour en attente (`/nodes/{node}/apt/update`), silencieux sans droit.
    pub updates: bool,
    /// Expiration des certificats des nœuds (`/nodes/{node}/certificates/info`).
    pub certificates: bool,
    /// Détail des machines virtuelles en marche (`status/current`) et, quand
    /// l'agent QEMU est activé, leurs systèmes de fichiers (`agent/get-fsinfo`).
    pub guest_agent: bool,
    /// Disques physiques et SMART (`/nodes/{node}/disks/list`, `disks/smart`).
    pub disks: bool,
    /// Pools ZFS (`/nodes/{node}/disks/zfs`).
    pub zfs: bool,
    /// Versions des paquets importants (`/nodes/{node}/apt/versions`), pour
    /// signaler un changement entre deux interrogations.
    pub packages: bool,
    /// Abonnement et dépôts APT (`/nodes/{node}/subscription`, `apt/repositories`).
    pub subscription: bool,
}

impl Options {
    pub fn from_target(target: &Target) -> Result<Self, ProbeError> {
        let port = parse_port(tag(target, "port"))?;
        let base_url = base_url(&target.address, port)?;

        Ok(Self {
            base_url,
            insecure_tls: parse_bool(tag(target, "insecure_tls"))?,
            request_timeout: parse_timeout(tag(target, "request_timeout_seconds"))?,
            backup_lookback_seconds: parse_lookback(tag(target, "backup_lookback_days"))?,
            task_limit: TASK_LIMIT,
            // Activé par défaut : sans lui, l'ancienneté des sauvegardes n'est
            // connue que pour les machines sauvegardées individuellement.
            scan_backup_storage: parse_bool_or(tag(target, "scan_backup_storage"), true)?,
            nodes: tag(target, "nodes")
                .map(|value| {
                    value
                        .split(',')
                        .map(str::trim)
                        .filter(|name| !name.is_empty())
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default(),
            ha: parse_bool_or(tag(target, "ha"), true)?,
            backup_jobs: parse_bool_or(tag(target, "backup_jobs"), true)?,
            scan_snapshots: parse_bool_or(tag(target, "scan_snapshots"), true)?,
            max_snapshot_guests: parse_max_snapshot_guests(tag(target, "max_snapshot_guests"))?,
            replication: parse_bool_or(tag(target, "replication"), true)?,
            ceph: parse_bool_or(tag(target, "ceph"), true)?,
            updates: parse_bool_or(tag(target, "updates"), true)?,
            certificates: parse_bool_or(tag(target, "certificates"), true)?,
            guest_agent: parse_bool_or(tag(target, "guest_agent"), true)?,
            disks: parse_bool_or(tag(target, "disks"), true)?,
            zfs: parse_bool_or(tag(target, "zfs"), true)?,
            packages: parse_bool_or(tag(target, "packages"), true)?,
            subscription: parse_bool_or(tag(target, "subscription"), true)?,
        })
    }

    /// Vrai si le nœud doit être interrogé, compte tenu du filtre éventuel.
    pub fn wants_node(&self, node: &str) -> bool {
        self.nodes.is_empty() || self.nodes.iter().any(|wanted| wanted == node)
    }
}

fn tag<'a>(target: &'a Target, key: &str) -> Option<&'a str> {
    target.tags.get(key).map(|value| value.trim()).filter(|value| !value.is_empty())
}

fn parse_bool(value: Option<&str>) -> Result<bool, ProbeError> {
    parse_bool_or(value, false)
}

fn parse_bool_or(value: Option<&str>, default: bool) -> Result<bool, ProbeError> {
    match value {
        None => Ok(default),
        Some(raw) => match raw.to_ascii_lowercase().as_str() {
            "true" | "1" | "yes" | "oui" | "on" => Ok(true),
            "false" | "0" | "no" | "non" | "off" => Ok(false),
            other => Err(ProbeError::Config(format!(
                "Expected a boolean value (true/false), got \"{other}\""
            ))),
        },
    }
}

fn parse_port(value: Option<&str>) -> Result<u16, ProbeError> {
    match value {
        None => Ok(DEFAULT_PORT),
        Some(raw) => {
            raw.parse().map_err(|_| ProbeError::Config(format!("Invalid port: \"{raw}\"")))
        }
    }
}

fn parse_timeout(value: Option<&str>) -> Result<Duration, ProbeError> {
    match value {
        None => Ok(DEFAULT_REQUEST_TIMEOUT),
        Some(raw) => {
            let seconds: u64 = raw
                .parse()
                .map_err(|_| ProbeError::Config(format!("Invalid timeout: \"{raw}\"")))?;
            if !(1..=120).contains(&seconds) {
                return Err(ProbeError::Config(
                    "Request timeout must be between 1 and 120 seconds".to_string(),
                ));
            }
            Ok(Duration::from_secs(seconds))
        }
    }
}

fn parse_lookback(value: Option<&str>) -> Result<i64, ProbeError> {
    let days = match value {
        None => DEFAULT_BACKUP_LOOKBACK_DAYS,
        Some(raw) => raw
            .parse()
            .map_err(|_| ProbeError::Config(format!("Invalid number of days: \"{raw}\"")))?,
    };
    if !(1..=3650).contains(&days) {
        return Err(ProbeError::Config(
            "backup_lookback_days must be between 1 and 3650".to_string(),
        ));
    }
    Ok(i64::from(days) * 86_400)
}

fn parse_max_snapshot_guests(value: Option<&str>) -> Result<u32, ProbeError> {
    let count = match value {
        None => DEFAULT_MAX_SNAPSHOT_GUESTS,
        Some(raw) => raw
            .parse()
            .map_err(|_| ProbeError::Config(format!("Invalid number of guests: \"{raw}\"")))?,
    };
    if !(1..=10_000).contains(&count) {
        return Err(ProbeError::Config(
            "max_snapshot_guests must be between 1 and 10000".to_string(),
        ));
    }
    Ok(count)
}

/// Compose la racine de l'API à partir de l'adresse saisie par l'utilisateur.
///
/// L'adresse est un champ libre : on y trouve aussi bien `10.0.0.1` que
/// `pve.lan:8006` ou une URL complète derrière un proxy inverse. Le schéma reste
/// `https` par défaut, Proxmox ne servant pas son API en clair.
fn base_url(address: &str, port: u16) -> Result<String, ProbeError> {
    let address = address.trim().trim_end_matches('/');
    if address.is_empty() {
        return Err(ProbeError::Config("Device address is empty".to_string()));
    }

    if address.starts_with("https://") || address.starts_with("http://") {
        return Ok(address.to_string());
    }

    // Une IPv6 nue contient plusieurs deux-points : sans crochets, l'URL serait
    // ambiguë et le dernier groupe passerait pour un port.
    if address.starts_with('[') {
        return Ok(match address.rfind("]:") {
            Some(_) => format!("https://{address}"),
            None => format!("https://{address}:{port}"),
        });
    }
    if address.matches(':').count() > 1 {
        return Ok(format!("https://[{address}]:{port}"));
    }
    if address.contains(':') {
        return Ok(format!("https://{address}"));
    }
    Ok(format!("https://{address}:{port}"))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use dumbmonit_proto::Credential;

    use super::*;

    fn cible(tags: &[(&str, &str)]) -> Target {
        Target {
            id: 1,
            name: "pve".into(),
            address: "10.0.0.10".into(),
            kind: "proxmox".into(),
            profile_id: None,
            parent_id: None,
            interval: Duration::from_secs(60),
            enabled: true,
            tags: tags
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect::<BTreeMap<_, _>>(),
            credential: Credential::None,
        }
    }

    #[test]
    fn les_valeurs_par_defaut_conviennent_a_une_installation_standard() {
        let options = Options::from_target(&cible(&[])).unwrap();
        assert_eq!(options.base_url, "https://10.0.0.10:8006");
        assert!(!options.insecure_tls, "la vérification TLS reste active par défaut");
        assert!(options.scan_backup_storage);
        assert_eq!(options.backup_lookback_seconds, 31 * 86_400);
        assert!(options.nodes.is_empty());
        // Les inventaires complémentaires sont tous actifs : chacun sait se taire
        // quand la fonctionnalité ou le droit manque.
        assert!(options.ha);
        assert!(options.backup_jobs);
        assert!(options.scan_snapshots);
        assert_eq!(options.max_snapshot_guests, 200);
        assert!(options.replication);
        assert!(options.ceph);
        assert!(options.updates);
        assert!(options.certificates);
        assert!(options.guest_agent);
        assert!(options.disks);
        assert!(options.zfs);
        assert!(options.packages);
        assert!(options.subscription);
    }

    /// Lecture d'un drapeau d'inventaire, pour parcourir les douze en boucle.
    type Lecture = fn(&Options) -> bool;

    #[test]
    fn chaque_inventaire_complementaire_se_coupe_par_etiquette() {
        let cas: [(&str, Lecture); 12] = [
            ("ha", |o| o.ha),
            ("backup_jobs", |o| o.backup_jobs),
            ("scan_snapshots", |o| o.scan_snapshots),
            ("replication", |o| o.replication),
            ("ceph", |o| o.ceph),
            ("updates", |o| o.updates),
            ("certificates", |o| o.certificates),
            ("guest_agent", |o| o.guest_agent),
            ("disks", |o| o.disks),
            ("zfs", |o| o.zfs),
            ("packages", |o| o.packages),
            ("subscription", |o| o.subscription),
        ];
        for (etiquette, lire) in cas {
            let options = Options::from_target(&cible(&[(etiquette, "false")])).unwrap();
            assert!(!lire(&options), "« {etiquette} = false » aurait dû couper l'inventaire");
            let error = Options::from_target(&cible(&[(etiquette, "jamais")])).unwrap_err();
            assert!(matches!(error, ProbeError::Config(_)));
        }
    }

    #[test]
    fn le_plafond_dinstantanes_est_borne() {
        let options = Options::from_target(&cible(&[("max_snapshot_guests", "50")])).unwrap();
        assert_eq!(options.max_snapshot_guests, 50);
        assert!(Options::from_target(&cible(&[("max_snapshot_guests", "0")])).is_err());
        assert!(Options::from_target(&cible(&[("max_snapshot_guests", "abc")])).is_err());
    }

    #[test]
    fn lacceptation_dun_certificat_auto_signe_est_explicite() {
        for valeur in ["true", "1", "yes", "oui", "ON"] {
            let options = Options::from_target(&cible(&[("insecure_tls", valeur)])).unwrap();
            assert!(options.insecure_tls, "« {valeur} » aurait dû activer l'option");
        }
        for valeur in ["false", "0", "non"] {
            let options = Options::from_target(&cible(&[("insecure_tls", valeur)])).unwrap();
            assert!(!options.insecure_tls);
        }
        let error = Options::from_target(&cible(&[("insecure_tls", "peut-être")])).unwrap_err();
        assert!(matches!(error, ProbeError::Config(_)));
    }

    #[test]
    fn ladresse_accepte_les_formes_rencontrees_en_pratique() {
        let cas = [
            ("10.0.0.10", "https://10.0.0.10:8006"),
            ("pve.lan", "https://pve.lan:8006"),
            ("pve.lan:8006", "https://pve.lan:8006"),
            ("pve.lan:443", "https://pve.lan:443"),
            ("https://pve.example.net", "https://pve.example.net"),
            ("https://pve.example.net/", "https://pve.example.net"),
            ("http://127.0.0.1:8006", "http://127.0.0.1:8006"),
            ("fd00::1", "https://[fd00::1]:8006"),
            ("[fd00::1]:8006", "https://[fd00::1]:8006"),
            ("[fd00::1]", "https://[fd00::1]:8006"),
        ];
        for (saisie, attendu) in cas {
            assert_eq!(base_url(saisie, DEFAULT_PORT).unwrap(), attendu, "pour « {saisie} »");
        }
        assert!(base_url("   ", DEFAULT_PORT).is_err());
    }

    #[test]
    fn le_port_peut_etre_impose_par_etiquette() {
        let options = Options::from_target(&cible(&[("port", "8007")])).unwrap();
        assert_eq!(options.base_url, "https://10.0.0.10:8007");
        assert!(Options::from_target(&cible(&[("port", "abc")])).is_err());
    }

    #[test]
    fn le_filtre_de_noeuds_est_une_liste_separee_par_des_virgules() {
        let options = Options::from_target(&cible(&[("nodes", "pve1, pve3 ,")])).unwrap();
        assert_eq!(options.nodes, vec!["pve1", "pve3"]);
        assert!(options.wants_node("pve1"));
        assert!(!options.wants_node("pve2"));

        let toutes = Options::from_target(&cible(&[])).unwrap();
        assert!(toutes.wants_node("nimporte"));
    }

    #[test]
    fn les_bornes_des_delais_sont_verifiees() {
        assert!(Options::from_target(&cible(&[("request_timeout_seconds", "0")])).is_err());
        assert!(Options::from_target(&cible(&[("request_timeout_seconds", "999")])).is_err());
        assert_eq!(
            Options::from_target(&cible(&[("request_timeout_seconds", "5")]))
                .unwrap()
                .request_timeout,
            Duration::from_secs(5)
        );
        assert!(Options::from_target(&cible(&[("backup_lookback_days", "0")])).is_err());
    }
}
