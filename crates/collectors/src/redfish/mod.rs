//! Collecteur Redfish : le matériel d'un serveur, vu par son contrôleur de
//! gestion (BMC).
//!
//! Redfish (DMTF, DSP0266) est l'API HTTPS que parlent tous les contrôleurs de
//! gestion actuels : Supermicro, Dell iDRAC, HPE iLO, Lenovo XClarity, ASRock
//! Rack, OpenBMC. Là où SNMP ne montre, sur un BMC, que le petit Linux du
//! contrôleur lui-même, Redfish décrit le serveur qu'il gère : ventilateurs,
//! températures et leurs seuils, tensions, alimentations et leur redondance,
//! disques, mémoire, processeurs, journal des événements.
//!
//! # Principes
//!
//! Les mêmes que pour les autres collecteurs REST :
//!
//! * **Une panne partielle reste une collecte réussie.** Seules la racine du
//!   service et la première collection authentifiée condamnent l'interrogation ;
//!   un châssis, un disque ou un journal illisible compte dans
//!   `redfish_scrape_errors`.
//! * **Les erreurs sont classées pour l'alerting.** Un mot de passe refusé donne
//!   `ProbeError::Auth`, un certificat auto-signé `ProbeError::Config` ; seul un
//!   contrôleur muet réveille « injoignable ».
//! * **Un emplacement vide n'est pas une panne.** `Status.State = Absent` (baie
//!   d'alimentation vide, emplacement DIMM libre) et `Disabled` ne produisent
//!   aucune série de santé.
//! * **Lecture seule, et rien du contenu des journaux.** Uniquement des `GET` —
//!   plus, en mode session, l'ouverture et la fermeture de la session. Les
//!   journaux sont comptés par gravité, leurs messages ne sortent pas.
//! * **Le contrôleur est ménagé.** Quatre requêtes simultanées au plus, des
//!   plafonds sur le nombre de membres, de capteurs et de disques lus.
//!
//! # Réglages, portés par les étiquettes de la cible
//!
//! | Étiquette | Défaut | Rôle |
//! |---|---|---|
//! | `port` | `443` | Port HTTPS, si l'adresse n'en précise pas. |
//! | `insecure_tls` | `false` | Accepte un certificat non vérifiable (auto-signé). |
//! | `request_timeout_seconds` | `8` | Délai par requête HTTP, de 1 à 120. |
//! | `auth` | `basic` | `basic` ou `session` (jeton `X-Auth-Token`). |
//! | `storage` | `true` | Lit les contrôleurs de stockage et leurs disques. |
//! | `logs` | `true` | Compte les entrées des journaux par gravité. |

mod client;
mod metrics;
mod model;
mod options;

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;
use dumbmonit_proto::{Collector, Credential, MetricKind, ProbeError, Sample, Target, TargetId};
use futures::StreamExt;
use serde_json::Value;
use tracing::{debug, warn};

use client::{Auth, RedfishClient, SessionSlot};
use model::{link, links, short_id};
use options::{AuthScheme, CONCURRENCY, MAX_DRIVES, MAX_MEMBERS, MAX_SENSORS, Options};

/// Identifiant de profil renvoyé par la découverte.
const PROFILE_ID: &str = "redfish";

#[derive(Default)]
pub struct RedfishCollector {
    /// Sessions ouvertes, une par cible, en mode `auth = session`.
    sessions: Mutex<HashMap<TargetId, SessionSlot>>,
}

impl RedfishCollector {
    pub fn new() -> Self {
        Self::default()
    }

    fn client(&self, target: &Target, options: &Options) -> Result<RedfishClient, ProbeError> {
        let (username, password) = match &target.credential {
            Credential::UsernamePassword { username, password } => {
                (username.clone(), password.clone())
            }
            other => {
                return Err(ProbeError::Config(format!(
                    "Redfish expects the user name and password of a management controller \
                     account, configured credential: {other}"
                )));
            }
        };
        let auth = match options.auth {
            AuthScheme::Basic => Auth::Basic { username, password },
            AuthScheme::Session => {
                Auth::Session { username, password, slot: self.session_slot(target.id) }
            }
        };
        Ok(RedfishClient::new(
            crate::http::client(options.insecure_tls)?,
            options.base_url.clone(),
            auth,
            options.request_timeout,
        ))
    }

