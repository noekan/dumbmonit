//! Collecteur Proxmox VE.
//!
//! Interroge l'API REST d'un hyperviseur ou d'un cluster Proxmox VE et en tire
//! l'état du quorum, des nœuds, des machines virtuelles, des conteneurs, des
//! stockages et — surtout — des sauvegardes.
//!
//! # Principes
//!
//! * **Une panne partielle reste une collecte réussie.** Sur un cluster de trois
//!   nœuds, un nœud éteint produit `proxmox_node_up{node="…"} = 0` et les deux
//!   autres livrent leurs métriques. Seul un échec sur `/version` — l'API elle-même
//!   ne répond pas, ou refuse l'authentification — fait échouer l'interrogation.
//! * **Les erreurs sont classées pour l'alerting.** Un jeton invalide donne
//!   `ProbeError::Auth`, qui s'affiche dans l'interface sans déclencher
//!   « équipement hors ligne » ; seul un vrai défaut de joignabilité donne
//!   `Unreachable` ou `Timeout`.
//! * **Aucun secret ne sort d'ici.** Ni jeton, ni ticket, ni mot de passe
//!   n'apparaît dans un journal, un message d'erreur ou une sortie `Debug`.
//!
//! # Réglages, portés par les étiquettes de la cible
//!
//! | Étiquette | Défaut | Rôle |
//! |---|---|---|
//! | `insecure_tls` | `false` | Accepte un certificat non vérifiable (auto-signé). |
//! | `port` | `8006` | Port de l'API, si l'adresse n'en précise pas. |
//! | `request_timeout_seconds` | `10` | Délai par requête HTTP. |
//! | `backup_lookback_days` | `31` | Profondeur d'examen des tâches `vzdump`. |
//! | `scan_backup_storage` | `true` | Inventorie les archives pour dater les sauvegardes par machine. |
//! | `nodes` | tous | Restreint la collecte à une liste de nœuds. |

mod auth;
mod backup;
mod client;
mod metrics;
mod model;
mod options;

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use ezymonit_proto::{Collector, Credential, MetricKind, ProbeError, Sample, Target, TargetId};
use tracing::{debug, warn};

use auth::{AuthMode, Ticket};
use backup::{Archive, GuestIndex, GuestRef};
use client::PveClient;
use metrics::GuestKind;
use model::{ClusterStatusEntry, GuestEntry, NodeListEntry, NodeStatus, StorageEntry, TaskEntry};
use options::Options;

/// Identifiant de profil renvoyé par la découverte.
const PROFILE_ID: &str = "proxmox-ve";

/// Délai d'établissement de la connexion TCP + TLS.
///
/// Il est fixé une fois pour toutes au niveau du client mutualisé : un nœud éteint
/// se manifeste par un `SYN` sans réponse, et cinq secondes suffisent à le
/// constater sans faire attendre les autres nœuds du cluster.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Default)]
pub struct ProxmoxCollector {
    /// Deux clients seulement, construits à la demande : `reqwest` mutualise le
    /// pool de connexions, et la politique TLS ne peut pas être changée par requête.
    http_verified: OnceLock<reqwest::Client>,
    http_unverified: OnceLock<reqwest::Client>,
    /// Tickets en cache, un par cible. Sans ce cache, chaque interrogation ouvrirait
    /// une session sur l'hyperviseur, qui les journalise toutes.
    tickets: Mutex<HashMap<TargetId, Arc<tokio::sync::Mutex<Option<Ticket>>>>>,
}

impl ProxmoxCollector {
    pub fn new() -> Self {
        Self::default()
    }

    fn http(&self, insecure_tls: bool) -> Result<reqwest::Client, ProbeError> {
        let cell = if insecure_tls { &self.http_unverified } else { &self.http_verified };
        if let Some(existing) = cell.get() {
            return Ok(existing.clone());
        }
        let built = client::build_http_client(insecure_tls, CONNECT_TIMEOUT)?;
        // Une course entre deux cibles construirait deux clients ; le perdant est
        // simplement jeté, ce qui est sans conséquence.
        let _ = cell.set(built.clone());
        Ok(built)
    }

