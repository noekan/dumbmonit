//! Collecteur Proxmox Backup Server.
//!
//! Interroge l'API REST d'un serveur de sauvegarde PBS et en tire l'état du
//! nœud, le remplissage des datastores, l'ancienneté et l'état de vérification
//! de la dernière sauvegarde de chaque machine, les tâches en échec, l'état des
//! travaux planifiés (synchronisation, vérification, purge) et les mises à jour
//! en attente.
//!
//! # Principes
//!
//! Les mêmes que pour Proxmox VE :
//!
//! * **Une panne partielle reste une collecte réussie.** Un datastore dont le
//!   disque est débranché produit `pbs_datastore_available = 0` et les autres
//!   livrent leurs métriques. Seul un échec sur `/version` — l'API ne répond pas
//!   ou refuse l'authentification — fait échouer l'interrogation.
//! * **Les erreurs sont classées pour l'alerting.** Un jeton invalide donne
//!   `ProbeError::Auth`, jamais « équipement hors ligne ».
//! * **Aucun secret ne sort d'ici.** Ni jeton, ni ticket, ni mot de passe
//!   n'apparaît dans un journal, un message d'erreur ou une sortie `Debug`.
//! * **La cardinalité est bornée.** Un PBS mutualisé héberge vite des milliers de
//!   groupes de sauvegarde ; `max_groups` plafonne le nombre de séries, et les
//!   appels par datastore sont limités à quelques-uns en parallèle.
//! * **Un privilège facultatif ne manque pas bruyamment.** Les listes de travaux
//!   et les mises à jour demandent des droits que le minimum documenté ne donne
//!   pas ; un 403 sur ces appels ne produit ni série, ni erreur de collecte.
//!
//! # Réglages, portés par les étiquettes de la cible
//!
//! | Étiquette | Défaut | Rôle |
//! |---|---|---|
//! | `insecure_tls` | `false` | Accepte un certificat non vérifiable (auto-signé). |
//! | `port` | `8007` | Port de l'API, si l'adresse n'en précise pas. |
//! | `request_timeout_seconds` | `15` | Délai par requête HTTP. |
//! | `task_lookback_hours` | `24` | Fenêtre d'examen des tâches. |
//! | `datastores` | tous | Restreint la collecte à une liste de datastores. |
//! | `max_groups` | `500` | Plafond de groupes de sauvegarde produisant des séries. |
//! | `jobs` | `true` | Interroge les travaux planifiés (`/admin/sync`, `/admin/verify`, `/admin/prune`). |
//! | `updates` | `true` | Interroge les mises à jour de paquets en attente. |
//! | `disks` | `true` | Interroge les disques physiques et les pools ZFS du nœud. |
//! | `services` | `true` | Interroge les unités systemd du nœud. |
//! | `datastore_details` | `true` | Décomptes par type, opérations en cours et mode de maintenance de chaque datastore. |
//! | `traffic_control` | `true` | Interroge les règles de limitation de débit et leur débit courant. |
//! | `certificates` | `false` | Interroge les certificats — PBS exige `Sys.Modify` pour les lire. |
//! | `tape` | `false` | Interroge l'étage bande : travaux, lecteurs, robotique, pools, médias. |
//!
//! # La vue, au-delà des métriques
//!
//! Le calendrier des sauvegardes de l'interface a besoin des tâches et des
//! instantanés eux-mêmes, pas seulement de séries. Chaque interrogation réussie
//! livre donc une [`ProbeView`] à l'observateur enregistré par
//! [`PbsCollector::with_observer`] — côté serveur, il la range en base. À la
//! première interrogation d'une cible, la liste des tâches couvre trente jours
//! pour reconstituer l'historique ; ensuite, la fenêtre d'examen suffit. Le
//! journal d'une tâche ([`task_log`]) et le détail SMART d'un disque
//! ([`disk_smart`]) se demandent à part : ils ne sont jamais lus par la sonde.

mod auth;
mod backup;
mod client;
mod jobs;
mod metrics;
mod model;
mod node;
mod options;
mod tape;
mod view;

pub use view::{
    CertificateView, DatastoreView, DiskSmart, DiskView, GcView, GroupView, HISTORY_DAYS, JobView,
    MediaPoolView, PackageView, ProbeObserver, ProbeView, ServiceView, SnapshotView,
    TapeChangerView, TapeDriveView, TapeJobView, TapeMediaView, TapeView, TaskLog, TaskView,
    TrafficRuleView, TypeCountView, ZpoolView,
};

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use dumbmonit_proto::{Collector, Credential, MetricKind, ProbeError, Sample, Target, TargetId};
use serde::de::DeserializeOwned;
use tokio::sync::Semaphore;
use tracing::{debug, warn};

