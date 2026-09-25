//! Collecteur TrueNAS (SCALE / Community Edition).
//!
//! Interroge l'API REST de TrueNAS (`/api/v2.0`) et en tire ce qu'un
//! administrateur de stockage regarde, dans l'ordre où il le regarde : l'état
//! des pools et de leurs disques, la vérification ou la reconstruction en cours,
//! l'occupation des pools et des jeux de données au regard de leur quota, les
//! instantanés et les réplications qui protègent les données, les alertes que
//! TrueNAS a lui-même levées, les services, et la machine.
//!
//! # La panne que ce collecteur existe pour voir
//!
//! Un vdev en miroir ou en RAIDZ qui perd un disque continue de servir les
//! données. Le partage reste monté, les sauvegardes passent, personne ne voit
//! rien — jusqu'au deuxième disque. `truenas_pool_healthy` tombe à 0 dès le
//! premier, et la page nomme le disque.
//!
//! # Principes
//!
//! * **Une panne partielle reste une collecte réussie.** Seul `system/info` — la
//!   preuve que le NAS répond et que la clé est acceptée — fait échouer
//!   l'interrogation ; tout autre appel en échec incrémente
//!   `truenas_scrape_errors`.
//! * **Les erreurs sont classées pour l'alerting.** Une clé refusée, ou d'un
//!   utilisateur qui n'est pas administrateur complet, donne `ProbeError::Auth`.
//! * **Rien ne réveille un disque.** Les températures sont lues dans le cache de
//!   TrueNAS (`only_cached`), jamais par un `smartctl` qui ferait tourner un
//!   disque endormi.
//! * **Rien ne lance quoi que ce soit.** Ni vérification, ni test SMART, ni
//!   recherche de mise à jour : tout est lu, rien n'est déclenché.
//! * **Les chemins qui changent d'une version à l'autre sont facultatifs.**
//!   `zfs/snapshot` est devenu `pool/snapshot` en 25.10, `smart/test/results` a
//!   disparu en 25.10 : un 404 coûte une métrique, pas la sonde.
//! * **La cardinalité est bornée** à soixante-quatre séries par famille.
//!
//! # Réglages, portés par les étiquettes de la cible
//!
//! | Étiquette | Défaut | Rôle |
//! |---|---|---|
//! | `scheme` | `https` | Protocole de l'interface web. |
//! | `port` | `443` | Port de l'API, si l'adresse n'en précise pas. |
//! | `insecure_tls` | `false` | Accepte un certificat non vérifiable (auto-signé). |
//! | `request_timeout_seconds` | `20` | Délai par requête HTTP. |
//! | `datasets` | `true` | Occupation, quotas et instantanés des jeux de données. |
//! | `disks` | `true` | Inventaire et température des disques. |
//! | `smart` | `true` | Résultats des tests SMART. |
//! | `alerts` | `true` | Alertes levées par TrueNAS. |
//! | `tasks` | `true` | Réplications et instantanés périodiques. |
//! | `services` | `true` | Services qui démarrent avec le NAS. |

#[cfg(test)]
mod capture;
mod client;
mod metrics;
mod model;
mod options;
mod view;

pub use metrics::ALERT_LEVELS;
pub use options::MAX_SERIES_PER_FAMILY;
pub use view::{
    AlertView, DatasetView, DeviceView, DiskView, PoolView, ProbeObserver, ProbeView, ScanView,
    ServiceView, SystemView, TaskView, VdevView,
};

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use dumbmonit_proto::{Collector, Credential, MetricKind, ProbeError, Sample, Target, TargetId};
use serde_json::{Value, json};
use tracing::{debug, warn};

use client::TruenasClient;
use model::{
    Alert, Dataset, Disk, Pool, Replication, ScrubTask, Service, SmartResult, SnapshotTask,
    SystemInfo,
};
use options::Options;

/// Identifiant de profil renvoyé par la découverte.
const PROFILE_ID: &str = "truenas";