    fn auth_mode(&self, target: &Target) -> Result<AuthMode, ProbeError> {
        match &target.credential {
            Credential::ApiToken { token } => Ok(AuthMode::Token(auth::token_header_value(token)?)),
            Credential::UsernamePassword { username, password } => Ok(AuthMode::Ticket {
                username: username.clone(),
                password: password.clone(),
                cached: self.ticket_slot(target.id),
            }),
            other => Err(ProbeError::Config(format!(
                "Proxmox VE expects an API token or a username / password pair, \
                 configured credential: {other}"
            ))),
        }
    }

    fn ticket_slot(&self, id: TargetId) -> Arc<tokio::sync::Mutex<Option<Ticket>>> {
        let mut cache = self.tickets.lock().unwrap_or_else(|poison| poison.into_inner());
        cache.entry(id).or_default().clone()
    }
}

#[async_trait]
impl Collector for ProxmoxCollector {
    fn kind(&self) -> &'static str {
        "proxmox"
    }

    async fn probe(&self, target: &Target) -> Result<Vec<Sample>, ProbeError> {
        let options = Options::from_target(target)?;
        let pve = PveClient::new(
            self.http(options.insecure_tls)?,
            options.base_url.clone(),
            self.auth_mode(target)?,
            options.request_timeout,
        );

        let started = std::time::Instant::now();
        let now = chrono::Utc::now();
        let (now_s, ts_ms) = (now.timestamp(), now.timestamp_millis());

        // `/version` sert de sonde de vie et d'authentification : c'est le seul
        // appel dont l'échec condamne l'interrogation entière.
        let version: model::Version = pve.get("/version", &[]).await?;
        let mut samples = metrics::version_samples(&version, ts_ms);
        samples.push(Sample::new("proxmox_up", 1.0, MetricKind::Gauge, ts_ms));
        let mut errors = 0u32;

        match pve.get::<Vec<ClusterStatusEntry>>("/cluster/status", &[]).await {
            Ok(entries) => samples.extend(metrics::cluster_samples(&entries, ts_ms)),
            Err(error) => {
                errors += 1;
                warn!(target_id = target.id, %error, "état du cluster Proxmox indisponible");
            }
        }

        match pve.get::<Vec<NodeListEntry>>("/nodes", &[]).await {
            Ok(nodes) => {
                let outcomes = futures::future::join_all(
                    nodes
                        .iter()
                        .filter(|node| options.wants_node(&node.node))
                        .map(|node| collect_node(&pve, &options, node, now_s, ts_ms)),
                )
                .await;

                let aggregate = merge(outcomes);
                errors += aggregate.errors;
                samples.extend(aggregate.samples);
                samples.extend(backup::guest_backup_samples(
                    &aggregate.guests,
                    &aggregate.archives,
                    &aggregate.task_backups,
                    now_s,
                    ts_ms,
                ));
            }
            Err(error) => {
                errors += 1;
                warn!(target_id = target.id, %error, "liste des nœuds Proxmox indisponible");
            }
        }

        samples.push(Sample::new(
            "proxmox_scrape_errors",
            f64::from(errors),
            MetricKind::Gauge,
            ts_ms,
        ));
        samples.push(Sample::new(
            "proxmox_scrape_duration_seconds",
            started.elapsed().as_secs_f64(),
            MetricKind::Gauge,
            ts_ms,
        ));
        Ok(samples)
    }

    async fn discover(&self, target: &Target) -> Result<Option<String>, ProbeError> {
        let options = Options::from_target(target)?;
        let pve = PveClient::new(
            self.http(options.insecure_tls)?,
            options.base_url.clone(),
            self.auth_mode(target)?,
            options.request_timeout,
        );

        let version: model::Version = pve.get("/version", &[]).await?;
        let nodes = pve.get::<Vec<NodeListEntry>>("/nodes", &[]).await.unwrap_or_default();

        debug!(
            target_id = target.id,
            version = version.version.as_deref().unwrap_or("inconnue"),
            nodes = nodes.len(),
            "Proxmox VE détecté"
        );
        Ok(Some(PROFILE_ID.to_string()))
    }
}