use auth::{AuthMode, Ticket};
use backup::{GroupKey, GroupSummary};
use client::PbsClient;
use jobs::JobKind;
use model::{
    ActiveOperations, AptUpdate, CertificateInfo, DatastoreConfig, DatastoreStatus, DatastoreUsage,
    DiskEntry, GcStatus, JobEntry, MediaPool, NamespaceEntry, NodeStatus, PackageVersion,
    ServiceEntry, SmartData, SnapshotEntry, TapeBackupJob, TapeChanger, TapeDrive, TapeMedia,
    TaskEntry, TaskLogLine, TrafficRule, ZpoolEntry,
};
use options::Options;

/// Identifiant de profil renvoyé par la découverte.
const PROFILE_ID: &str = "proxmox-backup-server";

/// Nombre maximal d'appels simultanés vers les datastores.
///
/// Lister les instantanés lit les index sur le disque du datastore : lancer dix
/// listings de front sur des disques mécaniques les ralentirait tous, et
/// ralentirait la sauvegarde en cours par la même occasion.
const DATASTORE_CONCURRENCY: usize = 4;

/// Nombre maximal de tâches ramenées quand l'historique est reconstitué.
///
/// Trente nuits d'une centaine de machines, avec leurs vérifications, purges et
/// GC : quelques milliers. Au-delà, le calendrier perd les jours les plus
/// anciens, ce qui vaut mieux qu'une réponse de plusieurs dizaines de Mo.
const BACKFILL_TASK_LIMIT: u32 = 5000;

/// Nombre maximal de lignes de journal servies pour une tâche.
const MAX_LOG_LINES: usize = 500;

#[derive(Default)]
pub struct PbsCollector {
    /// Tickets en cache, un par cible, pour ne pas ouvrir une session — que PBS
    /// journalise — à chaque interrogation.
    tickets: Mutex<HashMap<TargetId, Arc<tokio::sync::Mutex<Option<Ticket>>>>>,
    /// Destinataire de la vue de chaque interrogation ; sans lui, la sonde ne
    /// produit que des métriques et ne reconstitue pas l'historique.
    observer: Option<Arc<dyn ProbeObserver>>,
    /// Cibles dont l'historique a déjà été reconstitué depuis le démarrage.
    backfilled: Mutex<HashSet<TargetId>>,
}

impl PbsCollector {
    pub fn new() -> Self {
        Self::default()
    }

    /// Enregistre le destinataire des vues d'interrogation.
    pub fn with_observer(mut self, observer: Arc<dyn ProbeObserver>) -> Self {
        self.observer = Some(observer);
        self
    }

    /// Vrai une seule fois par cible et par démarrage : à cette interrogation, la
    /// liste des tâches couvre l'historique entier. Sans observateur, personne ne
    /// le conserverait : la fenêtre ordinaire suffit.
    fn wants_backfill(&self, id: TargetId) -> bool {
        if self.observer.is_none() {
            return false;
        }
        let mut done = self.backfilled.lock().unwrap_or_else(|poison| poison.into_inner());
        done.insert(id)
    }

    /// Oublie la reconstitution : l'interrogation qui la portait a échoué, la
    /// prochaine la refera.
    fn forget_backfill(&self, id: TargetId) {
        let mut done = self.backfilled.lock().unwrap_or_else(|poison| poison.into_inner());
        done.remove(&id);
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
                "Proxmox Backup Server expects an API token or a username / password pair, \
                 configured credential: {other}"
            ))),
        }
    }

    fn ticket_slot(&self, id: TargetId) -> Arc<tokio::sync::Mutex<Option<Ticket>>> {
        let mut cache = self.tickets.lock().unwrap_or_else(|poison| poison.into_inner());
        cache.entry(id).or_default().clone()
    }

    fn client(&self, target: &Target, options: &Options) -> Result<PbsClient, ProbeError> {
        Ok(PbsClient::new(
            crate::http::client(options.insecure_tls)?,
            options.base_url.clone(),
            self.auth_mode(target)?,
            options.request_timeout,
        ))
    }
}