    fn session_slot(&self, id: TargetId) -> SessionSlot {
        let mut cache = self.sessions.lock().unwrap_or_else(|poison| poison.into_inner());
        cache.entry(id).or_default().clone()
    }
}

#[async_trait]
impl Collector for RedfishCollector {
    fn kind(&self) -> &'static str {
        "redfish"
    }

    async fn probe(&self, target: &Target) -> Result<Vec<Sample>, ProbeError> {
        let options = Options::from_target(target)?;
        let client = self.client(target, &options)?;
        let started = std::time::Instant::now();
        let ts_ms = chrono::Utc::now().timestamp_millis();

        // La racine est publique : elle prouve que le service répond, pas que les
        // identifiants sont bons. C'est la première collection lue ensuite qui
        // tranche l'authentification.
        let root = client.service_root().await?;
        let mut samples = vec![metrics::service_info(&root, ts_ms)];

        let systems = link(&root, "/Systems");
        let chassis = link(&root, "/Chassis");
        let managers = link(&root, "/Managers");
        let Some(first) = systems.as_ref().or(chassis.as_ref()) else {
            return Err(ProbeError::Protocol(
                "The Redfish service lists neither systems nor chassis".to_string(),
            ));
        };
        // Erreur remontée telle quelle : 401, 403, contrôleur qui s'effondre.
        let first_collection = client.get(first).await?;

        let (system_part, chassis_part, manager_part) = futures::join!(
            collect_systems(
                &client,
                &options,
                systems.is_some().then_some(&first_collection),
                ts_ms
            ),
            collect_chassis(
                &client,
                chassis.as_deref(),
                systems.is_none().then_some(&first_collection),
                ts_ms
            ),
            collect_managers(&client, &options, managers.as_deref(), ts_ms),
        );
        let mut errors = 0u32;
        for part in [system_part, chassis_part, manager_part] {
            errors += part.errors;
            samples.extend(part.samples);
        }

        samples.push(Sample::new(
            "redfish_scrape_errors",
            f64::from(errors),
            MetricKind::Gauge,
            ts_ms,
        ));
        samples.push(Sample::new(
            "redfish_scrape_duration_seconds",
            started.elapsed().as_secs_f64(),
            MetricKind::Gauge,
            ts_ms,
        ));
        Ok(samples)
    }

    async fn discover(&self, target: &Target) -> Result<Option<String>, ProbeError> {
        let options = Options::from_target(target)?;
        let client = self.client(target, &options)?;
        let root = client.service_root().await?;
        debug!(
            target_id = target.id,
            vendor = model::text(&root, "/Vendor").unwrap_or("?"),
            version = model::text(&root, "/RedfishVersion").unwrap_or("?"),
            "service Redfish détecté"
        );
        Ok(Some(PROFILE_ID.to_string()))
    }
}

/// Ce qu'une partie de l'interrogation a produit.
#[derive(Default)]
struct Part {
    samples: Vec<Sample>,
    errors: u32,
}

impl Part {
    /// Range le résultat d'une lecture : `None` pour une ressource absente (404)
    /// ou en échec, l'échec comptant alors une erreur de collecte.
    fn settle(&mut self, path: &str, outcome: Result<Option<Value>, ProbeError>) -> Option<Value> {
        match outcome {
            Ok(value) => value,
            Err(error) => {
                self.errors += 1;
                warn!(path, %error, "ressource Redfish indisponible");
                None
            }
        }
    }
}

