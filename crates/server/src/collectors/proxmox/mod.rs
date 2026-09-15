//! Collecteur Proxmox VE.
//!
//! Interroge l'API REST d'un hyperviseur ou d'un cluster Proxmox VE et en tire
//! l'état du quorum, des nœuds, des machines virtuelles, des conteneurs, des
//! stockages, de la haute disponibilité, de la réplication, de Ceph, des
//! certificats, des mises à jour en attente et — surtout — des sauvegardes.
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
//! | `ha` | `true` | État de la haute disponibilité (`/cluster/ha/status/current`). |
//! | `backup_jobs` | `true` | Travaux de sauvegarde planifiés et invités non couverts. |
//! | `scan_snapshots` | `true` | Inventorie les instantanés, un appel par invité. |
//! | `max_snapshot_guests` | `200` | Plafond d'invités inventoriés par collecte ; au-delà, comptés dans `guest_snapshot_guests_skipped`. |
//! | `replication` | `true` | État des travaux de réplication. |
//! | `ceph` | `true` | Santé Ceph ; silencieux si Ceph n'est pas installé. |
//! | `updates` | `true` | Mises à jour en attente ; demande `Sys.Modify` sur `/nodes`, silencieux sinon. |
//! | `certificates` | `true` | Expiration des certificats de chaque nœud. |

mod auth;
mod backup;
mod ceph;
mod client;
mod ha;
mod metrics;
mod model;
mod options;
mod replication;
mod snapshots;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::future::Future;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use ezymonit_proto::{Collector, Credential, MetricKind, ProbeError, Sample, Target, TargetId};
use reqwest::StatusCode;
use tracing::{debug, warn};

use auth::{AuthMode, Ticket};
use backup::{Archive, GuestIndex, GuestRef, JobRun};
use client::PveClient;
use metrics::GuestKind;
use model::{
    AptPackage, BackupJob, CephStatus, CertificateInfo, ClusterStatusEntry, GuestEntry,
    HaStatusEntry, NodeListEntry, NodeStatus, NotBackedUp, ReplicationJob, Snapshot, StorageEntry,
    TaskEntry,
};
use options::Options;

use crate::collectors::http;

/// Identifiant de profil renvoyé par la découverte.
const PROFILE_ID: &str = "proxmox-ve";

/// Listes d'instantanés demandées simultanément à un même nœud.
///
/// Un appel par invité passe par le proxy du nœud, qui lit un fichier de
/// configuration à chaque fois : quatre en vol suffisent à masquer la latence
/// sans monopoliser `pveproxy`.
const SNAPSHOT_PARALLELISM: usize = 4;

#[derive(Default)]
pub struct ProxmoxCollector {
    /// Tickets en cache, un par cible. Sans ce cache, chaque interrogation ouvrirait
    /// une session sur l'hyperviseur, qui les journalise toutes.
    tickets: Mutex<HashMap<TargetId, Arc<tokio::sync::Mutex<Option<Ticket>>>>>,
}