/// Ce qu'un nœud a livré, avant fusion à l'échelle du cluster.
#[derive(Default)]
struct NodeOutcome {
    samples: Vec<Sample>,
    guests: GuestIndex,
    archives: Vec<Archive>,
    task_backups: BTreeMap<i64, i64>,
    errors: u32,
}

#[derive(Default)]
struct Aggregate {
    samples: Vec<Sample>,
    guests: GuestIndex,
    archives: BTreeMap<i64, Vec<Archive>>,
    task_backups: BTreeMap<i64, i64>,
    errors: u32,
}

/// Rassemble les résultats des nœuds.
///
/// Un stockage partagé est visible depuis chaque nœud du cluster : sans
/// déduplication, une même archive serait comptée autant de fois qu'il y a de
/// nœuds et `proxmox_backup_count` deviendrait faux.
fn merge(outcomes: Vec<NodeOutcome>) -> Aggregate {
    let mut aggregate = Aggregate::default();

    for outcome in outcomes {
        aggregate.errors += outcome.errors;
        aggregate.samples.extend(outcome.samples);
        aggregate.guests.extend(outcome.guests);

        for archive in outcome.archives {
            let connues = aggregate.archives.entry(archive.vmid).or_default();
            if !connues.iter().any(|autre| autre.ctime == archive.ctime) {
                connues.push(archive);
            }
        }

        for (vmid, date) in outcome.task_backups {
            aggregate
                .task_backups
                .entry(vmid)
                .and_modify(|current| *current = (*current).max(date))
                .or_insert(date);
        }
    }

    aggregate
}