/// Lit plusieurs ressources, au plus [`CONCURRENCY`] à la fois, dans l'ordre.
async fn fetch_all(
    client: &RedfishClient,
    paths: Vec<String>,
) -> Vec<(String, Result<Option<Value>, ProbeError>)> {
    futures::stream::iter(paths)
        .map(|path| async move {
            let outcome = client.get_optional(&path).await;
            (path, outcome)
        })
        .buffered(CONCURRENCY)
        .collect()
        .await
}

/// Les membres d'une collection, plafonnés.
async fn members(
    client: &RedfishClient,
    part: &mut Part,
    collection: &str,
    cap: usize,
) -> Vec<String> {
    let outcome = client.get_optional(collection).await;
    let Some(value) = part.settle(collection, outcome) else { return Vec::new() };
    let mut list = links(&value, "/Members");
    list.truncate(cap);
    list
}

async fn collect_systems(
    client: &RedfishClient,
    options: &Options,
    collection: Option<&Value>,
    ts_ms: i64,
) -> Part {
    let mut part = Part::default();
    let Some(collection) = collection else { return part };
    let mut paths = links(collection, "/Members");
    paths.truncate(MAX_MEMBERS);

    for (path, outcome) in fetch_all(client, paths).await {
        let Some(system) = part.settle(&path, outcome) else { continue };
        let id = short_id(&system, &path);
        part.samples.extend(metrics::system_samples(&system, &id, ts_ms));

        if options.storage
            && let Some(storage) = link(&system, "/Storage")
        {
            collect_storage(client, &mut part, &storage, &id, ts_ms).await;
        }
        if options.logs
            && let Some(logs) = link(&system, "/LogServices")
        {
            collect_logs(client, &mut part, &logs, &id, ts_ms).await;
        }
    }
    part
}

async fn collect_storage(
    client: &RedfishClient,
    part: &mut Part,
    collection: &str,
    system: &str,
    ts_ms: i64,
) {
    let controllers = members(client, part, collection, MAX_MEMBERS).await;
    let mut drives = Vec::new();
    for (path, outcome) in fetch_all(client, controllers).await {
        let Some(storage) = part.settle(&path, outcome) else { continue };
        part.samples.extend(metrics::storage(&storage, system, ts_ms));
        drives.extend(links(&storage, "/Drives"));
    }
    drives.sort();
    drives.dedup();
    drives.truncate(MAX_DRIVES);
    // Le mockup DMTF nomme ses quatre disques « Drive Sample », et plus d'un
    // contrôleur réel fait de même : l'identifiant départage les homonymes.
    let mut seen = std::collections::HashSet::new();
    for (path, outcome) in fetch_all(client, drives).await {
        let Some(drive) = part.settle(&path, outcome) else { continue };
        let mut name = model::display_name(&drive);
        if !seen.insert(name.clone()) {
            name = format!("{name} ({})", short_id(&drive, &path));
            seen.insert(name.clone());
        }
        part.samples.extend(metrics::drive(&drive, &name, system, ts_ms));
    }
}

async fn collect_logs(
    client: &RedfishClient,
    part: &mut Part,
    collection: &str,
    owner: &str,
    ts_ms: i64,
) {
    let services = members(client, part, collection, 8).await;
    for (path, outcome) in fetch_all(client, services).await {
        let Some(service) = part.settle(&path, outcome) else { continue };
        let Some(entries) = link(&service, "/Entries") else { continue };
        let outcome = client.get_optional(&entries).await;
        if let Some(entries) = part.settle(&entries, outcome) {
            let log = short_id(&service, &path);
            part.samples.extend(metrics::log_entries(&entries, owner, &log, ts_ms));
        }
    }
}