impl ProxmoxCollector {
    pub fn new() -> Self {
        Self::default()
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
            http::client(options.insecure_tls)?,
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

        // Les appels à l'échelle du cluster et la tournée des nœuds sont
        // indépendants : tout part en même temps.
        let budget = SnapshotBudget::new(options.max_snapshot_guests);
        let (cluster, ha, jobs, not_backed_up, ceph_status, nodes) = futures::join!(
            pve.get::<Vec<ClusterStatusEntry>>("/cluster/status", &[]),
            when(options.ha, pve.get::<Vec<HaStatusEntry>>("/cluster/ha/status/current", &[])),
            when(options.backup_jobs, pve.get::<Vec<BackupJob>>("/cluster/backup", &[])),
            when(
                options.backup_jobs,
                pve.get::<Vec<NotBackedUp>>("/cluster/backup-info/not-backed-up", &[])
            ),
            when(options.ceph, pve.get::<CephStatus>("/cluster/ceph/status", &[])),
            collect_nodes(&pve, &options, &budget, now_s, ts_ms),
        );

        match cluster {
            Ok(entries) => samples.extend(metrics::cluster_samples(&entries, ts_ms)),
            Err(error) => {
                errors += 1;
                warn!(target_id = target.id, %error, "état du cluster Proxmox indisponible");
            }
        }

        match ha {
            Some(Ok(entries)) => samples.extend(ha::ha_samples(&entries, ts_ms)),
            Some(Err(error)) => {
                errors += 1;
                warn!(target_id = target.id, %error, "état de la haute disponibilité indisponible");
            }
            None => {}
        }

        // Ceph absent se manifeste par une erreur (500 « not initialized », 404) :
        // ce n'est ni une panne ni une faute, juste une fonctionnalité non installée.
        match ceph_status {
            Some(Ok(status)) => samples.extend(ceph::ceph_samples(&status, ts_ms)),
            Some(Err(error)) => debug!(target_id = target.id, %error, "pas de Ceph sur ce cluster"),
            None => {}
        }

        let aggregate = match nodes {
            Ok(aggregate) => aggregate,
            Err(error) => {
                errors += 1;
                warn!(target_id = target.id, %error, "liste des nœuds Proxmox indisponible");
                Aggregate::default()
            }
        };
        errors += aggregate.errors;
        samples.extend(aggregate.samples);
        samples.extend(backup::guest_backup_samples(
            &aggregate.guests,
            &aggregate.archives,
            &aggregate.task_backups,
            now_s,
            ts_ms,
        ));

        // Les travaux planifiés ont besoin de l'inventaire des invités pour
        // publier la couverture : ils passent donc après la tournée des nœuds.
        match (jobs, not_backed_up) {
            (Some(Ok(jobs)), not_backed_up) => {
                let not_backed_up = match not_backed_up {
                    Some(Ok(list)) => list,
                    Some(Err(error)) => {
                        errors += 1;
                        warn!(target_id = target.id, %error, "liste des invités non sauvegardés indisponible");
                        Vec::new()
                    }
                    None => Vec::new(),
                };
                samples.extend(backup::cluster_job_samples(
                    &jobs,
                    &not_backed_up,
                    &aggregate.guests,
                    &aggregate.job_runs,
                    now_s,
                    ts_ms,
                ));
            }
            (Some(Err(error)), _) => {
                errors += 1;
                warn!(target_id = target.id, %error, "travaux de sauvegarde planifiés indisponibles");
            }
            (None, _) => {}
        }

        if aggregate.replication_supported {
            samples.push(Sample::new(
                "proxmox_replication_jobs_total",
                aggregate.replication_jobs.len() as f64,
                MetricKind::Gauge,
                ts_ms,
            ));
        }
        if options.scan_snapshots {
            samples.push(Sample::new(
                "proxmox_guest_snapshot_guests_skipped",
                f64::from(aggregate.snapshot_skipped),
                MetricKind::Gauge,
                ts_ms,
            ));
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
            http::client(options.insecure_tls)?,
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

/// N'exécute l'appel que si l'option est active ; `None` sinon.
///
/// Permet de placer un appel facultatif dans un `join!` sans dupliquer la
/// branche « option coupée » à chaque endroit.
async fn when<T, F>(enabled: bool, call: F) -> Option<Result<T, ProbeError>>
where
    F: Future<Output = Result<T, ProbeError>>,
{
    if enabled { Some(call.await) } else { None }
}

/// Plafond d'invités dont on liste les instantanés, partagé entre les nœuds.
///
/// Les nœuds sont collectés en parallèle : un compteur atomique est la seule
/// façon de tenir un plafond global sans sérialiser la tournée.
struct SnapshotBudget(AtomicU32);

impl SnapshotBudget {
    fn new(max_guests: u32) -> Self {
        Self(AtomicU32::new(max_guests))
    }

    /// Réserve une place ; `false` quand le plafond est atteint.
    fn claim(&self) -> bool {
        self.0.fetch_update(Ordering::AcqRel, Ordering::Acquire, |left| left.checked_sub(1)).is_ok()
    }
}

/// Liste les nœuds puis les collecte en parallèle.
async fn collect_nodes(
    pve: &PveClient,
    options: &Options,
    budget: &SnapshotBudget,
    now_s: i64,
    ts_ms: i64,
) -> Result<Aggregate, ProbeError> {
    let nodes = pve.get::<Vec<NodeListEntry>>("/nodes", &[]).await?;
    let outcomes = futures::future::join_all(
        nodes
            .iter()
            .filter(|node| options.wants_node(&node.node))
            .map(|node| collect_node(pve, options, node, budget, now_s, ts_ms)),
    )
    .await;
    Ok(merge(outcomes))
}

/// Ce qu'un nœud a livré, avant fusion à l'échelle du cluster.
#[derive(Default)]
struct NodeOutcome {
    samples: Vec<Sample>,
    guests: GuestIndex,
    archives: Vec<Archive>,
    task_backups: BTreeMap<i64, i64>,
    /// Dernière exécution des travaux planifiés vue depuis ce nœud.
    job_runs: BTreeMap<String, JobRun>,
    /// Séries de réplication, fusionnées à part pour dédoublonner par travail.
    replication: Vec<Sample>,
    /// Vrai si le nœud a répondu à `/replication`, même par une liste vide.
    replication_supported: bool,
    /// Invités dont les instantanés n'ont pas été listés, plafond atteint.
    snapshot_skipped: u32,
    errors: u32,
}

#[derive(Default)]
struct Aggregate {
    samples: Vec<Sample>,
    guests: GuestIndex,
    archives: BTreeMap<i64, Vec<Archive>>,
    task_backups: BTreeMap<i64, i64>,
    job_runs: BTreeMap<String, JobRun>,
    replication_jobs: BTreeSet<String>,
    replication_supported: bool,
    snapshot_skipped: u32,
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

        for (job, run) in outcome.job_runs {
            aggregate
                .job_runs
                .entry(job)
                .and_modify(|current| *current = current.latest(run))
                .or_insert(run);
        }

        // Un travail de réplication peut être listé par sa source et par sa
        // destination : la première occurrence l'emporte.
        aggregate.replication_supported |= outcome.replication_supported;
        let mut seen_here = BTreeSet::new();
        for sample in outcome.replication {
            let job = sample.labels.get("job").cloned().unwrap_or_default();
            if aggregate.replication_jobs.contains(&job) && !seen_here.contains(&job) {
                continue;
            }
            seen_here.insert(job);
            aggregate.samples.push(sample);
        }
        aggregate.replication_jobs.extend(seen_here);
        aggregate.snapshot_skipped += outcome.snapshot_skipped;
    }

    aggregate
}

/// Interroge un nœud. Ne renvoie jamais d'erreur : un nœud en panne se traduit par
/// `proxmox_node_up = 0` et un compteur d'erreurs, jamais par l'abandon du cluster.
async fn collect_node(
    pve: &PveClient,
    options: &Options,
    node: &NodeListEntry,
    budget: &SnapshotBudget,
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
    let replication_path = format!("/nodes/{name}/replication");
    let updates_path = format!("/nodes/{name}/apt/update");
    let certificates_path = format!("/nodes/{name}/certificates/info");

    // Les inventaires d'un nœud sont indépendants : les enchaîner multiplierait
    // d'autant le temps passé sur un nœud lent.
    let (qemu, lxc, storages, tasks, replication, updates, certificates) = futures::join!(
        pve.get::<Vec<GuestEntry>>(&qemu_path, &[]),
        pve.get::<Vec<GuestEntry>>(&lxc_path, &[]),
        pve.get::<Vec<StorageEntry>>(&storage_path, &[]),
        pve.get::<Vec<TaskEntry>>(&tasks_path, &tasks_query),
        // Une machine isolée sans réplication configurée répond 404 ou 501.
        when(
            options.replication,
            pve.get_unless::<Vec<ReplicationJob>>(
                &replication_path,
                &[],
                &[StatusCode::NOT_FOUND, StatusCode::NOT_IMPLEMENTED],
            )
        ),
        // `PVEAuditor` ne couvre pas `Sys.Modify`, exigé par `apt/update` : le 403
        // est un droit facultatif non accordé, pas une erreur.
        when(
            options.updates,
            pve.get_unless::<Vec<AptPackage>>(&updates_path, &[], &[StatusCode::FORBIDDEN])
        ),
        when(
            options.certificates,
            pve.get_unless::<Vec<CertificateInfo>>(
                &certificates_path,
                &[],
                &[StatusCode::FORBIDDEN],
            )
        ),
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
            outcome.job_runs =
                backup::task_runs_by_job(&tasks, now_s, options.backup_lookback_seconds);
        }
        Err(error) => {
            outcome.errors += 1;
            warn!(node = name, %error, "historique des tâches vzdump indisponible");
        }
    }

    match replication {
        Some(Ok(Some(jobs))) => {
            outcome.replication_supported = true;
            outcome.replication = replication::replication_samples(name, &jobs, now_s, ts_ms);
        }
        Some(Ok(None)) => debug!(node = name, "pas de réplication sur ce nœud"),
        Some(Err(error)) => {
            outcome.errors += 1;
            warn!(node = name, %error, "état de la réplication indisponible");
        }
        None => {}
    }

    match updates {
        Some(Ok(Some(packages))) => {
            outcome.samples.extend(metrics::updates_samples(name, &packages, ts_ms));
        }
        Some(Ok(None)) => debug!(
            node = name,
            "mises à jour non listées : Sys.Modify manquant sur /nodes (droit facultatif)"
        ),
        Some(Err(error)) => {
            outcome.errors += 1;
            warn!(node = name, %error, "liste des mises à jour indisponible");
        }
        None => {}
    }

    match certificates {
        Some(Ok(Some(certs))) => {
            outcome.samples.extend(metrics::certificate_samples(name, &certs, now_s, ts_ms));
        }
        Some(Ok(None)) => debug!(node = name, "certificats non listés : droit manquant"),
        Some(Err(error)) => {
            outcome.errors += 1;
            warn!(node = name, %error, "informations de certificat indisponibles");
        }
        None => {}
    }

    if options.scan_snapshots {
        collect_snapshots(pve, name, budget, &mut outcome, now_s, ts_ms).await;
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

/// Liste les instantanés des invités du nœud, dans la limite du plafond global
/// et de `SNAPSHOT_PARALLELISM` appels en vol.
async fn collect_snapshots(
    pve: &PveClient,
    node: &str,
    budget: &SnapshotBudget,
    outcome: &mut NodeOutcome,
    now_s: i64,
    ts_ms: i64,
) {
    let mut planned: Vec<(i64, GuestRef)> = Vec::new();
    for (vmid, guest) in &outcome.guests {
        if budget.claim() {
            planned.push((*vmid, guest.clone()));
        } else {
            outcome.snapshot_skipped += 1;
        }
    }

    let semaphore = tokio::sync::Semaphore::new(SNAPSHOT_PARALLELISM);
    let listings = futures::future::join_all(planned.iter().map(|(vmid, guest)| {
        let semaphore = &semaphore;
        async move {
            // Le sémaphore n'est jamais fermé : un refus de permis est théorique,
            // et l'appel part alors sans attendre plutôt que d'être perdu.
            let _permit = semaphore.acquire().await.ok();
            let path = format!("/nodes/{node}/{}/{vmid}/snapshot", guest.kind.as_str());
            (*vmid, guest, pve.get::<Vec<Snapshot>>(&path, &[]).await)
        }
    }))
    .await;

    for (vmid, guest, result) in listings {
        match result {
            Ok(list) => outcome
                .samples
                .extend(snapshots::snapshot_samples(vmid, guest, &list, now_s, ts_ms)),
            Err(error) => {
                outcome.errors += 1;
                warn!(node, vmid, %error, "liste des instantanés indisponible");
            }
        }
    }
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
            ..Default::default()
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

    #[test]
    fn le_plafond_dinstantanes_est_partage_et_ne_descend_pas_sous_zero() {
        let budget = SnapshotBudget::new(2);
        assert!(budget.claim());
        assert!(budget.claim());
        assert!(!budget.claim(), "le plafond est atteint");
        assert!(!budget.claim(), "et le reste");
        assert_eq!(budget.0.load(Ordering::Acquire), 0);
    }

    #[test]
    fn un_travail_de_replication_vu_par_deux_noeuds_nest_publie_quune_fois() {
        let serie = |node: &str| {
            Sample::new("proxmox_replication_job_error", 0.0, MetricKind::Gauge, 1000)
                .with_label("job", "102-0")
                .with_label("node", node)
        };
        let source = NodeOutcome {
            replication: vec![serie("pve2")],
            replication_supported: true,
            ..Default::default()
        };
        let destination = NodeOutcome {
            replication: vec![serie("pve2")],
            replication_supported: true,
            ..Default::default()
        };
        let sans = NodeOutcome::default();

        let aggregate = merge(vec![source, destination, sans]);
        assert_eq!(aggregate.samples.len(), 1);
        assert_eq!(aggregate.replication_jobs.len(), 1);
        assert!(aggregate.replication_supported);
    }

    #[test]
    fn la_derniere_execution_dun_travail_planifie_est_fusionnee_entre_noeuds() {
        let a = NodeOutcome {
            job_runs: BTreeMap::from([("backup-1".to_string(), JobRun { start: 100, ok: true })]),
            snapshot_skipped: 2,
            ..Default::default()
        };
        let b = NodeOutcome {
            job_runs: BTreeMap::from([("backup-1".to_string(), JobRun { start: 200, ok: false })]),
            snapshot_skipped: 1,
            ..Default::default()
        };
        let aggregate = merge(vec![a, b]);
        assert_eq!(aggregate.job_runs["backup-1"], JobRun { start: 200, ok: false });
        assert_eq!(aggregate.snapshot_skipped, 3);
    }

    #[tokio::test]
    async fn un_appel_facultatif_coupe_ne_part_pas() {
        let appele = std::cell::Cell::new(false);
        let call = async {
            appele.set(true);
            Ok::<u8, ProbeError>(1)
        };
        assert!(when(false, call).await.is_none());
        assert!(!appele.get(), "l'option coupée ne doit rien exécuter");
        assert_eq!(when(true, async { Ok::<u8, ProbeError>(1) }).await.unwrap().unwrap(), 1);
    }
}
