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
//! | `guest_agent` | `true` | Détail des VM en marche (ballon, agent) et leurs systèmes de fichiers via l'agent QEMU ; demande `VM.Monitor`, silencieux sinon. |
//! | `disks` | `true` | Disques physiques : santé SMART, usure, température. |
//! | `zfs` | `true` | État et capacité des pools ZFS. |
//! | `packages` | `true` | Versions des paquets Proxmox, pour signaler un changement entre deux interrogations. |
//! | `subscription` | `true` | Abonnement et dépôts APT du nœud. |
//! | `cluster_resources` | `true` | Inventaire du cluster en un appel ; voit les invités d'un nœud injoignable. |
//! | `services` | `true` | Démons du nœud (`pvestatd`, `pveproxy`, `corosync`…) et sa version. |
//! | `network` | `true` | Ponts, agrégats et VLAN du nœud, et les compteurs par carte d'invité. |
//! | `lvm` | `true` | Groupes de volumes, pools à provisionnement fin, montages gérés. |
//! | `ceph_detail` | `true` | OSD, pools, CephFS, drapeaux et sourdines de santé. |
//! | `backup_volumes` | `true` | Volumes qu'un travail de sauvegarde couvre réellement. |
//! | `guest_os` | `true` | Système et adresses de chaque invité, rafraîchis une fois par heure. |
//! | `metrics_export` | `false` | Flux RRD complet du cluster (`/cluster/metrics/export`), pression système comprise. |

mod apt;
mod auth;
mod backup;
mod ceph;
mod client;
mod disks;
mod export;
mod guest;
mod ha;
mod metrics;
mod model;
mod node;
mod options;
mod replication;
mod resources;
mod snapshots;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::future::Future;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use dumbmonit_proto::{Collector, Credential, MetricKind, ProbeError, Sample, Target, TargetId};
use reqwest::StatusCode;
use tracing::{debug, warn};

use auth::{AuthMode, Ticket};
use backup::{Archive, GuestIndex, GuestRef, JobRun};
use client::PveClient;
use metrics::GuestKind;
use model::{
    AgentFsInfo, AgentInterfaces, AgentOsInfo, AptPackage, AptVersion, BackupJob, CephFlag, CephFs,
    CephHealthMute, CephOsdTree, CephPool, CephStatus, CertificateInfo, ClusterStatusEntry,
    DirectoryMount, DiskEntry, GuestEntry, HaManagerStatus, HaStatusEntry, IncludedVolumes,
    LvmTree, LxcInterface, MetricsExport, NetstatEntry, NetworkInterface, NodeListEntry,
    NodeStatus, NotBackedUp, Num, QemuStatus, ReplicationJob, Repositories, ResourceEntry,
    ServiceEntry, SmartReport, Snapshot, StorageEntry, Subscription, TaskEntry, ThinPool, ZfsPool,
};
use options::Options;
use resources::GuestSummary;

use crate::http;

/// Identifiant de profil renvoyé par la découverte.
const PROFILE_ID: &str = "proxmox-ve";

/// Listes d'instantanés demandées simultanément à un même nœud.
///
/// Un appel par invité passe par le proxy du nœud, qui lit un fichier de
/// configuration à chaque fois : quatre en vol suffisent à masquer la latence
/// sans monopoliser `pveproxy`.
const SNAPSHOT_PARALLELISM: usize = 4;

/// Versions de paquets retenues d'une interrogation à l'autre, par nœud.
type PackageMemories = BTreeMap<String, apt::PackageMemory>;

/// Durée de validité des faits d'un invité — système et adresses.
///
/// Un système d'exploitation ne change pas entre deux minutes, une adresse
/// presque jamais. Les redemander à chaque interrogation coûterait deux appels
/// à l'agent par machine en marche, soit plus que tout le reste de la collecte
/// sur un parc de cinquante machines. On les garde une heure et on les republie
/// à chaque tour, pour que la série reste continue.
const GUEST_FACTS_TTL_SECONDS: i64 = 3_600;

/// Faits d'un invité, avec la date à laquelle ils ont été demandés.
struct GuestFact {
    fetched_at: i64,
    samples: Vec<Sample>,
}

/// Faits de tous les invités d'une cible, par VMID.
type GuestFacts = HashMap<i64, GuestFact>;

#[derive(Default)]
pub struct ProxmoxCollector {
    /// Tickets en cache, un par cible. Sans ce cache, chaque interrogation ouvrirait
    /// une session sur l'hyperviseur, qui les journalise toutes.
    tickets: Mutex<HashMap<TargetId, Arc<tokio::sync::Mutex<Option<Ticket>>>>>,
    /// Versions des paquets vues à la dernière interrogation, par cible puis par
    /// nœud : c'est la référence qui permet de dire « ce nœud a été mis à jour ».
    packages: Mutex<HashMap<TargetId, PackageMemories>>,
    /// Système et adresses de chaque invité, rafraîchis une fois par heure.
    facts: Mutex<HashMap<TargetId, GuestFacts>>,
    /// Horodatage du dernier point lu dans le flux RRD, par cible : c'est lui qui
    /// dit à Proxmox où reprendre, et qui évite de republier deux fois le même
    /// point.
    export_cursor: Mutex<HashMap<TargetId, i64>>,
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

    /// Lit le flux RRD du cluster et le traduit en échantillons.
    ///
    /// La reprise se fait à l'horodatage du dernier point vu : Proxmox renvoie
    /// alors uniquement ce qui s'est passé depuis, à la minute. Une cible
    /// interrogée toutes les cinq minutes récupère ainsi les cinq points de
    /// l'intervalle, au lieu de n'en garder qu'un.
    ///
    /// Tout échoue en silence : l'endpoint demande `Sys.Audit` sur `/`, et
    /// n'existe pas avant PVE 7.
    async fn collect_rrd_export(
        &self,
        pve: &PveClient,
        options: &Options,
        id: TargetId,
        now_s: i64,
    ) -> Vec<Sample> {
        if !options.metrics_export {
            return Vec::new();
        }

        let since = self
            .export_cursor
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .get(&id)
            .copied();
        let start = since.unwrap_or(now_s - export::INITIAL_HISTORY_SECONDS);
        let query = [("history", "1".to_string()), ("start-time", start.max(0).to_string())];

        let response =
            pve.get_unless::<MetricsExport>("/cluster/metrics/export", &query, &OPTIONAL_STATUSES);
        let stream = match response.await {
            Ok(Some(stream)) => stream,
            Ok(None) => {
                debug!(target_id = id, "flux RRD non lisible : Sys.Audit manquant sur « / »");
                return Vec::new();
            }
            Err(error) => {
                debug!(target_id = id, %error, "flux RRD indisponible");
                return Vec::new();
            }
        };

        let export = export::export_samples(&stream, since);
        if let Some(latest) = export.latest_timestamp {
            self.export_cursor
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .insert(id, latest);
        }
        debug!(
            target_id = id,
            points = export.samples.len(),
            unknown = export.unknown,
            "flux RRD lu"
        );
        export.samples
    }

