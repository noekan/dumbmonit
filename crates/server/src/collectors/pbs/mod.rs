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

mod auth;
mod backup;
mod client;
mod jobs;
mod metrics;
mod model;
mod options;

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use ezymonit_proto::{Collector, Credential, MetricKind, ProbeError, Sample, Target, TargetId};
use serde::de::DeserializeOwned;
use tokio::sync::Semaphore;
use tracing::{debug, warn};

use auth::{AuthMode, Ticket};
use backup::{GroupKey, GroupSummary};
use client::PbsClient;
use jobs::JobKind;
use model::{
    AptUpdate, DatastoreUsage, GcStatus, JobEntry, NamespaceEntry, NodeStatus, SnapshotEntry,
    TaskEntry,
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

#[derive(Default)]
pub struct PbsCollector {
    /// Tickets en cache, un par cible, pour ne pas ouvrir une session — que PBS
    /// journalise — à chaque interrogation.
    tickets: Mutex<HashMap<TargetId, Arc<tokio::sync::Mutex<Option<Ticket>>>>>,
}

impl PbsCollector {
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
            crate::collectors::http::client(options.insecure_tls)?,
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

        let tasks_query = [
            ("limit", options.task_limit.to_string()),
            ("since", (now_s - options.task_lookback_seconds).to_string()),
        ];
        // Les inventaires sont indépendants : les enchaîner multiplierait
        // d'autant le temps passé sur un serveur lent.
        let (node, usage, tasks, sync_jobs, verify_jobs, prune_jobs, updates) = futures::join!(
            pbs.get::<NodeStatus>("/nodes/localhost/status", &[]),
            pbs.get::<Vec<DatastoreUsage>>("/status/datastore-usage", &[]),
            pbs.get::<Vec<TaskEntry>>("/nodes/localhost/tasks", &tasks_query),
            optional_list::<JobEntry>(&pbs, options.jobs, JobKind::Sync.path()),
            optional_list::<JobEntry>(&pbs, options.jobs, JobKind::Verify.path()),
            optional_list::<JobEntry>(&pbs, options.jobs, JobKind::Prune.path()),
            optional_list::<AptUpdate>(&pbs, options.updates, "/nodes/localhost/apt/update"),
        );

        match node {
            Ok(status) => samples.extend(metrics::node_samples(&status, ts_ms)),
            Err(error) => {
                errors += 1;
                warn!(target_id = target.id, %error, "état du nœud PBS indisponible");
            }
        }

        let digest = match tasks {
            Ok(tasks) => backup::digest_tasks(&tasks, now_s, options.task_lookback_seconds),
            Err(error) => {
                errors += 1;
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
            }
        }
        if let Some(list) = settle(updates, &mut errors, target.id, "/nodes/localhost/apt/update") {
            samples.extend(jobs::updates_samples(&list, ts_ms));
        }

        let mut gc_from_status = BTreeMap::new();
        match usage {
            Ok(usages) => {
                let permits = Semaphore::new(DATASTORE_CONCURRENCY);
                let mut groups = BTreeMap::new();
                let mut listed = Vec::new();

                let selected: Vec<&DatastoreUsage> =
                    usages.iter().filter(|u| options.wants_datastore(&u.store)).collect();
                for usage in &selected {
                    samples.extend(metrics::datastore_samples(usage, now_s, ts_ms));
                }

                // Un datastore en erreur ne sera pas interrogé : chaque appel
                // échouerait et attendrait le délai complet pour rien.
                let outcomes = futures::future::join_all(
                    selected
                        .iter()
                        .filter(|usage| usage.is_available())
                        .map(|usage| collect_datastore(&pbs, &usage.store, &permits, ts_ms)),
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
                        gc_from_status.insert(outcome.store, date);
                    }
                }

                samples.extend(backup::group_samples(&groups, options.max_groups, now_s, ts_ms));
                samples.extend(backup::namespace_samples(&listed, &groups, ts_ms));
            }
            Err(error) => {
                errors += 1;
                warn!(target_id = target.id, %error, "occupation des datastores PBS indisponible");
            }
        }

        samples.extend(backup::maintenance_samples(&digest, &gc_from_status, now_s, ts_ms));

        samples.push(Sample::new("pbs_scrape_errors", f64::from(errors), MetricKind::Gauge, ts_ms));
        samples.push(Sample::new(
            "pbs_scrape_duration_seconds",
            started.elapsed().as_secs_f64(),
            MetricKind::Gauge,
            ts_ms,
        ));
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
    ts_ms: i64,
) -> DatastoreOutcome {
    let mut outcome = DatastoreOutcome { store: store.to_string(), ..Default::default() };

    {
        let _permit = permits.acquire().await.ok();
        match pbs.get::<GcStatus>(&format!("/admin/datastore/{store}/gc"), &[]).await {
            Ok(status) => {
                outcome.samples.extend(metrics::gc_samples(store, &status, ts_ms));
                outcome.gc_last_success = metrics::gc_last_success(&status);
            }
            Err(error) => {
                outcome.errors += 1;
                warn!(datastore = store, %error, "statut de GC indisponible");
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

    use ezymonit_proto::Credential;

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
            Credential::ApiToken { token: "monitoring@pbs!ezymonit=8f3a1c9e-dead-beef".into() };
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