/// Interroge un nœud. Ne renvoie jamais d'erreur : un nœud en panne se traduit par
/// `proxmox_node_up = 0` et un compteur d'erreurs, jamais par l'abandon du cluster.
async fn collect_node(
    pve: &PveClient,
    options: &Options,
    node: &NodeListEntry,
    now_s: i64,
    ts_ms: i64,
) -> NodeOutcome {
    let mut outcome = NodeOutcome::default();
    let name = node.node.as_str();

    // Un nœud que le cluster annonce déjà hors ligne n'a pas besoin d'être
    // interrogé : l'appel passerait par le proxy et attendrait le délai complet.
    if !node.is_online() {
        outcome.samples.push(metrics::node_up_sample(name, false, ts_ms));
        return outcome;
    }

    match pve.get::<NodeStatus>(&format!("/nodes/{name}/status"), &[]).await {
        Ok(status) => {
            outcome.samples.push(metrics::node_up_sample(name, true, ts_ms));
            outcome.samples.extend(metrics::node_samples(name, &status, ts_ms));
        }
        Err(error) => {
            // Une erreur de droits ou de protocole prouve que le nœud a répondu :
            // seule une indisponibilité réelle doit alimenter « hors ligne ».
            outcome.samples.push(metrics::node_up_sample(name, !error.means_down(), ts_ms));
            outcome.errors += 1;
            warn!(node = name, %error, "nœud Proxmox non collecté");
            return outcome;
        }
    }

    let (qemu_path, lxc_path) = (format!("/nodes/{name}/qemu"), format!("/nodes/{name}/lxc"));
    let (storage_path, tasks_path) =
        (format!("/nodes/{name}/storage"), format!("/nodes/{name}/tasks"));
    let tasks_query =
        [("typefilter", "vzdump".to_string()), ("limit", options.task_limit.to_string())];

    // Les quatre inventaires d'un nœud sont indépendants : les enchaîner
    // multiplierait par quatre le temps passé sur un nœud lent.
    let (qemu, lxc, storages, tasks) = futures::join!(
        pve.get::<Vec<GuestEntry>>(&qemu_path, &[]),
        pve.get::<Vec<GuestEntry>>(&lxc_path, &[]),
        pve.get::<Vec<StorageEntry>>(&storage_path, &[]),
        pve.get::<Vec<TaskEntry>>(&tasks_path, &tasks_query),
    );

    for (kind, result) in [(GuestKind::Qemu, qemu), (GuestKind::Lxc, lxc)] {
        match result {
            Ok(guests) => {
                for guest in guests.iter().filter(|guest| !guest.is_template()) {
                    outcome.guests.insert(
                        guest.vmid(),
                        GuestRef { node: name.to_string(), name: guest.display_name(), kind },
                    );
                }
                outcome.samples.extend(metrics::guest_samples(name, kind, &guests, ts_ms));
            }
            Err(error) => {
                outcome.errors += 1;
                warn!(node = name, kind = kind.as_str(), %error, "inventaire des invités échoué");
            }
        }
    }

    let mut backup_storages = Vec::new();
    match storages {
        Ok(storages) => {
            backup_storages = storages
                .iter()
                .filter(|storage| storage.is_active() && storage.holds_backups())
                .map(|storage| storage.storage.clone())
                .collect();
            outcome.samples.extend(metrics::storage_samples(name, &storages, ts_ms));
        }
        Err(error) => {
            outcome.errors += 1;
            warn!(node = name, %error, "inventaire des stockages échoué");
        }
    }

    match tasks {
        Ok(tasks) => {
            outcome.samples.extend(backup::job_samples(
                name,
                &tasks,
                now_s,
                options.backup_lookback_seconds,
                ts_ms,
            ));
            outcome.task_backups =
                backup::task_backups_by_vmid(&tasks, now_s, options.backup_lookback_seconds);
        }
        Err(error) => {
            outcome.errors += 1;
            warn!(node = name, %error, "historique des tâches vzdump indisponible");
        }
    }

    if options.scan_backup_storage {
        let listings =
            futures::future::join_all(backup_storages.iter().map(|storage| async move {
                let path = format!("/nodes/{name}/storage/{storage}/content");
                let result = pve
                    .get::<Vec<model::BackupVolume>>(&path, &[("content", "backup".to_string())])
                    .await;
                (storage.as_str(), result)
            }))
            .await;

        for (storage, result) in listings {
            match result {
                Ok(volumes) => outcome.archives.extend(backup::archives_from_content(&volumes)),
                Err(error) => {
                    outcome.errors += 1;
                    warn!(node = name, storage, %error, "listing des sauvegardes échoué");
                }
            }
        }
    }

    outcome
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::time::Duration;

    use ezymonit_proto::Credential;

    use super::*;

    fn cible(credential: Credential) -> Target {
        Target {
            id: 7,
            name: "pve".into(),
            address: "10.0.0.10".into(),
            kind: "proxmox".into(),
            profile_id: None,
            parent_id: None,
            interval: Duration::from_secs(60),
            enabled: true,
            tags: BTreeMap::new(),
            credential,
        }
    }

    #[test]
    fn le_collecteur_annonce_son_type() {
        assert_eq!(ProxmoxCollector::new().kind(), "proxmox");
    }

    #[test]
    fn le_jeton_dapi_produit_une_session_sans_ticket() {
        let collector = ProxmoxCollector::new();
        let credential =
            Credential::ApiToken { token: "monitoring@pve!ezymonit=8f3a1c9e-dead-beef".into() };
        let mode = collector.auth_mode(&cible(credential)).unwrap();
        assert!(matches!(mode, AuthMode::Token(_)));
    }

    #[test]
    fn un_couple_identifiants_produit_une_session_a_ticket() {
        let collector = ProxmoxCollector::new();
        let credential = Credential::UsernamePassword {
            username: "monitoring@pve".into(),
            password: "secret".into(),
        };
        let mode = collector.auth_mode(&cible(credential)).unwrap();
        assert!(matches!(mode, AuthMode::Ticket { .. }));
    }

    #[test]
    fn le_cache_de_ticket_est_partage_entre_deux_interrogations_de_la_meme_cible() {
        let collector = ProxmoxCollector::new();
        let premier = collector.ticket_slot(7);
        let second = collector.ticket_slot(7);
        assert!(Arc::ptr_eq(&premier, &second), "le ticket doit survivre à l'interrogation");
        assert!(!Arc::ptr_eq(&premier, &collector.ticket_slot(8)), "une cible, un ticket");
    }

    #[test]
    fn un_identifiant_inadapte_est_refuse_avant_tout_appel_reseau() {
        let collector = ProxmoxCollector::new();
        for credential in
            [Credential::None, Credential::SnmpCommunity { community: "public".into() }]
        {
            let error = collector.auth_mode(&cible(credential)).unwrap_err();
            assert!(matches!(error, ProbeError::Config(_)));
        }
    }

    #[test]
    fn le_message_didentifiant_inadapte_ne_divulgue_pas_le_secret() {
        let collector = ProxmoxCollector::new();
        let credential = Credential::SnmpCommunity { community: "SECRET-COMMUNITY".into() };
        let error = collector.auth_mode(&cible(credential)).unwrap_err();
        assert!(!format!("{error}").contains("SECRET-COMMUNITY"));
    }

    fn archive(vmid: i64, ctime: i64) -> Archive {
        Archive { vmid, ctime, size: 1024.0 }
    }

    #[test]
    fn un_noeud_en_echec_nempeche_pas_les_autres_de_livrer_leurs_metriques() {
        let sain = NodeOutcome {
            samples: vec![metrics::node_up_sample("pve1", true, 1000)],
            guests: BTreeMap::from([(
                100,
                GuestRef { node: "pve1".into(), name: "web".into(), kind: GuestKind::Qemu },
            )]),
            archives: vec![archive(100, 1_724_000_000)],
            task_backups: BTreeMap::from([(100, 1_724_000_000)]),
            errors: 0,
        };
        let injoignable = NodeOutcome {
            samples: vec![metrics::node_up_sample("pve3", false, 1000)],
            errors: 1,
            ..Default::default()
        };

        let aggregate = merge(vec![sain, injoignable]);

        assert_eq!(aggregate.errors, 1, "l'échec est remonté sans masquer le reste");
        assert_eq!(aggregate.guests.len(), 1);
        assert_eq!(aggregate.samples.len(), 2);
        assert!(
            aggregate
                .samples
                .iter()
                .any(|s| s.series_key() == r#"proxmox_node_up{node="pve3"}"# && s.value == 0.0)
        );
        assert!(
            aggregate
                .samples
                .iter()
                .any(|s| s.series_key() == r#"proxmox_node_up{node="pve1"}"# && s.value == 1.0)
        );
    }

    #[test]
    fn une_archive_sur_stockage_partage_nest_comptee_quune_fois() {
        let depuis_pve1 =
            NodeOutcome { archives: vec![archive(100, 1_724_000_000)], ..Default::default() };
        let depuis_pve2 = NodeOutcome {
            archives: vec![archive(100, 1_724_000_000), archive(100, 1_723_400_000)],
            ..Default::default()
        };

        let aggregate = merge(vec![depuis_pve1, depuis_pve2]);
        assert_eq!(aggregate.archives[&100].len(), 2, "deux archives distinctes, pas trois");
    }

    #[test]
    fn la_sauvegarde_la_plus_recente_lemporte_entre_noeuds() {
        let a = NodeOutcome { task_backups: BTreeMap::from([(100, 1_000)]), ..Default::default() };
        let b = NodeOutcome { task_backups: BTreeMap::from([(100, 2_000)]), ..Default::default() };
        assert_eq!(merge(vec![a, b]).task_backups[&100], 2_000);
    }
}