/// Options de la liste des jeux de données : une liste plate de tous les jeux
/// de données, sans propriétés utilisateur, avec le décompte d'instantanés — que
/// TrueNAS calcule depuis un cache ZFS, sans énumérer les instantanés — et les
/// seules propriétés lues.
///
/// Pas de `extra.retrieve_children=false` : sur un vrai 25.04, cette option ne
/// rend que le jeu de données racine de chaque pool, pas une liste plate. Et
/// pas de `snapshots_changed` : TrueNAS ne renvoie que les propriétés qu'il
/// connaît, et ignore celle-ci sans erreur.
const DATASET_QUERY: [(&str, &str); 4] = [
    ("extra.flat", "true"),
    ("extra.retrieve_user_props", "false"),
    ("extra.snapshots_count", "true"),
    ("extra.properties", r#"["used","available","quota","refquota"]"#),
];

/// La même sans le décompte d'instantanés, pour une version qui le refuserait.
const DATASET_QUERY_BASIC: [(&str, &str); 3] = [
    ("extra.flat", "true"),
    ("extra.retrieve_user_props", "false"),
    ("extra.properties", r#"["used","available","quota","refquota"]"#),
];

#[derive(Default)]
pub struct TruenasCollector {
    observer: Option<Arc<dyn ProbeObserver>>,
}

impl TruenasCollector {
    pub fn new() -> Self {
        Self::default()
    }

    /// Enregistre le destinataire des vues d'interrogation.
    pub fn with_observer(mut self, observer: Arc<dyn ProbeObserver>) -> Self {
        self.observer = Some(observer);
        self
    }

    fn client(&self, target: &Target, options: &Options) -> Result<TruenasClient, ProbeError> {
        let key = match &target.credential {
            Credential::ApiToken { token } if !token.trim().is_empty() => token.clone(),
            other => {
                return Err(ProbeError::Config(format!(
                    "TrueNAS expects an API key, configured credential: {other}"
                )));
            }
        };
        Ok(TruenasClient::new(
            crate::http::client(options.insecure_tls)?,
            options.base_url.clone(),
            &key,
            options.request_timeout,
        ))
    }
}

#[async_trait]
impl Collector for TruenasCollector {
    fn kind(&self) -> &'static str {
        "truenas"
    }

    async fn probe(&self, target: &Target) -> Result<Vec<Sample>, ProbeError> {
        let options = Options::from_target(target)?;
        let nas = self.client(target, &options)?;

        let started = std::time::Instant::now();
        let now = chrono::Utc::now();
        let (now_s, ts_ms) = (now.timestamp(), now.timestamp_millis());

        // La sonde de vie et d'authentification : le seul appel qui condamne.
        let info: SystemInfo = nas.get("/system/info", &[]).await?;
        let version = info.short_version();
        let mut samples = metrics::identity_samples(version.as_deref(), ts_ms);
        samples.push(Sample::new("truenas_up", 1.0, MetricKind::Gauge, ts_ms));
        let mut errors = 0u32;

        let system = metrics::system_view(&info);
        samples.extend(metrics::system_samples(&system, ts_ms));
        let mut view = ProbeView {
            probed_at: now_s,
            version,
            hostname: info.hostname.clone().filter(|name| !name.trim().is_empty()),
            system: Some(system),
            ..Default::default()
        };

        // Les pools et leur calendrier de vérification, puis tout le reste en
        // parallèle : un NAS lent ne doit pas additionner ses lenteurs.
        let (pools, scrubs, datasets, alerts, replications, snapshot_tasks, services) = futures::join!(
            nas.get_optional::<Vec<Pool>>("/pool", &[]),
            nas.get_optional::<Vec<ScrubTask>>("/pool/scrub", &[]),
            datasets_call(&nas, options.datasets),
            enabled(options.alerts, nas.get_optional::<Vec<Alert>>("/alert/list", &[])),
            enabled(options.tasks, nas.get_optional::<Vec<Replication>>("/replication", &[])),
            enabled(
                options.tasks,
                nas.get_optional::<Vec<SnapshotTask>>("/pool/snapshottask", &[])
            ),
            enabled(options.services, nas.get_optional::<Vec<Service>>("/service", &[])),
        );

        let scrubs = settle(scrubs, &mut errors, target.id, "pool/scrub").unwrap_or_default();
        if let Some(pools) = settle(pools, &mut errors, target.id, "pool") {
            view.pools = metrics::pool_views(&pools, &scrubs);
            samples.extend(metrics::pool_samples(&view.pools, now_s, ts_ms));
        }

        if let Some(alerts) =
            alerts.and_then(|outcome| settle(outcome, &mut errors, target.id, "alert/list"))
        {
            view.alerts = metrics::alert_views(&alerts);
            samples.extend(metrics::alert_samples(&view.alerts, ts_ms));
        }

        let replications = replications
            .and_then(|outcome| settle(outcome, &mut errors, target.id, "replication"))
            .unwrap_or_default();
        let snapshot_tasks = snapshot_tasks
            .and_then(|outcome| settle(outcome, &mut errors, target.id, "pool/snapshottask"))
            .unwrap_or_default();
        view.tasks = metrics::task_views(&replications, &snapshot_tasks);

        // Les jeux de données après les tâches : c'est la tâche d'instantanés
        // qui couvre un jeu de données qui dit quand il a été photographié.
        if let Some(datasets) =
            datasets.and_then(|outcome| settle(outcome, &mut errors, target.id, "pool/dataset"))
        {
            view.datasets = metrics::dataset_views(&datasets, &snapshot_tasks);
            samples.extend(metrics::dataset_samples(&view.datasets, now_s, ts_ms));
            view.snapshots_total =
                snapshot_total(&nas, &view.datasets, &mut errors, target.id).await;
            if let Some(total) = view.snapshots_total {
                samples.push(Sample::new("truenas_snapshots", total, MetricKind::Gauge, ts_ms));
            }
        }
        samples.extend(metrics::task_samples(&view.tasks, now_s, ts_ms));

        if let Some(services) =
            services.and_then(|outcome| settle(outcome, &mut errors, target.id, "service"))
        {
            view.services = metrics::service_views(&services);
            samples.extend(metrics::service_samples(&view.services, ts_ms));
        }

        if options.disks {
            view.disks = collect_disks(&nas, &options, &mut errors, target.id).await;
            samples.extend(metrics::disk_samples(&view.disks, ts_ms));
        }

        samples.push(Sample::new(
            "truenas_scrape_errors",
            f64::from(errors),
            MetricKind::Gauge,
            ts_ms,
        ));
        samples.push(Sample::new(
            "truenas_scrape_duration_seconds",
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
        let nas = self.client(target, &options)?;
        let info: SystemInfo = nas.get("/system/info", &[]).await?;
        debug!(
            target_id = target.id,
            version = info.version.as_deref().unwrap_or("inconnue"),
            "TrueNAS détecté"
        );
        Ok(Some(PROFILE_ID.to_string()))
    }
}

/// Un appel qui n'est fait que si l'option le demande.
async fn enabled<T>(
    wanted: bool,
    call: impl std::future::Future<Output = Result<Option<T>, ProbeError>>,
) -> Option<Result<Option<T>, ProbeError>> {
    if wanted { Some(call.await) } else { None }
}

/// La liste des jeux de données, avec repli sans le décompte d'instantanés si
/// la version le refuse.
async fn datasets_call(
    nas: &TruenasClient,
    wanted: bool,
) -> Option<Result<Option<Vec<Dataset>>, ProbeError>> {
    if !wanted {
        return None;
    }
    match nas.get_optional::<Vec<Dataset>>("/pool/dataset", &DATASET_QUERY).await {
        Err(ProbeError::Protocol(_)) => {
            Some(nas.get_optional::<Vec<Dataset>>("/pool/dataset", &DATASET_QUERY_BASIC).await)
        }
        outcome => Some(outcome),
    }
}

/// Le nombre total d'instantanés.
///
/// `?count=true` sur la liste des instantanés passe par un chemin rapide côté
/// TrueNAS et compte aussi ceux des jeux de données internes que la liste des
/// jeux de données cache — 16 contre 9 sur le NAS de test. C'est donc lui qui
/// fait foi : `zfs/snapshot` jusqu'en 25.04, `pool/snapshot` ensuite. La somme
/// des décomptes par jeu de données ne sert que de repli.
async fn snapshot_total(
    nas: &TruenasClient,
    datasets: &[DatasetView],
    errors: &mut u32,
    target_id: TargetId,
) -> Option<f64> {
    for path in ["/zfs/snapshot", "/pool/snapshot"] {
        match nas.get_optional::<Value>(path, &[("count", "true")]).await {
            Ok(Some(value)) if value.is_number() => return value.as_f64(),
            Ok(_) => continue,
            Err(error) => {
                *errors += 1;
                warn!(target_id, path, %error, "décompte des instantanés TrueNAS indisponible");
                break;
            }
        }
    }
    let counts: Vec<f64> = datasets.iter().filter_map(|dataset| dataset.snapshot_count).collect();
    (!counts.is_empty()).then(|| counts.iter().sum())
}

/// Les disques, leur température lue en cache et leurs tests SMART.
async fn collect_disks(
    nas: &TruenasClient,
    options: &Options,
    errors: &mut u32,
    target_id: TargetId,
) -> Vec<DiskView> {
    // `names: []` : tous les disques suivis. `only_cached` : jamais de
    // `smartctl`, donc jamais un disque endormi réveillé pour une mesure.
    let temperatures_body = json!({"names": [], "options": {"only_cached": true}});
    let (disks, temperatures, smart) = futures::join!(
        nas.get_optional::<Vec<Disk>>("/disk", &[("extra.pools", "true")]),
        nas.post_optional::<BTreeMap<String, Option<f64>>, _>(
            "/disk/temperatures",
            &temperatures_body
        ),
        enabled(options.smart, nas.get_optional::<Vec<SmartResult>>("/smart/test/results", &[])),
    );
    let Some(disks) = settle(disks, errors, target_id, "disk") else { return Vec::new() };
    let temperatures =
        settle(temperatures, errors, target_id, "disk/temperatures").unwrap_or_default();
    let smart = smart
        .and_then(|outcome| settle(outcome, errors, target_id, "smart/test/results"))
        .unwrap_or_default();
    metrics::disk_views(&disks, &temperatures, &smart)
}

/// Range le résultat d'un appel facultatif : la valeur si elle existe, un
/// compteur d'erreur incrémenté et une trace sinon.
fn settle<T>(
    outcome: Result<Option<T>, ProbeError>,
    errors: &mut u32,
    target_id: TargetId,
    path: &str,
) -> Option<T> {
    match outcome {
        Ok(value) => value,
        Err(error) => {
            *errors += 1;
            warn!(target_id, path, %error, "appel TrueNAS indisponible");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap as Map;
    use std::time::Duration;

    fn target(credential: Credential) -> Target {
        Target {
            id: 1,
            name: "nas".into(),
            address: "nas.lan".into(),
            kind: "truenas".into(),
            profile_id: None,
            parent_id: None,
            interval: Duration::from_secs(60),
            enabled: true,
            tags: Map::new(),
            credential,
        }
    }

    #[test]
    fn seule_une_cle_d_api_est_acceptee() {
        let collector = TruenasCollector::new();
        let options = Options::from_target(&target(Credential::None)).unwrap();
        assert!(
            collector
                .client(&target(Credential::ApiToken { token: "1-abc".into() }), &options)
                .is_ok()
        );
        for credential in [
            Credential::None,
            Credential::ApiToken { token: "  ".into() },
            Credential::UsernamePassword { username: "a".into(), password: "b".into() },
        ] {
            let error = collector.client(&target(credential), &options).err().unwrap();
            assert!(matches!(error, ProbeError::Config(_)));
        }
    }

    #[test]
    fn le_collecteur_annonce_son_type() {
        assert_eq!(TruenasCollector::new().kind(), "truenas");
    }
}