#[async_trait]
impl Collector for PbsCollector {
    fn kind(&self) -> &'static str {
        "pbs"
    }

    async fn probe(&self, target: &Target) -> Result<Vec<Sample>, ProbeError> {
        let options = Options::from_target(target)?;
        let pbs = self.client(target, &options)?;

        let started = std::time::Instant::now();
        let now = chrono::Utc::now();
        let (now_s, ts_ms) = (now.timestamp(), now.timestamp_millis());

        // `/version` sert de sonde de vie et d'authentification : c'est le seul
        // appel dont l'échec condamne l'interrogation entière.
        let version: model::Version = pbs.get("/version", &[]).await?;
        let mut samples = metrics::version_samples(&version, ts_ms);
        samples.push(Sample::new("pbs_up", 1.0, MetricKind::Gauge, ts_ms));
        let mut errors = 0u32;

        let mut view =
            ProbeView { probed_at: now_s, version: version.version.clone(), ..Default::default() };

        // À la première interrogation, la liste des tâches remonte trente jours
        // pour que le calendrier ne parte pas d'une page blanche ; ensuite la
        // fenêtre d'examen suffit, l'observateur garde le reste.
        let backfill = self.wants_backfill(target.id);
        let tasks_query = if backfill {
            [
                ("limit", BACKFILL_TASK_LIMIT.to_string()),
                ("since", (now_s - HISTORY_DAYS * 86_400).to_string()),
            ]
        } else {
            [
                ("limit", options.task_limit.to_string()),
                ("since", (now_s - options.task_lookback_seconds).to_string()),
            ]
        };
        // Les inventaires sont indépendants : les enchaîner multiplierait
        // d'autant le temps passé sur un serveur lent.
        let (
            node,
            usage,
            tasks,
            sync_jobs,
            verify_jobs,
            prune_jobs,
            updates,
            disks,
            zpools,
            services,
            packages,
            certificates,
            traffic,
            gc_list,
            store_configs,
            tape_inventory,
        ) = futures::join!(
            pbs.get::<NodeStatus>("/nodes/localhost/status", &[]),
            pbs.get::<Vec<DatastoreUsage>>("/status/datastore-usage", &[]),
            pbs.get::<Vec<TaskEntry>>("/nodes/localhost/tasks", &tasks_query),
            optional_list::<JobEntry>(&pbs, options.jobs, JobKind::Sync.path()),
            optional_list::<JobEntry>(&pbs, options.jobs, JobKind::Verify.path()),
            optional_list::<JobEntry>(&pbs, options.jobs, JobKind::Prune.path()),
            optional_list::<AptUpdate>(&pbs, options.updates, "/nodes/localhost/apt/update"),
            optional_list::<DiskEntry>(&pbs, options.disks, "/nodes/localhost/disks/list"),
            optional_list::<ZpoolEntry>(&pbs, options.disks, "/nodes/localhost/disks/zfs"),
            optional_list::<ServiceEntry>(&pbs, options.services, "/nodes/localhost/services"),
            optional_list::<PackageVersion>(&pbs, options.updates, "/nodes/localhost/apt/versions"),
            optional_list::<CertificateInfo>(
                &pbs,
                options.certificates,
                "/nodes/localhost/certificates/info"
            ),
            optional_list::<TrafficRule>(&pbs, options.traffic_control, "/admin/traffic-control"),
            // Un seul appel pour la GC de tous les datastores, là où il en
            // fallait un par datastore — et il porte en plus la durée, la
            // planification et le compte de chunks illisibles.
            optional_list::<GcStatus>(&pbs, true, "/admin/gc"),
            optional_list::<DatastoreConfig>(&pbs, options.datastore_details, "/config/datastore"),
            collect_tape(&pbs, options.tape),
        );

        match node {
            Ok(status) => samples.extend(metrics::node_samples(&status, ts_ms)),
            Err(error) => {
                errors += 1;
                warn!(target_id = target.id, %error, "état du nœud PBS indisponible");
            }
        }

        let digest = match tasks {
            Ok(tasks) => {
                let digest = backup::digest_tasks(&tasks, now_s, options.task_lookback_seconds);
                view.tasks = backup::task_views(&tasks);
                digest
            }
            Err(error) => {
                errors += 1;
                if backfill {
                    self.forget_backfill(target.id);
                }
                warn!(target_id = target.id, %error, "historique des tâches PBS indisponible");
                backup::TaskDigest::default()
            }
        };
        samples.extend(backup::task_samples(&digest, ts_ms));

        for (kind, outcome) in [
            (JobKind::Sync, sync_jobs),
            (JobKind::Verify, verify_jobs),
            (JobKind::Prune, prune_jobs),
        ] {
            if let Some(list) = settle(outcome, &mut errors, target.id, kind.path()) {
                samples.extend(jobs::job_samples(kind, &list, now_s, ts_ms));
                view.jobs.extend(jobs::job_views(kind, &list));
            }
        }
        if let Some(list) = settle(updates, &mut errors, target.id, "/nodes/localhost/apt/update") {
            samples.extend(jobs::updates_samples(&list, ts_ms));
        }
        if let Some(list) = settle(services, &mut errors, target.id, "/nodes/localhost/services") {
            samples.extend(node::service_samples(&list, ts_ms));
            view.services = node::service_views(&list);
        }
        if let Some(list) =
            settle(packages, &mut errors, target.id, "/nodes/localhost/apt/versions")
        {
            samples.extend(node::package_samples(&list, ts_ms));
            view.packages = node::package_views(&list);
        }
        // Les certificats et les règles de débit sont facultatifs : l'appel est
        // désactivé, ou le privilège manque. Dans les deux cas, rien à compter.
        if let Some(Ok(list)) = certificates {
            samples.extend(node::certificate_samples(&list, now_s, ts_ms));
            view.certificates = node::certificate_views(&list);
        }
        if let Some(list) = settle(traffic, &mut errors, target.id, "/admin/traffic-control") {
            samples.extend(node::traffic_samples(&list, ts_ms));
            view.traffic = node::traffic_views(&list);
        }
        if let Some(inventory) = tape_inventory {
            samples.extend(tape::tape_samples(&inventory, now_s, ts_ms));
            view.tape = tape::tape_view(&inventory);
        }

        // La GC de tous les datastores en un appel, indexée par datastore : ce
        // qu'il faut pour que chaque datastore n'ait plus à la demander.
        let gc_by_store: BTreeMap<String, GcStatus> = match gc_list {
            Some(Ok(list)) => list
                .into_iter()
                .filter_map(|status| Some((status.store.clone()?, status)))
                .collect(),
            // Un PBS trop ancien pour `/admin/gc`, ou un privilège absent : le
            // repli par datastore prend le relais, ce n'est pas une erreur.
            Some(Err(error)) => {
                debug!(target_id = target.id, %error, "GC globale indisponible, repli par datastore");
                BTreeMap::new()
            }
            None => BTreeMap::new(),
        };

        let maintenance_by_store: BTreeMap<String, String> =
            match settle(store_configs, &mut errors, target.id, "/config/datastore") {
                Some(configs) => {
                    samples.extend(metrics::datastore_maintenance_samples(&configs, ts_ms));
                    configs
                        .iter()
                        .filter_map(|config| {
                            Some((config.name.clone(), config.maintenance_kind()?))
                        })
                        .collect()
                }
                None => BTreeMap::new(),
            };
        // Les disques et les pools sont facultatifs deux fois : un PBS sans ZFS
        // répond une liste vide ou une erreur — ni l'une ni l'autre n'est un
        // défaut de collecte.
        if let Some(list) = settle(disks, &mut errors, target.id, "/nodes/localhost/disks/list") {
            samples.extend(metrics::disk_samples(&list, ts_ms));
            view.disks = metrics::disk_views(&list);
        }
        if let Some(Ok(list)) = zpools {
            samples.extend(metrics::zpool_samples(&list, ts_ms));
            view.zpools = metrics::zpool_views(&list);
        }

        let mut gc_from_status = BTreeMap::new();
        let mut gc_state_known = BTreeSet::new();
        match usage {
            Ok(usages) => {
                let permits = Semaphore::new(DATASTORE_CONCURRENCY);
                let mut groups = BTreeMap::new();
                let mut listed = Vec::new();

                let selected: Vec<&DatastoreUsage> =
                    usages.iter().filter(|u| options.wants_datastore(&u.store)).collect();
                for usage in &selected {
                    samples.extend(metrics::datastore_samples(usage, now_s, ts_ms));
                    view.datastores.push(metrics::datastore_view(usage, now_s));
                }

                // Un datastore en erreur ne sera pas interrogé : chaque appel
                // échouerait et attendrait le délai complet pour rien.
                let outcomes = futures::future::join_all(
                    selected.iter().filter(|usage| usage.is_available()).map(|usage| {
                        collect_datastore(
                            &pbs,
                            &usage.store,
                            &permits,
                            gc_by_store.get(&usage.store),
                            options.datastore_details,
                            ts_ms,
                        )
                    }),
                )
                .await;

                for outcome in outcomes {
                    errors += outcome.errors;
                    samples.extend(outcome.samples);
                    groups.extend(outcome.groups);
                    listed.extend(
                        outcome.namespaces.into_iter().map(|ns| (outcome.store.clone(), ns)),
                    );
                    if let Some(date) = outcome.gc_last_success {
                        gc_from_status.insert(outcome.store.clone(), date);
                    }
                    if let Some(entry) =
                        view.datastores.iter_mut().find(|d| d.name == outcome.store)
                    {
                        entry.dedup_factor = outcome.dedup_factor;
                        if outcome.gc.as_ref().is_some_and(|gc| gc.last_run_state.is_some()) {
                            gc_state_known.insert(outcome.store.clone());
                        }
                        entry.gc = outcome.gc;
                        entry.counts = outcome.counts;
                        entry.active_reads = outcome.active_reads;
                        entry.active_writes = outcome.active_writes;
                        entry.maintenance = maintenance_by_store.get(&outcome.store).cloned();
                    }
                }

                samples.extend(backup::group_samples(&groups, options.max_groups, now_s, ts_ms));
                samples.extend(backup::namespace_samples(&listed, &groups, ts_ms));
                view.groups = backup::group_views(&groups);
            }
            Err(error) => {
                errors += 1;
                warn!(target_id = target.id, %error, "occupation des datastores PBS indisponible");
            }
        }

        samples.extend(backup::maintenance_samples(&digest, &gc_from_status, now_s, ts_ms));
        samples.extend(backup::gc_outcome_samples(&digest, &gc_state_known, ts_ms));
        view.jobs.extend(gc_jobs(&view.datastores, &digest));

        samples.push(Sample::new("pbs_scrape_errors", f64::from(errors), MetricKind::Gauge, ts_ms));
        samples.push(Sample::new(
            "pbs_scrape_duration_seconds",
            started.elapsed().as_secs_f64(),
            MetricKind::Gauge,
            ts_ms,
        ));

        if let Some(observer) = &self.observer {
            observer.observe(target, &view).await;
        }
        Ok(samples)
    }

    async fn discover(&self, target: &Target) -> Result<Option<String>, ProbeError> {
        let options = Options::from_target(target)?;
        let pbs = self.client(target, &options)?;

        let version: model::Version = pbs.get("/version", &[]).await?;
        debug!(
            target_id = target.id,
            version = version.version.as_deref().unwrap_or("inconnue"),
            "Proxmox Backup Server détecté"
        );
        Ok(Some(PROFILE_ID.to_string()))
    }
}