async fn collect_chassis(
    client: &RedfishClient,
    collection_path: Option<&str>,
    already_read: Option<&Value>,
    ts_ms: i64,
) -> Part {
    let mut part = Part::default();
    let collection = match (already_read, collection_path) {
        (Some(value), _) => Some(value.clone()),
        (None, Some(path)) => {
            let outcome = client.get_optional(path).await;
            part.settle(path, outcome)
        }
        (None, None) => None,
    };
    let Some(collection) = collection else { return part };
    let mut paths = links(&collection, "/Members");
    paths.truncate(MAX_MEMBERS);

    for (path, outcome) in fetch_all(client, paths).await {
        let Some(chassis) = part.settle(&path, outcome) else { continue };
        let id = short_id(&chassis, &path);
        part.samples.extend(metrics::chassis_samples(&chassis, &id, ts_ms));
        collect_thermal(client, &mut part, &chassis, &id, ts_ms).await;
        collect_power(client, &mut part, &chassis, &id, ts_ms).await;
    }
    part
}

/// Températures et ventilateurs : l'ancien `Thermal` s'il existe (une requête),
/// sinon `ThermalSubsystem` et les capteurs un par un.
async fn collect_thermal(
    client: &RedfishClient,
    part: &mut Part,
    chassis: &Value,
    id: &str,
    ts_ms: i64,
) {
    if let Some(path) = link(chassis, "/Thermal") {
        let outcome = client.get_optional(&path).await;
        if let Some(thermal) = part.settle(&path, outcome) {
            part.samples.extend(metrics::legacy_thermal(&thermal, id, ts_ms));
            return;
        }
    }
    if let Some(path) = link(chassis, "/ThermalSubsystem") {
        let outcome = client.get_optional(&path).await;
        if let Some(subsystem) = part.settle(&path, outcome) {
            part.samples.extend(metrics::thermal_subsystem(&subsystem, id, ts_ms));
            if let Some(fans) = link(&subsystem, "/Fans") {
                let fans = members(client, part, &fans, MAX_SENSORS).await;
                for (path, outcome) in fetch_all(client, fans).await {
                    if let Some(fan) = part.settle(&path, outcome) {
                        part.samples.extend(metrics::fan(&fan, id, ts_ms));
                    }
                }
            }
        }
    }
    if let Some(path) = link(chassis, "/Sensors") {
        let sensors = members(client, part, &path, MAX_SENSORS).await;
        for (path, outcome) in fetch_all(client, sensors).await {
            if let Some(sensor) = part.settle(&path, outcome) {
                part.samples.extend(metrics::sensor(&sensor, id, ts_ms));
            }
        }
    }
}

/// Alimentations : l'ancien `Power` s'il existe, sinon `PowerSubsystem`.
async fn collect_power(
    client: &RedfishClient,
    part: &mut Part,
    chassis: &Value,
    id: &str,
    ts_ms: i64,
) {
    if let Some(path) = link(chassis, "/Power") {
        let outcome = client.get_optional(&path).await;
        if let Some(power) = part.settle(&path, outcome) {
            part.samples.extend(metrics::legacy_power(&power, id, ts_ms));
            return;
        }
    }
    if let Some(path) = link(chassis, "/PowerSubsystem") {
        let outcome = client.get_optional(&path).await;
        if let Some(subsystem) = part.settle(&path, outcome) {
            part.samples.extend(metrics::power_subsystem(&subsystem, id, ts_ms));
            if let Some(supplies) = link(&subsystem, "/PowerSupplies") {
                let supplies = members(client, part, &supplies, MAX_MEMBERS).await;
                for (path, outcome) in fetch_all(client, supplies).await {
                    if let Some(psu) = part.settle(&path, outcome) {
                        part.samples.extend(metrics::power_supply(&psu, id, ts_ms));
                    }
                }
            }
        }
    }
    if let Some(path) = link(chassis, "/EnvironmentMetrics") {
        let outcome = client.get_optional(&path).await;
        if let Some(environment) = part.settle(&path, outcome) {
            part.samples.extend(metrics::environment(&environment, id, ts_ms));
        }
    }
}