    /// Système et adresses des invités en marche, rafraîchis une fois par heure.
    ///
    /// Le verrou n'est jamais tenu pendant un appel réseau : on lit la mémoire,
    /// on relâche, on interroge, on réécrit. Les faits déjà connus sont
    /// republiés à l'horodatage de cette collecte, de sorte que la série ne
    /// clignote pas entre deux rafraîchissements.
    async fn guest_fact_samples(
        &self,
        pve: &PveClient,
        id: TargetId,
        guests: &GuestIndex,
        running: &BTreeSet<i64>,
        now_s: i64,
        ts_ms: i64,
    ) -> Vec<Sample> {
        let (mut samples, stale) = {
            let mut cache = self.facts.lock().unwrap_or_else(|poison| poison.into_inner());
            let facts = cache.entry(id).or_default();
            // Un invité détruit ne doit pas garder sa place en mémoire.
            facts.retain(|vmid, _| guests.contains_key(vmid));

            let mut samples = Vec::new();
            let mut stale = Vec::new();
            for vmid in running {
                let Some(guest) = guests.get(vmid) else { continue };
                match facts.get(vmid) {
                    Some(fact) if fact.fetched_at + GUEST_FACTS_TTL_SECONDS > now_s => {
                        samples.extend(fact.samples.iter().cloned().map(|mut sample| {
                            sample.ts_ms = ts_ms;
                            sample
                        }));
                    }
                    _ => stale.push((*vmid, guest.clone())),
                }
            }
            (samples, stale)
        };

        if stale.is_empty() {
            return samples;
        }

        let semaphore = tokio::sync::Semaphore::new(SNAPSHOT_PARALLELISM);
        let fetched = futures::future::join_all(stale.iter().map(|(vmid, guest)| {
            let semaphore = &semaphore;
            async move { (*vmid, fetch_guest_facts(pve, semaphore, *vmid, guest, ts_ms).await) }
        }))
        .await;

        let mut cache = self.facts.lock().unwrap_or_else(|poison| poison.into_inner());
        let facts = cache.entry(id).or_default();
        for (vmid, fresh) in fetched {
            samples.extend(fresh.iter().cloned());
            facts.insert(vmid, GuestFact { fetched_at: now_s, samples: fresh });
        }
        samples
    }