/// Les dernières lignes du journal d'une tâche, demandées à part.
///
/// Deux appels : le premier ne sert qu'à connaître la longueur du journal, le
/// second ramène la fin — un journal de sauvegarde complet pèse vite des
/// milliers de lignes, et seule la fin dit pourquoi la tâche a échoué.
pub async fn task_log(target: &Target, upid: &str, lines: usize) -> Result<TaskLog, ProbeError> {
    let lines = lines.clamp(1, MAX_LOG_LINES);
    let options = Options::from_target(target)?;
    let pbs = PbsCollector::new().client(target, &options)?;
    // Un UPID porte des `:` et, pour l'identifiant du travail, des `\x3a` :
    // laissés tels quels dans une URL, l'analyseur les prendrait pour des
    // barres obliques.
    let path = format!("/nodes/localhost/tasks/{}/log", percent_encode(upid));

    let probe = pbs
        .get_envelope::<Vec<TaskLogLine>>(&path, &[("start", "0".into()), ("limit", "1".into())])
        .await?;
    let total = probe.total.map_or(probe.data.len(), |t| t.0.max(0.0) as usize);
    let start = total.saturating_sub(lines);
    let page = if start == 0 && total <= 1 {
        probe.data
    } else {
        pbs.get::<Vec<TaskLogLine>>(
            &path,
            &[("start", start.to_string()), ("limit", lines.to_string())],
        )
        .await?
    };

    Ok(TaskLog {
        upid: upid.to_string(),
        total,
        lines: page.into_iter().filter_map(|line| line.t).collect(),
    })
}