async fn collect_managers(
    client: &RedfishClient,
    options: &Options,
    collection: Option<&str>,
    ts_ms: i64,
) -> Part {
    let mut part = Part::default();
    let Some(collection) = collection else { return part };
    let paths = members(client, &mut part, collection, MAX_MEMBERS).await;
    for (path, outcome) in fetch_all(client, paths).await {
        let Some(manager) = part.settle(&path, outcome) else { continue };
        let id = short_id(&manager, &path);
        part.samples.extend(metrics::manager(&manager, &id, ts_ms));
        if options.logs
            && let Some(logs) = link(&manager, "/LogServices")
        {
            collect_logs(client, &mut part, &logs, &id, ts_ms).await;
        }
    }
    part
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Arc;
    use std::time::Duration;

    use super::*;

    fn cible(credential: Credential) -> Target {
        Target {
            id: 7,
            name: "bmc".into(),
            address: "10.0.0.50".into(),
            kind: "redfish".into(),
            profile_id: None,
            parent_id: None,
            interval: Duration::from_secs(120),
            enabled: true,
            tags: BTreeMap::new(),
            credential,
        }
    }

    #[test]
    fn le_collecteur_annonce_son_type() {
        assert_eq!(RedfishCollector::new().kind(), "redfish");
    }

    #[test]
    fn un_identifiant_inadapte_est_refuse_sans_divulguer_le_secret() {
        let collector = RedfishCollector::new();
        let target = cible(Credential::SnmpCommunity { community: "SECRET-COMMUNITY".into() });
        let options = Options::from_target(&target).unwrap();
        let error = collector.client(&target, &options).err().unwrap();
        assert!(matches!(error, ProbeError::Config(_)));
        assert!(!error.to_string().contains("SECRET-COMMUNITY"));
    }

    #[test]
    fn la_session_survit_a_l_interrogation_et_reste_propre_a_la_cible() {
        let collector = RedfishCollector::new();
        assert!(Arc::ptr_eq(&collector.session_slot(7), &collector.session_slot(7)));
        assert!(!Arc::ptr_eq(&collector.session_slot(7), &collector.session_slot(8)));
    }

    /// Sert un mockup DMTF depuis la mémoire, comme le ferait le simulateur
    /// officiel : une requête par connexion, 404 pour ce qui n'y est pas, et 401
    /// sans l'en-tête Basic attendu (`dumbmonit` / `bon`) — sauf sur la racine,
    /// publique selon la spécification.
    async fn mockup() -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        const FIXTURE: &str = include_str!("fixtures/dmtf-localstorage-degraded.json");
        let doc: Value = serde_json::from_str(FIXTURE).unwrap();
        let resources = Arc::new(doc["resources"].as_object().unwrap().clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let resources = resources.clone();
                tokio::spawn(async move {
                    let mut request = Vec::new();
                    let mut chunk = [0u8; 4096];
                    while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                        match socket.read(&mut chunk).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => request.extend_from_slice(&chunk[..n]),
                        }
                    }
                    let head = String::from_utf8_lossy(&request).to_lowercase();
                    let path = head.lines().next().and_then(|l| l.split(' ').nth(1)).unwrap_or("/");
                    let path = path.trim_end_matches('/');
                    // dumbmonit:bon
                    let authorized = head.contains("authorization: basic zhvtym1vbml0omjvbg==");
                    let found = resources.iter().find(|(k, _)| k.to_lowercase() == path);
                    let (status, body) = match found {
                        Some(_) if !authorized && path != "/redfish/v1" => {
                            ("401 Unauthorized", "{}".to_string())
                        }
                        Some((_, value)) => ("200 OK", value.to_string()),
                        None => ("404 Not Found", "{}".to_string()),
                    };
                    let reply = format!(
                        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\n\
                         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = socket.write_all(reply.as_bytes()).await;
                });
            }
        });
        format!("http://{address}")
    }

    fn cible_mockup(address: String, password: &str) -> Target {
        let mut target = cible(Credential::UsernamePassword {
            username: "dumbmonit".into(),
            password: password.into(),
        });
        target.address = address;
        target
    }

    fn valeur(samples: &[Sample], metric: &str, labels: &[(&str, &str)]) -> Option<f64> {
        samples
            .iter()
            .find(|s| {
                s.metric == metric
                    && labels.iter().all(|(k, v)| s.labels.get(*k).map(String::as_str) == Some(*v))
            })
            .map(|s| s.value)
    }

    #[tokio::test]
    async fn un_mockup_dmtf_degrade_declenche_chaque_regle_et_rien_de_plus() {
        let address = mockup().await;
        let samples = RedfishCollector::new().probe(&cible_mockup(address, "bon")).await.unwrap();

        // Ventilateur en panne, l'autre sain.
        let fan = [("chassis", "1U"), ("fan", "BaseBoard System Fan Backup")];
        assert_eq!(valeur(&samples, "redfish_fan_health", &fan), Some(2.0));
        let fan = [("chassis", "1U"), ("fan", "BaseBoard System Fan")];
        assert_eq!(valeur(&samples, "redfish_fan_health", &fan), Some(0.0));
        // Température au-dessus du seuil critique que le capteur publie.
        let cpu = [("chassis", "1U"), ("sensor", "CPU1 Temp")];
        assert_eq!(valeur(&samples, "redfish_temperature_celsius", &cpu), Some(47.0));
        assert_eq!(
            valeur(&samples, "redfish_temperature_upper_critical_celsius", &cpu),
            Some(45.0)
        );
        // CPU2 Temp est « Disabled » : aucune série.
        assert!(
            samples.iter().all(|s| s.labels.get("sensor").map(String::as_str) != Some("CPU2 Temp"))
        );
        // Une alimentation en panne, une baie vide ignorée, la redondance perdue.
        let psus: Vec<_> = samples.iter().filter(|s| s.metric == "redfish_psu_health").collect();
        assert_eq!(psus.len(), 1, "la baie « Absent » ne produit rien");
        assert_eq!(psus[0].value, 2.0);
        assert_eq!(valeur(&samples, "redfish_power_redundancy_health", &[]), Some(1.0));
        // Un disque sur quatre annonce sa fin ; les homonymes restent distincts.
        let predicted: Vec<_> =
            samples.iter().filter(|s| s.metric == "redfish_drive_failure_predicted").collect();
        assert_eq!(predicted.len(), 4);
        assert_eq!(predicted.iter().filter(|s| s.value == 1.0).count(), 1);
        // Santé du système, et l'identité du service.
        assert_eq!(
            valeur(&samples, "redfish_system_health", &[("system", "437XR1138R2")]),
            Some(2.0)
        );
        assert!(valeur(&samples, "redfish_info", &[]).is_some());
        // Les journaux sont comptés.
        assert!(valeur(&samples, "redfish_log_entries", &[("owner", "437XR1138R2")]).is_some());
        // Tout ce qui est lié et existe a été lu sans erreur.
        assert_eq!(valeur(&samples, "redfish_scrape_errors", &[]), Some(0.0));
    }

    #[tokio::test]
    async fn un_mauvais_mot_de_passe_est_une_erreur_dauthentification() {
        let address = mockup().await;
        let error =
            RedfishCollector::new().probe(&cible_mockup(address, "mauvais")).await.unwrap_err();
        assert!(matches!(error, ProbeError::Auth(_)), "{error}");
        assert!(!error.means_down());
        assert!(!error.to_string().contains("mauvais"));
    }

    #[test]
    fn une_erreur_de_ressource_compte_sans_interrompre() {
        let mut part = Part::default();
        assert!(part.settle("/a", Ok(None)).is_none());
        assert_eq!(part.errors, 0, "une ressource absente (404) n'est pas une erreur");
        let failed = part.settle("/b", Err(ProbeError::Unreachable("502".into())));
        assert!(failed.is_none());
        assert_eq!(part.errors, 1);
    }
}