    /// Compare les versions de paquets de chaque nœud à celles de l'interrogation
    /// précédente et renvoie les séries de changement.
    ///
    /// Le verrou n'est tenu que le temps de la comparaison, jamais pendant un
    /// appel réseau ; les listes ont déjà été rapportées par la tournée des nœuds.
    fn package_change_samples(
        &self,
        id: TargetId,
        versions: BTreeMap<String, Vec<AptVersion>>,
        now_s: i64,
        ts_ms: i64,
    ) -> Vec<Sample> {
        let mut cache = self.packages.lock().unwrap_or_else(|poison| poison.into_inner());
        let memories = cache.entry(id).or_default();
        let mut samples = Vec::new();
        for (node, list) in versions {
            let memory = memories.entry(node.clone()).or_default();
            let change = memory.observe(&list, now_s);
            if let Some(change) = change {
                debug!(target_id = id, node = %node, changes = %change.summary, "paquets mis à jour");
            }
            samples.extend(apt::package_change_samples(&node, change, ts_ms));
        }
        samples
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

        // L'inventaire du cluster et la liste des nœuds conditionnent tout le
        // reste : l'un dit quels invités existent, l'autre quels nœuds
        // interroger. Ils partent ensemble, avant la suite.
        let (inventory, node_list) = futures::join!(
            when(
                options.cluster_resources,
                pve.get::<Vec<ResourceEntry>>("/cluster/resources", &[])
            ),
            pve.get::<Vec<NodeListEntry>>("/nodes", &[]),
        );

        let inventory = match inventory {
            Some(Ok(entries)) => Some(resources::inventory(&entries, ts_ms)),
            Some(Err(error)) => {
                errors += 1;
                warn!(target_id = target.id, %error, "inventaire du cluster indisponible");
                None
            }
            None => None,
        };

        let node_list = match node_list {
            Ok(list) => list,
            Err(error) => {
                errors += 1;
                warn!(target_id = target.id, %error, "liste des nœuds Proxmox indisponible");
                Vec::new()
            }
        };

        // Les endpoints Ceph propres aux nœuds décrivent le cluster entier :
        // les interroger sur chaque nœud multiplierait le coût sans rien
        // apprendre. Un nœud en ligne suffit.
        let ceph_node = node_list
            .iter()
            .find(|entry| entry.is_online() && options.wants_node(&entry.node))
            .map(|entry| entry.node.clone());

        // Les appels à l'échelle du cluster et la tournée des nœuds sont
        // indépendants : tout part en même temps.
        let budget = SnapshotBudget::new(options.max_snapshot_guests);
        let guests_by_node = inventory.as_ref().map(|inventory| &inventory.guests_by_node);
        let (cluster, ha, ha_manager, backups, ceph_status, ceph_detail, rrd, nodes) = futures::join!(
            pve.get::<Vec<ClusterStatusEntry>>("/cluster/status", &[]),
            when(options.ha, pve.get::<Vec<HaStatusEntry>>("/cluster/ha/status/current", &[])),
            when(
                options.ha,
                pve.get_unless::<HaManagerStatus>(
                    "/cluster/ha/status/manager_status",
                    &[],
                    &OPTIONAL_STATUSES
                )
            ),
            collect_backup_jobs(&pve, &options),
            when(options.ceph, pve.get::<CephStatus>("/cluster/ceph/status", &[])),
            collect_ceph_detail(&pve, &options, ceph_node.as_deref(), ts_ms),
            self.collect_rrd_export(&pve, &options, target.id, now_s),
            collect_nodes(&pve, &options, &node_list, guests_by_node, &budget, now_s, ts_ms),
        );

        if let Some(inventory) = inventory {
            samples.extend(inventory.samples);
        }
        samples.extend(ceph_detail);
        samples.extend(rrd);

        match ha_manager {
            Some(Ok(Some(status))) => samples.extend(ha::manager_samples(&status, now_s, ts_ms)),
            Some(Ok(None)) => {
                debug!(target_id = target.id, "état détaillé de la HA non lisible : droit manquant")
            }
            Some(Err(error)) => {
                debug!(target_id = target.id, %error, "pas de gestionnaire de haute disponibilité")
            }
            None => {}
        }

        let (jobs, not_backed_up, volume_samples) = backups;
        samples.extend(volume_samples);

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

        let aggregate = nodes;
        errors += aggregate.errors;
        samples.extend(aggregate.samples);
        if options.packages {
            samples.extend(self.package_change_samples(
                target.id,
                aggregate.package_versions,
                now_s,
                ts_ms,
            ));
        }
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

        // Le système et les adresses viennent en dernier : ils ont besoin de
        // l'inventaire complet, et l'essentiel est déjà rassemblé si l'agent
        // d'une machine se fait attendre.
        if options.guest_os {
            samples.extend(
                self.guest_fact_samples(
                    &pve,
                    target.id,
                    &aggregate.guests,
                    &aggregate.running_guests,
                    now_s,
                    ts_ms,
                )
                .await,
            );
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

/// Statuts qui signifient « pas ici » plutôt que « en panne ».
///
/// Un droit non accordé (403), un endpoint apparu dans une version plus récente
/// (404) ou une fonctionnalité non compilée (501) sont des cas normaux : ils
/// donnent `Ok(None)`, pas une erreur de collecte.
const OPTIONAL_STATUSES: [StatusCode; 3] =
    [StatusCode::FORBIDDEN, StatusCode::NOT_FOUND, StatusCode::NOT_IMPLEMENTED];

/// Travaux de sauvegarde planifiés, invités non couverts, et — pour chaque
/// travail — les volumes qu'il embarque réellement.
///
/// Les trois lectures tiennent ensemble parce que la troisième a besoin de la
/// première : on ne connaît l'identifiant d'un travail qu'après l'avoir listé.
#[allow(clippy::type_complexity)]
async fn collect_backup_jobs(
    pve: &PveClient,
    options: &Options,
) -> (
    Option<Result<Vec<BackupJob>, ProbeError>>,
    Option<Result<Vec<NotBackedUp>, ProbeError>>,
    Vec<Sample>,
) {
    let (jobs, not_backed_up) = futures::join!(
        when(options.backup_jobs, pve.get::<Vec<BackupJob>>("/cluster/backup", &[])),
        when(
            options.backup_jobs,
            pve.get::<Vec<NotBackedUp>>("/cluster/backup-info/not-backed-up", &[])
        ),
    );

    let mut samples = Vec::new();
    if options.backup_volumes
        && let Some(Ok(list)) = &jobs
    {
        let ts_ms = chrono::Utc::now().timestamp_millis();
        let trees = futures::future::join_all(list.iter().map(|job| async move {
            let path = format!("/cluster/backup/{}/included_volumes", job.id);
            (
                job.id.as_str(),
                pve.get_unless::<IncludedVolumes>(&path, &[], &OPTIONAL_STATUSES).await,
            )
        }))
        .await;

        for (job, tree) in trees {
            match tree {
                Ok(Some(tree)) => {
                    samples.extend(backup::included_volume_samples(job, &tree, ts_ms))
                }
                Ok(None) => debug!(job, "contenu du travail de sauvegarde non lisible"),
                Err(error) => debug!(job, %error, "contenu du travail de sauvegarde indisponible"),
            }
        }
    }

    (jobs, not_backed_up, samples)
}

/// Le détail de Ceph : OSD, pools, CephFS, drapeaux et sourdines.
///
/// Rien ici ne compte comme erreur : sans Ceph installé, PVE répond 500
/// « rados_connect failed », et `Datastore.Audit` n'est pas toujours accordé.
/// Un cluster sans Ceph doit rester parfaitement silencieux.
async fn collect_ceph_detail(
    pve: &PveClient,
    options: &Options,
    node: Option<&str>,
    ts_ms: i64,
) -> Vec<Sample> {
    if !options.ceph || !options.ceph_detail {
        return Vec::new();
    }
    let Some(node) = node else { return Vec::new() };

    let (osd_path, pool_path, fs_path) = (
        format!("/nodes/{node}/ceph/osd"),
        format!("/nodes/{node}/ceph/pool"),
        format!("/nodes/{node}/ceph/fs"),
    );
    let (osd, pools, filesystems, flags, mutes) = futures::join!(
        pve.get::<CephOsdTree>(&osd_path, &[]),
        pve.get::<Vec<CephPool>>(&pool_path, &[]),
        pve.get::<Vec<CephFs>>(&fs_path, &[]),
        pve.get::<Vec<CephFlag>>("/cluster/ceph/flags", &[]),
        pve.get::<Vec<CephHealthMute>>("/cluster/ceph/health-mute", &[]),
    );

    let mut samples = Vec::new();
    match osd {
        Ok(tree) => samples.extend(ceph::osd_samples(&tree, ts_ms)),
        Err(error) => debug!(node, %error, "arbre des OSD Ceph indisponible"),
    }
    match pools {
        Ok(list) => samples.extend(ceph::pool_samples(&list, ts_ms)),
        Err(error) => debug!(node, %error, "pools Ceph indisponibles"),
    }
    match filesystems {
        Ok(list) => samples.extend(ceph::fs_samples(&list, ts_ms)),
        Err(error) => debug!(node, %error, "systèmes de fichiers Ceph indisponibles"),
    }
    match flags {
        Ok(list) => samples.extend(ceph::flag_samples(&list, ts_ms)),
        Err(error) => debug!(%error, "drapeaux Ceph indisponibles"),
    }
    match mutes {
        Ok(list) => samples.extend(ceph::health_mute_samples(&list, ts_ms)),
        Err(error) => debug!(%error, "sourdines de santé Ceph indisponibles"),
    }
    samples
}

/// Les inventaires d'un nœud ajoutés par-dessus `/status` : démons, version,
/// réseau, LVM.
///
/// Chacun demande un droit que `PVEAuditor` n'accorde pas toujours et peut
/// manquer d'une version à l'autre : aucun échec ne compte comme erreur de
/// collecte, la tournée du nœud a déjà livré l'essentiel.
async fn collect_node_extras(
    pve: &PveClient,
    options: &Options,
    node: &str,
    ts_ms: i64,
) -> Vec<Sample> {
    let paths = [
        format!("/nodes/{node}/services"),
        format!("/nodes/{node}/version"),
        format!("/nodes/{node}/network"),
        format!("/nodes/{node}/netstat"),
        format!("/nodes/{node}/disks/lvm"),
        format!("/nodes/{node}/disks/lvmthin"),
        format!("/nodes/{node}/disks/directory"),
    ];
    let optional = &OPTIONAL_STATUSES;

    let (services, version, network, netstat, lvm, thin, directories) = futures::join!(
        when(options.services, pve.get_unless::<Vec<ServiceEntry>>(&paths[0], &[], optional)),
        when(options.services, pve.get_unless::<model::Version>(&paths[1], &[], optional)),
        when(options.network, pve.get_unless::<Vec<NetworkInterface>>(&paths[2], &[], optional)),
        when(options.network, pve.get_unless::<Vec<NetstatEntry>>(&paths[3], &[], optional)),
        when(options.lvm, pve.get_unless::<LvmTree>(&paths[4], &[], optional)),
        when(options.lvm, pve.get_unless::<Vec<ThinPool>>(&paths[5], &[], optional)),
        when(options.lvm, pve.get_unless::<Vec<DirectoryMount>>(&paths[6], &[], optional)),
    );

    let mut samples = Vec::new();
    match services {
        Some(Ok(Some(list))) => samples.extend(node::service_samples(node, &list, ts_ms)),
        Some(Ok(None)) => debug!(node, "démons non listés : Sys.Audit manquant sur le nœud"),
        Some(Err(error)) => debug!(node, %error, "liste des démons indisponible"),
        None => {}
    }
    match version {
        Some(Ok(Some(version))) => samples.extend(node::version_samples(node, &version, ts_ms)),
        Some(Ok(None)) | None => {}
        Some(Err(error)) => debug!(node, %error, "version du nœud indisponible"),
    }
    match network {
        Some(Ok(Some(list))) => samples.extend(node::network_samples(node, &list, ts_ms)),
        Some(Ok(None)) => debug!(node, "interfaces non listées : droit manquant"),
        Some(Err(error)) => debug!(node, %error, "interfaces réseau indisponibles"),
        None => {}
    }
    match netstat {
        Some(Ok(Some(list))) => samples.extend(node::netstat_samples(node, &list, ts_ms)),
        Some(Ok(None)) | None => {}
        Some(Err(error)) => debug!(node, %error, "compteurs réseau par invité indisponibles"),
    }
    match lvm {
        Some(Ok(Some(tree))) => samples.extend(node::lvm_samples(node, &tree, ts_ms)),
        Some(Ok(None)) => {
            debug!(node, "groupes de volumes non listés : Sys.Audit manquant sur « / »")
        }
        Some(Err(error)) => debug!(node, %error, "groupes de volumes indisponibles"),
        None => {}
    }
    match thin {
        Some(Ok(Some(list))) => samples.extend(node::thinpool_samples(node, &list, ts_ms)),
        Some(Ok(None)) | None => {}
        Some(Err(error)) => debug!(node, %error, "pools à provisionnement fin indisponibles"),
    }
    match directories {
        Some(Ok(Some(list))) => samples.extend(node::directory_samples(node, &list, ts_ms)),
        Some(Ok(None)) | None => {}
        Some(Err(error)) => debug!(node, %error, "montages gérés indisponibles"),
    }
    samples
}

/// Interroge l'agent d'un invité pour savoir ce qui tourne dedans et à quelle
/// adresse il répond.
///
/// Muet sur tous les fronts : une VM sans agent, un agent sans le droit
/// `VM.GuestAgent.Audit`, un conteneur dont PVE ne lit pas l'espace de noms —
/// aucun de ces cas n'est une anomalie.
async fn fetch_guest_facts(
    pve: &PveClient,
    semaphore: &tokio::sync::Semaphore,
    vmid: i64,
    guest: &GuestRef,
    ts_ms: i64,
) -> Vec<Sample> {
    let _permit = semaphore.acquire().await.ok();
    let node = guest.node.as_str();
    // Les séries portent les étiquettes d'identité de l'invité : un squelette
    // suffit, l'inventaire a déjà publié ses mesures.
    let entry = GuestEntry {
        vmid: Num(vmid as f64),
        name: Some(guest.name.clone()),
        status: Some("running".to_string()),
        ..Default::default()
    };

    match guest.kind {
        GuestKind::Qemu => {
            let (os_path, net_path) = (
                format!("/nodes/{node}/qemu/{vmid}/agent/get-osinfo"),
                format!("/nodes/{node}/qemu/{vmid}/agent/network-get-interfaces"),
            );
            let (os, addresses) = futures::join!(
                pve.get::<AgentOsInfo>(&os_path, &[]),
                pve.get::<AgentInterfaces>(&net_path, &[]),
            );
            let mut samples = Vec::new();
            match os {
                Ok(info) => samples.extend(guest::os_samples(node, &entry, &info, ts_ms)),
                Err(error) => debug!(node, vmid, %error, "système de la VM non rapporté"),
            }
            match addresses {
                Ok(list) => {
                    samples.extend(guest::agent_address_samples(node, &entry, &list, ts_ms))
                }
                Err(error) => debug!(node, vmid, %error, "adresses de la VM non rapportées"),
            }
            samples
        }
        GuestKind::Lxc => {
            let path = format!("/nodes/{node}/lxc/{vmid}/interfaces");
            match pve.get::<Vec<LxcInterface>>(&path, &[]).await {
                Ok(list) => guest::lxc_address_samples(node, &entry, &list, ts_ms),
                Err(error) => {
                    debug!(node, vmid, %error, "adresses du conteneur non rapportées");
                    Vec::new()
                }
            }
        }
    }
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

/// Collecte les nœuds en parallèle, puis complète l'inventaire par ce que seul
/// le cluster sait.
///
/// La tournée ne voit que les nœuds qui répondent. Quand `/cluster/resources` a
/// répondu, ses invités sont réinjectés ici : ceux d'un nœud injoignable
/// existent toujours, et leurs sauvegardes comme leur couverture doivent
/// continuer d'être suivies.
async fn collect_nodes(
    pve: &PveClient,
    options: &Options,
    nodes: &[NodeListEntry],
    guests_by_node: Option<&BTreeMap<String, Vec<GuestSummary>>>,
    budget: &SnapshotBudget,
    now_s: i64,
    ts_ms: i64,
) -> Aggregate {
    let outcomes = futures::future::join_all(
        nodes.iter().filter(|node| options.wants_node(&node.node)).map(|node| {
            let known = guests_by_node
                .map(|index| index.get(&node.node).map(Vec::as_slice).unwrap_or_default());
            collect_node(pve, options, node, known, budget, now_s, ts_ms)
        }),
    )
    .await;

    let mut aggregate = merge(outcomes);

    if let Some(index) = guests_by_node {
        for (node, guests) in index.iter().filter(|(node, _)| options.wants_node(node)) {
            for guest in guests.iter().filter(|guest| !guest.template) {
                aggregate.guests.entry(guest.vmid).or_insert_with(|| GuestRef {
                    node: node.clone(),
                    name: guest.name.clone(),
                    kind: guest.kind,
                });
                if guest.running {
                    aggregate.running_guests.insert(guest.vmid);
                }
            }
        }
    }

    aggregate
}

/// Ce qu'un nœud a livré, avant fusion à l'échelle du cluster.
#[derive(Default)]
struct NodeOutcome {
    node: String,
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
    /// VMID des invités en marche sur ce nœud, pour les appels qui n'ont de sens
    /// que sur une machine démarrée (agent, adresses).
    running_guests: BTreeSet<i64>,
    /// Versions des paquets importants du nœud, à comparer à l'interrogation
    /// précédente (`ProxmoxCollector::packages`).
    package_versions: Option<Vec<AptVersion>>,
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
    running_guests: BTreeSet<i64>,
    /// Versions des paquets par nœud, pour les nœuds qui ont répondu.
    package_versions: BTreeMap<String, Vec<AptVersion>>,
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
        aggregate.running_guests.extend(outcome.running_guests);
        if let Some(versions) = outcome.package_versions {
            aggregate.package_versions.insert(outcome.node.clone(), versions);
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
    known_guests: Option<&[GuestSummary]>,
    budget: &SnapshotBudget,
    now_s: i64,
    ts_ms: i64,
) -> NodeOutcome {
    let mut outcome = NodeOutcome { node: node.node.clone(), ..Default::default() };
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
    let disks_path = format!("/nodes/{name}/disks/list");
    let zfs_path = format!("/nodes/{name}/disks/zfs");
    let versions_path = format!("/nodes/{name}/apt/versions");
    let subscription_path = format!("/nodes/{name}/subscription");
    let repositories_path = format!("/nodes/{name}/apt/repositories");
    // Un droit manquant sur un inventaire facultatif n'est pas une erreur.
    let optional = [StatusCode::FORBIDDEN, StatusCode::NOT_FOUND, StatusCode::NOT_IMPLEMENTED];

    // Les inventaires d'un nœud sont indépendants : les enchaîner multiplierait
    // d'autant le temps passé sur un nœud lent.
    let (
        qemu,
        lxc,
        storages,
        tasks,
        replication,
        updates,
        certificates,
        disks,
        zfs,
        versions,
        subscription,
        repositories,
        extras,
    ) = futures::join!(
        // L'inventaire du cluster a déjà tout dit des invités : ces deux appels
        // ne partent que s'il a manqué.
        when(known_guests.is_none(), pve.get::<Vec<GuestEntry>>(&qemu_path, &[])),
        when(known_guests.is_none(), pve.get::<Vec<GuestEntry>>(&lxc_path, &[])),
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
        when(options.disks, pve.get_unless::<Vec<DiskEntry>>(&disks_path, &[], &optional)),
        // `zpool` absent : PVE répond 500. Ce n'est pas une panne du nœud, dont
        // `/status` vient de répondre.
        when(
            options.zfs,
            pve.get_unless::<Vec<ZfsPool>>(
                &zfs_path,
                &[],
                &[
                    StatusCode::FORBIDDEN,
                    StatusCode::NOT_FOUND,
                    StatusCode::NOT_IMPLEMENTED,
                    StatusCode::INTERNAL_SERVER_ERROR,
                ],
            )
        ),
        when(options.packages, pve.get_unless::<Vec<AptVersion>>(&versions_path, &[], &optional)),
        when(
            options.subscription,
            pve.get_unless::<Subscription>(&subscription_path, &[], &optional)
        ),
        when(
            options.subscription,
            pve.get_unless::<Repositories>(&repositories_path, &[], &optional)
        ),
        collect_node_extras(pve, options, name, ts_ms),
    );

    outcome.samples.extend(extras);

    let mut running_vms: Vec<GuestEntry> = Vec::new();
    match known_guests {
        // Inventaire du cluster : les séries sont déjà publiées, il ne reste
        // qu'à savoir qui interroger ensuite.
        Some(guests) => {
            for guest in guests.iter().filter(|guest| !guest.template) {
                outcome.guests.insert(
                    guest.vmid,
                    GuestRef { node: name.to_string(), name: guest.name.clone(), kind: guest.kind },
                );
                if guest.running {
                    outcome.running_guests.insert(guest.vmid);
                    if guest.kind == GuestKind::Qemu && options.guest_agent {
                        running_vms.push(GuestEntry {
                            vmid: Num(guest.vmid as f64),
                            name: Some(guest.name.clone()),
                            status: Some("running".to_string()),
                            ..Default::default()
                        });
                    }
                }
            }
        }
        None => {
            for (kind, result) in [(GuestKind::Qemu, qemu), (GuestKind::Lxc, lxc)] {
                match result {
                    Some(Ok(mut guests)) => {
                        for guest in guests.iter().filter(|guest| !guest.is_template()) {
                            outcome.guests.insert(
                                guest.vmid(),
                                GuestRef {
                                    node: name.to_string(),
                                    name: guest.display_name(),
                                    kind,
                                },
                            );
                            if guest.is_running() {
                                outcome.running_guests.insert(guest.vmid());
                            }
                        }
                        outcome.samples.extend(metrics::guest_samples(name, kind, &guests, ts_ms));
                        if kind == GuestKind::Qemu && options.guest_agent {
                            running_vms =
                                guests.drain(..).filter(|guest| guest.is_running()).collect();
                        }
                    }
                    Some(Err(error)) => {
                        outcome.errors += 1;
                        warn!(node = name, kind = kind.as_str(), %error, "inventaire des invités échoué");
                    }
                    None => {}
                }
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
            outcome.samples.extend(apt::updates_samples(name, &packages, ts_ms));
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

    match disks {
        Some(Ok(Some(list))) => {
            outcome.samples.extend(disks::disk_samples(name, &list, ts_ms));
            collect_smart(pve, name, &list, &mut outcome, ts_ms).await;
        }
        Some(Ok(None)) => debug!(node = name, "disques non listés : droit manquant"),
        Some(Err(error)) => {
            outcome.errors += 1;
            warn!(node = name, %error, "liste des disques indisponible");
        }
        None => {}
    }

    match zfs {
        Some(Ok(Some(pools))) => outcome.samples.extend(disks::zfs_samples(name, &pools, ts_ms)),
        Some(Ok(None)) => debug!(node = name, "pas de pool ZFS listé sur ce nœud"),
        Some(Err(error)) => {
            outcome.errors += 1;
            warn!(node = name, %error, "pools ZFS indisponibles");
        }
        None => {}
    }

    match versions {
        Some(Ok(Some(list))) => outcome.package_versions = Some(list),
        Some(Ok(None)) => debug!(node = name, "versions des paquets non listées : droit manquant"),
        Some(Err(error)) => {
            outcome.errors += 1;
            warn!(node = name, %error, "versions des paquets indisponibles");
        }
        None => {}
    }

    match subscription {
        Some(Ok(Some(subscription))) => {
            outcome.samples.extend(apt::subscription_samples(name, &subscription, ts_ms));
        }
        Some(Ok(None)) => debug!(node = name, "abonnement non lu : droit manquant"),
        Some(Err(error)) => {
            outcome.errors += 1;
            warn!(node = name, %error, "état de l'abonnement indisponible");
        }
        None => {}
    }

    match repositories {
        Some(Ok(Some(repositories))) => {
            outcome.samples.extend(apt::repository_samples(name, &repositories, ts_ms));
        }
        Some(Ok(None)) => debug!(node = name, "dépôts non lus : droit manquant"),
        Some(Err(error)) => {
            outcome.errors += 1;
            warn!(node = name, %error, "dépôts APT indisponibles");
        }
        None => {}
    }

    if !running_vms.is_empty() {
        collect_guest_details(pve, name, &running_vms, &mut outcome, ts_ms).await;
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

/// Lit le rapport SMART de chaque disque, `SNAPSHOT_PARALLELISM` appels en vol.
///
/// Un rapport qui échoue (disque USB muet, contrôleur RAID qui ne relaie pas
/// SMART) ne compte pas comme erreur : la liste a déjà donné la santé, il ne
/// manque que la température.
async fn collect_smart(
    pve: &PveClient,
    node: &str,
    list: &[DiskEntry],
    outcome: &mut NodeOutcome,
    ts_ms: i64,
) {
    let semaphore = tokio::sync::Semaphore::new(SNAPSHOT_PARALLELISM);
    let reports = futures::future::join_all(list.iter().map(|disk| {
        let semaphore = &semaphore;
        async move {
            let _permit = semaphore.acquire().await.ok();
            let path = format!("/nodes/{node}/disks/smart");
            (disk, pve.get::<SmartReport>(&path, &[("disk", disk.devpath.clone())]).await)
        }
    }))
    .await;

    for (disk, result) in reports {
        match result {
            Ok(report) => outcome.samples.extend(disks::smart_samples(node, disk, &report, ts_ms)),
            Err(error) => {
                debug!(node, disk = %disk.devpath, %error, "rapport SMART indisponible");
            }
        }
    }
}

/// Détail des machines virtuelles en marche : état courant, puis systèmes de
/// fichiers pour celles dont l'agent QEMU est activé.
///
/// Tout ici est facultatif et se dégrade sans bruit : `VM.Monitor` manquant
/// (403), agent activé mais pas installé ou pas encore démarré (500 « not
/// running »), machine qui vient de s'éteindre entre deux appels. Aucun de ces
/// cas ne compte comme erreur de collecte — la liste des invités a déjà donné
/// l'essentiel.
async fn collect_guest_details(
    pve: &PveClient,
    node: &str,
    running_vms: &[GuestEntry],
    outcome: &mut NodeOutcome,
    ts_ms: i64,
) {
    let semaphore = tokio::sync::Semaphore::new(SNAPSHOT_PARALLELISM);
    let details = futures::future::join_all(running_vms.iter().map(|guest| {
        let semaphore = &semaphore;
        async move {
            let _permit = semaphore.acquire().await.ok();
            let vmid = guest.vmid();
            let status_path = format!("/nodes/{node}/qemu/{vmid}/status/current");
            let status = pve.get::<QemuStatus>(&status_path, &[]).await;
            let fsinfo = match &status {
                Ok(status) if status.agent_enabled() => {
                    let path = format!("/nodes/{node}/qemu/{vmid}/agent/get-fsinfo");
                    Some(pve.get::<AgentFsInfo>(&path, &[]).await)
                }
                _ => None,
            };
            (guest, status, fsinfo)
        }
    }))
    .await;

    for (guest, status, fsinfo) in details {
        match status {
            Ok(status) => {
                outcome.samples.extend(guest::qemu_status_samples(node, guest, &status, ts_ms));
            }
            Err(error) => {
                debug!(node, vmid = guest.vmid(), %error, "état détaillé de la VM indisponible");
            }
        }
        match fsinfo {
            Some(Ok(info)) => outcome.samples.extend(guest::fs_samples(node, guest, &info, ts_ms)),
            Some(Err(error)) => {
                debug!(node, vmid = guest.vmid(), %error, "agent QEMU muet ou droit VM.Monitor manquant");
                outcome.samples.push(guest::agent_silent_sample(node, guest, ts_ms));
            }
            None => {}
        }
    }
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

    use dumbmonit_proto::Credential;

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
            Credential::ApiToken { token: "monitoring@pve!dumbmonit=8f3a1c9e-dead-beef".into() };
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

    #[test]
    fn les_versions_de_paquets_sont_comparees_dune_interrogation_a_lautre_par_noeud() {
        let collector = ProxmoxCollector::new();
        let liste = |version: &str| -> Vec<AptVersion> {
            serde_json::from_str(&format!(
                r#"[{{"Package":"pve-manager","Version":"{version}","OldVersion":"{version}","CurrentState":"Installed"}}]"#
            ))
            .unwrap()
        };
        let premiere = collector.package_change_samples(
            7,
            BTreeMap::from([("pve1".to_string(), liste("8.2.4"))]),
            1_000,
            1_000_000,
        );
        assert_eq!(premiere.len(), 1);
        assert_eq!(premiere[0].value, 0.0, "la première interrogation ne fait qu'apprendre");

        let seconde = collector.package_change_samples(
            7,
            BTreeMap::from([
                ("pve1".to_string(), liste("8.2.7")),
                ("pve2".to_string(), liste("8.2.4")),
            ]),
            1_060,
            1_060_000,
        );
        let pve1 = seconde.iter().find(|s| s.labels["node"] == "pve1").unwrap();
        assert_eq!(pve1.value, 1.0);
        assert_eq!(pve1.labels["changes"], "pve-manager 8.2.4→8.2.7");
        let pve2 = seconde.iter().find(|s| s.labels["node"] == "pve2").unwrap();
        assert_eq!(pve2.value, 0.0, "pve2 n'a pas de référence, rien à signaler");

        // Une autre cible a sa propre mémoire.
        let autre = collector.package_change_samples(
            8,
            BTreeMap::from([("pve1".to_string(), liste("8.2.7"))]),
            1_060,
            1_060_000,
        );
        assert_eq!(autre[0].value, 0.0);
    }

    #[test]
    fn la_fusion_rapporte_les_versions_de_paquets_par_noeud() {
        let a = NodeOutcome {
            node: "pve1".into(),
            package_versions: Some(vec![AptVersion::default()]),
            ..Default::default()
        };
        let b = NodeOutcome { node: "pve2".into(), ..Default::default() };
        let aggregate = merge(vec![a, b]);
        assert_eq!(aggregate.package_versions.len(), 1);
        assert!(aggregate.package_versions.contains_key("pve1"));
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

/// Interrogation complète contre un faux Proxmox monté dans le test.
///
/// Les tests unitaires des modules couvrent chacun sa conversion ; celui-ci
/// couvre ce qu'aucun d'eux ne voit : l'orchestration. Qu'un nœud injoignable
/// laisse quand même ses invités dans l'inventaire, qu'un endpoint absent ou
/// interdit ne compte pas comme une erreur, et que rien ne part quand l'option
/// est coupée.
#[cfg(test)]
mod e2e {
    use std::collections::BTreeMap;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::Duration;

    use axum::extract::{Path, State};
    use axum::http::StatusCode;
    use axum::response::{IntoResponse, Response};
    use axum::routing::get;
    use axum::{Json, Router};
    use dumbmonit_proto::{Collector, Credential, Sample, Target};
    use serde_json::{Value, json};

    use super::ProxmoxCollector;

    /// Compteur d'appels par chemin, pour prouver qu'une option coupée ne part pas
    /// et qu'un fait mis en cache n'est pas redemandé.
    #[derive(Default)]
    struct Calls {
        osinfo: AtomicU32,
        qemu_list: AtomicU32,
        export: AtomicU32,
    }

    fn data(value: Value) -> Response {
        Json(json!({ "data": value })).into_response()
    }

    /// Ce que répond `pveproxy` pour un nœud qui ne répond plus.
    fn node_down() -> Response {
        (StatusCode::from_u16(595).unwrap(), Json(json!({"data": null, "message": "timed out"})))
            .into_response()
    }

    fn forbidden() -> Response {
        (StatusCode::FORBIDDEN, Json(json!({"data": null, "message": "Permission check failed"})))
            .into_response()
    }

    async fn serve(calls: Arc<Calls>) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let router = Router::new()
            .route("/api2/json/version", get(|| async { data(json!({"version": "8.2.4"})) }))
            .route("/api2/json/cluster/resources", get(resources))
            .route(
                "/api2/json/nodes",
                get(|| async {
                    data(json!([
                        {"node": "pve1", "status": "online"},
                        {"node": "pve2", "status": "offline"}
                    ]))
                }),
            )
            .route("/api2/json/cluster/status", get(|| async { data(json!([])) }))
            .route("/api2/json/cluster/ha/status/current", get(|| async { data(json!([])) }))
            .route("/api2/json/cluster/ha/status/manager_status", get(manager_status))
            .route("/api2/json/cluster/backup", get(|| async { data(json!([{"id": "job-1"}])) }))
            .route("/api2/json/cluster/backup/{id}/included_volumes", get(included_volumes))
            .route(
                "/api2/json/cluster/backup-info/not-backed-up",
                get(|| async { data(json!([])) }),
            )
            // Ceph absent : c'est ainsi que PVE le dit.
            .route(
                "/api2/json/cluster/ceph/status",
                get(|| async {
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(json!({"data": null, "message": "rados_connect failed"})),
                    )
                        .into_response()
                }),
            )
            .route("/api2/json/cluster/metrics/export", get(export))
            .route("/api2/json/nodes/{node}/status", get(node_status))
            .route("/api2/json/nodes/{node}/qemu", get(qemu_list))
            .route("/api2/json/nodes/{node}/lxc", get(|| async { data(json!([])) }))
            .route("/api2/json/nodes/{node}/storage", get(|| async { data(json!([])) }))
            .route("/api2/json/nodes/{node}/tasks", get(|| async { data(json!([])) }))
            .route("/api2/json/nodes/{node}/services", get(services))
            .route(
                "/api2/json/nodes/{node}/version",
                get(|| async { data(json!({"version": "8.2.4", "release": "8.2"})) }),
            )
            .route("/api2/json/nodes/{node}/network", get(network))
            .route(
                "/api2/json/nodes/{node}/netstat",
                get(|| async {
                    data(json!([{"dev": "tap100i0", "vmid": "100", "in": 42, "out": 7}]))
                }),
            )
            // Droits non accordés à `PVEAuditor` : tous doivent rester muets.
            .route("/api2/json/nodes/{node}/apt/update", get(|| async { forbidden() }))
            .route("/api2/json/nodes/{node}/certificates/info", get(|| async { forbidden() }))
            .route("/api2/json/nodes/{node}/disks/lvm", get(|| async { forbidden() }))
            .route("/api2/json/nodes/{node}/disks/lvmthin", get(thinpools))
            .route("/api2/json/nodes/{node}/disks/directory", get(|| async { data(json!([])) }))
            .route(
                "/api2/json/nodes/{node}/qemu/{vmid}/status/current",
                get(|| async { data(json!({"agent": 1})) }),
            )
            .route(
                "/api2/json/nodes/{node}/qemu/{vmid}/agent/get-fsinfo",
                get(|| async { data(json!({"result": []})) }),
            )
            .route("/api2/json/nodes/{node}/qemu/{vmid}/agent/get-osinfo", get(osinfo))
            .route(
                "/api2/json/nodes/{node}/qemu/{vmid}/agent/network-get-interfaces",
                get(|| async {
                    data(json!({"result": [
                        {"name": "lo", "ip-addresses": [{"ip-address": "127.0.0.1"}]},
                        {"name": "ens18", "ip-addresses": [{"ip-address": "192.168.10.50"}]}
                    ]}))
                }),
            )
            .route(
                "/api2/json/nodes/{node}/qemu/{vmid}/snapshot",
                get(|| async { data(json!([])) }),
            )
            .route("/api2/json/nodes/{node}/lxc/{vmid}/snapshot", get(|| async { data(json!([])) }))
            .route(
                "/api2/json/nodes/{node}/lxc/{vmid}/interfaces",
                get(|| async { data(json!([{"name": "eth0", "inet": "192.168.10.60/24"}])) }),
            )
            // Tout le reste (réplication, apt, certificats, disques, ZFS…) est
            // absent : un `PVEAuditor` sur une vieille version n'a rien de plus.
            .fallback(|| async {
                (StatusCode::NOT_IMPLEMENTED, Json(json!({"data": null}))).into_response()
            })
            .with_state(calls);
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        format!("http://{address}")
    }

    async fn resources() -> Response {
        data(json!([
            {"type": "node", "id": "node/pve1", "node": "pve1", "status": "online", "cpu": 0.04, "maxcpu": 8},
            {"type": "node", "id": "node/pve2", "node": "pve2", "status": "unknown"},
            {"type": "qemu", "id": "qemu/100", "node": "pve1", "vmid": 100, "name": "router-vm",
             "status": "running", "cpu": 0.05, "maxcpu": 2, "mem": 1000, "maxmem": 2000,
             "uptime": 864000, "pool": "production"},
            // L'invité du nœud injoignable : c'est lui que la tournée par nœud
            // perdrait, et c'est tout l'intérêt de `/cluster/resources`.
            {"type": "lxc", "id": "lxc/202", "node": "pve2", "vmid": 202, "name": "nextcloud",
             "status": "unknown", "maxcpu": 4, "maxmem": 4000, "maxdisk": 100, "lock": "backup"}
        ]))
    }

    async fn manager_status() -> Response {
        data(json!({
            "manager_status": {"master_node": "pve1", "node_status": {"pve1": "online", "pve2": "unknown"}},
            "lrm_status": {"pve1": {"mode": "active", "state": "active", "timestamp": 0}}
        }))
    }

    async fn included_volumes(Path(_id): Path<String>) -> Response {
        data(json!({"children": [
            {"id": 100, "name": "router-vm", "type": "qemu", "children": [
                {"id": "scsi0", "name": "local-lvm:vm-100-disk-0", "included": true, "reason": "because"},
                {"id": "scsi1", "name": "local-lvm:vm-100-disk-1", "included": false, "reason": "disk excluded from backup"},
                {"id": "ide2", "name": "local:iso/debian.iso", "included": false, "reason": "CD-ROM"}
            ]}
        ]}))
    }

    async fn export(State(calls): State<Arc<Calls>>) -> Response {
        calls.export.fetch_add(1, Ordering::Relaxed);
        data(json!({"data": [
            {"id": "node/pve1", "metric": "pressure.io.some.avg10", "timestamp": 1_789_510_600i64,
             "type": "gauge", "value": 3.5}
        ]}))
    }

    async fn node_status(Path(node): Path<String>) -> Response {
        if node == "pve2" {
            return node_down();
        }
        data(json!({"uptime": 1000, "cpu": 0.04, "memory": {"total": 100, "used": 50}}))
    }

    async fn qemu_list(State(calls): State<Arc<Calls>>) -> Response {
        calls.qemu_list.fetch_add(1, Ordering::Relaxed);
        data(json!([{"vmid": 100, "name": "router-vm", "status": "running", "maxmem": 2000}]))
    }

    async fn services() -> Response {
        data(json!([
            {"service": "pveproxy", "state": "running", "active-state": "active", "unit-state": "enabled"},
            {"service": "pvestatd", "state": "dead", "active-state": "failed", "unit-state": "enabled"}
        ]))
    }

    async fn network() -> Response {
        data(json!([
            {"iface": "lo", "type": "loopback", "active": 1, "exists": 1, "autostart": 1},
            {"iface": "vmbr0", "type": "bridge", "active": 1, "exists": 1, "autostart": 1},
            {"iface": "vmbr1", "type": "bridge", "active": 0, "exists": 1, "autostart": 1}
        ]))
    }

    async fn thinpools() -> Response {
        data(json!([{"lv": "data", "vg": "pve", "lv_size": 1000, "used": 960,
                     "metadata_size": 100, "metadata_used": 88}]))
    }

    async fn osinfo(State(calls): State<Arc<Calls>>) -> Response {
        calls.osinfo.fetch_add(1, Ordering::Relaxed);
        data(json!({"result": {"id": "debian", "pretty-name": "Debian GNU/Linux 12 (bookworm)",
                               "version-id": "12", "kernel-release": "6.1.0-18-amd64"}}))
    }

    fn cible(address: String, tags: &[(&str, &str)]) -> Target {
        Target {
            id: 42,
            name: "pve".into(),
            address,
            kind: "proxmox".into(),
            profile_id: None,
            parent_id: None,
            interval: Duration::from_secs(60),
            enabled: true,
            tags: tags
                .iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                .collect::<BTreeMap<_, _>>(),
            credential: Credential::ApiToken {
                token: "monitoring@pve!dumbmonit=8f3a1c9e-dead-beef".into(),
            },
        }
    }

    fn valeur(samples: &[Sample], cle: &str) -> Option<f64> {
        samples.iter().find(|s| s.series_key() == cle).map(|s| s.value)
    }

    #[tokio::test]
    async fn une_interrogation_complete_voit_tout_le_cluster() {
        let calls = Arc::new(Calls::default());
        let address = serve(calls.clone()).await;
        let collector = ProxmoxCollector::new();
        let samples = collector.probe(&cible(address, &[])).await.unwrap();

        // L'invité du nœud injoignable est là, avec sa taille et sans mesure.
        let ct = r#"{name="nextcloud",node="pve2",type="lxc",vmid="202"}"#;
        assert_eq!(valeur(&samples, &format!("proxmox_guest_running{ct}")), Some(0.0));
        assert_eq!(
            valeur(&samples, &format!("proxmox_guest_memory_total_bytes{ct}")),
            Some(4000.0)
        );
        assert_eq!(
            valeur(
                &samples,
                r#"proxmox_guest_locked{lock="backup",name="nextcloud",node="pve2",type="lxc",vmid="202"}"#
            ),
            Some(1.0)
        );
        assert_eq!(valeur(&samples, "proxmox_cluster_guests_total"), Some(2.0));
        assert_eq!(valeur(&samples, r#"proxmox_pool_guests{pool="production"}"#), Some(1.0));
        assert_eq!(valeur(&samples, r#"proxmox_node_up{node="pve2"}"#), Some(0.0));

        // L'inventaire du cluster a répondu : les listes par nœud ne partent pas.
        assert_eq!(calls.qemu_list.load(Ordering::Relaxed), 0);

        // Profondeur des nœuds.
        assert_eq!(valeur(&samples, r#"proxmox_node_core_services_down{node="pve1"}"#), Some(1.0));
        assert_eq!(valeur(&samples, r#"proxmox_node_interfaces_offline{node="pve1"}"#), Some(1.0));
        assert_eq!(
            valeur(
                &samples,
                r#"proxmox_node_thinpool_used_percent{node="pve1",pool="data",vg="pve"}"#
            ),
            Some(96.0)
        );
        assert_eq!(
            valeur(
                &samples,
                r#"proxmox_node_pve_version_info{node="pve1",release="8.2",version="8.2.4"}"#
            ),
            Some(1.0)
        );
        assert_eq!(
            valeur(
                &samples,
                r#"proxmox_guest_netdev_in_bytes{dev="tap100i0",node="pve1",vmid="100"}"#
            ),
            Some(42.0)
        );

        // Haute disponibilité, vue du gestionnaire.
        assert_eq!(valeur(&samples, r#"proxmox_ha_node_online{node="pve2"}"#), Some(0.0));
        assert_eq!(valeur(&samples, r#"proxmox_ha_master_info{node="pve1"}"#), Some(1.0));

        // Couverture réelle du travail de sauvegarde : le disque de données
        // exclu est signalé, le lecteur de CD non.
        assert_eq!(
            valeur(
                &samples,
                r#"proxmox_backup_job_guest_excluded_volumes{job="job-1",name="router-vm",reason="disk excluded from backup",type="qemu",vmid="100"}"#
            ),
            Some(1.0)
        );
        assert_eq!(
            valeur(&samples, r#"proxmox_backup_job_volumes_excluded{job="job-1"}"#),
            Some(2.0)
        );

        // Système et adresse, vus de l'intérieur.
        let vm = r#"{name="router-vm",node="pve1",type="qemu",vmid="100"}"#;
        assert_eq!(
            valeur(
                &samples,
                r#"proxmox_guest_os_info{kernel="6.1.0-18-amd64",name="router-vm",node="pve1",os="Debian GNU/Linux 12 (bookworm)",os_id="debian",os_version="12",type="qemu",vmid="100"}"#
            ),
            Some(1.0)
        );
        assert!(
            samples.iter().any(|s| s.series_key()
                == r#"proxmox_guest_ip_info{iface="ens18",ip="192.168.10.50",name="router-vm",node="pve1",type="qemu",vmid="100"}"#),
            "l'adresse routable doit être publiée, pas la boucle locale"
        );
        assert!(valeur(&samples, &format!("proxmox_guest_running{vm}")).is_some());

        // Ceph absent, LVM interdit, la moitié des endpoints en 501 : rien de
        // tout cela ne doit compter comme une erreur de collecte.
        assert_eq!(valeur(&samples, "proxmox_scrape_errors"), Some(0.0));
        assert!(!samples.iter().any(|s| s.metric.starts_with("proxmox_ceph_")));
        assert!(!samples.iter().any(|s| s.metric.starts_with("proxmox_node_lvm_")));

        // Le flux RRD ne part pas tant qu'on ne l'a pas demandé.
        assert_eq!(calls.export.load(Ordering::Relaxed), 0);
        assert!(!samples.iter().any(|s| s.metric.contains("_rrd_")));
    }

    #[tokio::test]
    async fn les_faits_dun_invite_ne_sont_demandes_quune_fois() {
        let calls = Arc::new(Calls::default());
        let address = serve(calls.clone()).await;
        let collector = ProxmoxCollector::new();
        let target = cible(address, &[]);

        let premiere = collector.probe(&target).await.unwrap();
        let seconde = collector.probe(&target).await.unwrap();

        assert_eq!(
            calls.osinfo.load(Ordering::Relaxed),
            1,
            "le système est relu une fois par heure"
        );
        let cle = |samples: &[Sample]| {
            samples.iter().filter(|s| s.metric == "proxmox_guest_os_info").count()
        };
        assert_eq!(cle(&premiere), 1);
        assert_eq!(cle(&seconde), 1, "la série est republiée depuis la mémoire");
    }

    #[tokio::test]
    async fn les_options_coupees_nenvoient_aucun_appel() {
        let calls = Arc::new(Calls::default());
        let address = serve(calls.clone()).await;
        let collector = ProxmoxCollector::new();
        let target = cible(
            address,
            &[
                ("cluster_resources", "false"),
                ("services", "false"),
                ("network", "false"),
                ("lvm", "false"),
                ("guest_os", "false"),
                ("metrics_export", "true"),
            ],
        );
        let samples = collector.probe(&target).await.unwrap();

        // Sans inventaire du cluster, on retombe sur les listes par nœud — et
        // l'invité du nœud injoignable disparaît, ce qui est exactement le
        // défaut que `/cluster/resources` corrige.
        assert_eq!(calls.qemu_list.load(Ordering::Relaxed), 1);
        assert!(!samples.iter().any(|s| s.labels.get("vmid").is_some_and(|id| id == "202")));
        assert!(!samples.iter().any(|s| s.metric.starts_with("proxmox_node_service_")));
        assert!(!samples.iter().any(|s| s.metric.starts_with("proxmox_node_interface_")));
        assert!(!samples.iter().any(|s| s.metric == "proxmox_guest_os_info"));

        // Le flux RRD, lui, a été demandé et publié sous son nom d'origine.
        assert_eq!(calls.export.load(Ordering::Relaxed), 1);
        assert_eq!(
            valeur(&samples, r#"proxmox_node_rrd_pressure_io_some_avg10{node="pve1"}"#),
            Some(3.5)
        );
    }
}