/// Encode un segment de chemin : tout sauf les caractères non réservés.
fn percent_encode(segment: &str) -> String {
    let mut out = String::with_capacity(segment.len() * 3);
    for byte in segment.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// Le détail SMART d'un disque, demandé à part : smartctl est lancé sur le
/// serveur à chaque appel, ce n'est pas une chose à faire à chaque sonde.
pub async fn disk_smart(target: &Target, disk: &str) -> Result<DiskSmart, ProbeError> {
    let options = Options::from_target(target)?;
    let pbs = PbsCollector::new().client(target, &options)?;
    let data: SmartData =
        pbs.get("/nodes/localhost/disks/smart", &[("disk", disk.to_string())]).await?;
    Ok(DiskSmart {
        disk: disk.to_string(),
        health: data.health,
        wearout_percent: data
            .wearout
            .map(|w| w.0)
            .filter(|w| (0.0..=100.0).contains(w))
            .map(|w| 100.0 - w),
        kind: data.kind,
        attributes: data.attributes,
        text: data.text,
    })
}

/// La GC de chaque datastore, présentée comme un travail planifié de plus
/// (`kind = "gc"`) : c'est ainsi que PBS la range depuis la 3.3, et c'est ce que
/// l'interface montre à côté des synchronisations et des purges. Avant la 3.3,
/// `/gc` ne dit pas l'issue du dernier passage : la tâche la plus récente de
/// la fenêtre le remplace.
fn gc_jobs(datastores: &[DatastoreView], digest: &backup::TaskDigest) -> Vec<JobView> {
    datastores
        .iter()
        .filter_map(|store| {
            let gc = store.gc.as_ref()?;
            let from_task = digest.last_gc_outcome.get(&store.name);
            let (state, end) = match (&gc.last_run_state, from_task) {
                (Some(state), _) => (Some(state.clone()), gc.last_run_end),
                (None, Some((start, ok))) => (
                    Some(if *ok {
                        "OK".to_string()
                    } else {
                        "TASK ERROR: see the task log".to_string()
                    }),
                    Some(*start),
                ),
                (None, None) => (None, gc.last_run_end),
            };
            Some(JobView {
                kind: "gc".to_string(),
                id: store.name.clone(),
                datastore: store.name.clone(),
                namespace: None,
                remote: None,
                enabled: true,
                schedule: gc.schedule.clone(),
                comment: None,
                retention: None,
                next_run: gc.next_run,
                last_run_state: state,
                last_run_end: end,
                last_run_upid: gc.last_run_upid.clone(),
            })
        })
        .collect()
}

/// Une liste facultative : `None` si l'option est désactivée ou si le serveur
/// refuse l'accès — privilège facultatif, absent du minimum documenté —, sinon
/// le résultat de l'appel, erreurs comprises.
async fn optional_list<T: DeserializeOwned>(
    pbs: &PbsClient,
    enabled: bool,
    path: &'static str,
) -> Option<Result<Vec<T>, ProbeError>> {
    if !enabled {
        return None;
    }
    match pbs.get_optional::<Vec<T>>(path, &[]).await {
        Ok(Some(list)) => Some(Ok(list)),
        Ok(None) => {
            debug!(path, "accès refusé : privilège facultatif absent, aucune série");
            None
        }
        Err(error) => Some(Err(error)),
    }
}

/// Dépouille le résultat d'une liste facultative : une erreur — autre qu'un
/// refus d'accès, déjà absorbé — compte dans `pbs_scrape_errors` comme pour
/// n'importe quel inventaire.
fn settle<T>(
    outcome: Option<Result<Vec<T>, ProbeError>>,
    errors: &mut u32,
    target_id: TargetId,
    path: &str,
) -> Option<Vec<T>> {
    match outcome? {
        Ok(list) => Some(list),
        Err(error) => {
            *errors += 1;
            warn!(target_id, path, %error, "liste PBS indisponible");
            None
        }
    }
}

/// L'étage bande, ou `None` quand l'option est fermée.
///
/// Les cinq appels sont indulgents jusqu'au bout : un PBS sans support de
/// bande, un `tape.cfg` illisible ou un privilège `Tape.Audit` absent répondent
/// une erreur, et aucune de ces trois situations n'est un défaut de collecte.
/// On ne renvoie donc que ce qui a abouti — rien, le plus souvent.
async fn collect_tape(pbs: &PbsClient, enabled: bool) -> Option<tape::Tape> {
    if !enabled {
        return None;
    }
    async fn list<T: DeserializeOwned>(pbs: &PbsClient, path: &str) -> Vec<T> {
        match pbs.get::<Vec<T>>(path, &[]).await {
            Ok(list) => list,
            Err(error) => {
                debug!(path, %error, "étage bande non interrogeable, ignoré");
                Vec::new()
            }
        }
    }
    let (jobs, drives, changers, pools, media) = futures::join!(
        list::<TapeBackupJob>(pbs, "/tape/backup"),
        list::<TapeDrive>(pbs, "/tape/drive"),
        list::<TapeChanger>(pbs, "/tape/changer"),
        list::<MediaPool>(pbs, "/config/media-pool"),
        list::<TapeMedia>(pbs, "/tape/media/list"),
    );
    Some(tape::Tape { jobs, drives, changers, pools, media })
}

/// Ce qu'un datastore a livré.
#[derive(Default)]
struct DatastoreOutcome {
    store: String,
    samples: Vec<Sample>,
    groups: BTreeMap<GroupKey, GroupSummary>,
    /// Espaces de noms dont le listing d'instantanés a abouti.
    namespaces: Vec<String>,
    /// Date de la dernière GC réussie d'après `/gc`, en secondes Unix.
    gc_last_success: Option<i64>,
    gc: Option<GcView>,
    dedup_factor: Option<f64>,
    /// Groupes et instantanés par type de sauvegarde.
    counts: Vec<view::TypeCountView>,
    active_reads: Option<f64>,
    active_writes: Option<f64>,
    errors: u32,
}

/// Interroge un datastore. Ne renvoie jamais d'erreur : un datastore qui ne
/// répond pas se traduit par un compteur d'erreurs, jamais par l'abandon des
/// autres.
///
/// Chaque appel prend un jeton du sémaphore, jamais plus d'un à la fois : c'est le
/// nombre d'appels *en vol* que l'on borne, pas le nombre de datastores traités.
async fn collect_datastore(
    pbs: &PbsClient,
    store: &str,
    permits: &Semaphore,
    gc: Option<&GcStatus>,
    details: bool,
    ts_ms: i64,
) -> DatastoreOutcome {
    let mut outcome = DatastoreOutcome { store: store.to_string(), ..Default::default() };

    // `/admin/gc` a déjà tout dit pour ce datastore : inutile de le redemander.
    // Sinon — vieux PBS, privilège absent — le statut par datastore prend le
    // relais, exactement comme avant.
    let fetched;
    let status = match gc {
        Some(status) => Some(status),
        None => {
            let _permit = permits.acquire().await.ok();
            match pbs.get::<GcStatus>(&format!("/admin/datastore/{store}/gc"), &[]).await {
                Ok(status) => {
                    fetched = status;
                    Some(&fetched)
                }
                Err(error) => {
                    outcome.errors += 1;
                    warn!(datastore = store, %error, "statut de GC indisponible");
                    None
                }
            }
        }
    };
    if let Some(status) = status {
        outcome.samples.extend(metrics::gc_samples(store, status, ts_ms));
        outcome.gc_last_success = metrics::gc_last_success(status);
        outcome.dedup_factor = metrics::dedup_factor(status);
        outcome.gc = Some(metrics::gc_view(status));
    }

    // Décomptes et opérations en cours : deux appels légers, une option pour
    // les fermer sur un serveur que l'on veut laisser tranquille.
    if details {
        let _permit = permits.acquire().await.ok();
        // Sans `verbose`, PBS ne renvoie que les tailles, déjà connues.
        match pbs
            .get::<DatastoreStatus>(
                &format!("/admin/datastore/{store}/status"),
                &[("verbose", "1".to_string())],
            )
            .await
        {
            Ok(status) => {
                outcome.samples.extend(metrics::counts_samples(store, &status, ts_ms));
                outcome.counts = metrics::counts_views(&status);
            }
            Err(error) => {
                debug!(datastore = store, %error, "décomptes du datastore indisponibles");
            }
        }
        match pbs
            .get::<ActiveOperations>(&format!("/admin/datastore/{store}/active-operations"), &[])
            .await
        {
            Ok(operations) => {
                outcome.samples.extend(metrics::active_operations_samples(
                    store,
                    &operations,
                    ts_ms,
                ));
                outcome.active_reads = operations.read.map(|n| n.0);
                outcome.active_writes = operations.write.map(|n| n.0);
            }
            Err(error) => {
                debug!(datastore = store, %error, "opérations en cours indisponibles");
            }
        }
    }

    // Sans espace de noms explicite, l'API ne liste que la racine : les
    // sauvegardes d'un PVE rangé dans son propre espace seraient invisibles.
    let mut namespaces = vec![String::new()];
    {
        let _permit = permits.acquire().await.ok();
        match pbs
            .get::<Vec<NamespaceEntry>>(&format!("/admin/datastore/{store}/namespace"), &[])
            .await
        {
            Ok(entries) => {
                for entry in entries {
                    if !entry.ns.is_empty() && !namespaces.contains(&entry.ns) {
                        namespaces.push(entry.ns);
                    }
                }
            }
            Err(error) => {
                // Un PBS antérieur à 2.2 ne connaît pas les espaces de noms : la
                // racine suffit alors, et ce n'est pas une erreur de collecte.
                debug!(datastore = store, %error, "espaces de noms non listés, racine seule");
            }
        }
    }

    let path = format!("/admin/datastore/{store}/snapshots");
    for namespace in namespaces {
        let query: Vec<(&str, String)> =
            if namespace.is_empty() { Vec::new() } else { vec![("ns", namespace.clone())] };
        let _permit = permits.acquire().await.ok();
        match pbs.get::<Vec<SnapshotEntry>>(&path, &query).await {
            Ok(snapshots) => {
                outcome.groups.extend(backup::summarize_groups(store, &namespace, &snapshots));
                outcome.namespaces.push(namespace);
            }
            Err(error) => {
                outcome.errors += 1;
                warn!(datastore = store, namespace = %namespace, %error, "listing des instantanés échoué");
            }
        }
    }

    outcome
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
            name: "pbs".into(),
            address: "10.0.0.20".into(),
            kind: "pbs".into(),
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
        assert_eq!(PbsCollector::new().kind(), "pbs");
    }

    #[test]
    fn le_jeton_dapi_produit_une_session_sans_ticket() {
        let collector = PbsCollector::new();
        let credential =
            Credential::ApiToken { token: "monitoring@pbs!dumbmonit=8f3a1c9e-dead-beef".into() };
        let mode = collector.auth_mode(&cible(credential)).unwrap();
        assert!(matches!(mode, AuthMode::Token(_)));
    }

    #[test]
    fn un_couple_identifiants_produit_une_session_a_ticket() {
        let collector = PbsCollector::new();
        let credential = Credential::UsernamePassword {
            username: "monitoring@pbs".into(),
            password: "secret".into(),
        };
        let mode = collector.auth_mode(&cible(credential)).unwrap();
        assert!(matches!(mode, AuthMode::Ticket { .. }));
    }

    #[test]
    fn le_cache_de_ticket_est_partage_entre_deux_interrogations_de_la_meme_cible() {
        let collector = PbsCollector::new();
        let premier = collector.ticket_slot(7);
        let second = collector.ticket_slot(7);
        assert!(Arc::ptr_eq(&premier, &second), "le ticket doit survivre à l'interrogation");
        assert!(!Arc::ptr_eq(&premier, &collector.ticket_slot(8)), "une cible, un ticket");
    }

    #[test]
    fn un_identifiant_inadapte_est_refuse_avant_tout_appel_reseau() {
        let collector = PbsCollector::new();
        for credential in
            [Credential::None, Credential::SnmpCommunity { community: "public".into() }]
        {
            let error = collector.auth_mode(&cible(credential)).unwrap_err();
            assert!(matches!(error, ProbeError::Config(_)));
        }
    }

    #[test]
    fn le_message_didentifiant_inadapte_ne_divulgue_pas_le_secret() {
        let collector = PbsCollector::new();
        let credential = Credential::SnmpCommunity { community: "SECRET-COMMUNITY".into() };
        let error = collector.auth_mode(&cible(credential)).unwrap_err();
        assert!(!format!("{error}").contains("SECRET-COMMUNITY"));
    }

    #[test]
    fn le_port_par_defaut_est_celui_de_pbs() {
        let options = Options::from_target(&cible(Credential::None)).unwrap();
        assert_eq!(options.base_url, "https://10.0.0.20:8007");
    }

    #[test]
    fn une_liste_facultative_en_erreur_compte_une_erreur_de_collecte() {
        let mut errors = 0;
        let absent: Option<Vec<u8>> = settle(None, &mut errors, 7, "/admin/sync");
        assert!(absent.is_none());
        assert_eq!(errors, 0, "option désactivée ou 403 : rien à compter");

        let ok = settle(Some(Ok(vec![1u8, 2])), &mut errors, 7, "/admin/sync");
        assert_eq!(ok, Some(vec![1, 2]));
        assert_eq!(errors, 0);

        let failed: Option<Vec<u8>> = settle(
            Some(Err(ProbeError::Unreachable("/admin/prune: 502".into()))),
            &mut errors,
            7,
            "/admin/prune",
        );
        assert!(failed.is_none());
        assert_eq!(errors, 1);
    }
}
