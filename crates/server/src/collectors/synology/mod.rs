//! Collecteur Synology DSM.
//!
//! Interroge l'API web d'un NAS Synology et en tire l'état du système, du
//! processeur, de la mémoire, des volumes et — surtout — des disques : température
//! et santé S.M.A.R.T. C'est la raison première de surveiller un NAS, un disque qui
//! chauffe ou dont le S.M.A.R.T. se dégrade annonçant une panne bien avant qu'un
//! volume ne tombe.
//!
//! # Principes
//!
//! * **Une panne partielle reste une collecte réussie.** Si l'inventaire de
//!   stockage échoue, l'état système et l'utilisation sont tout de même publiés, et
//!   l'échec est compté dans `synology_scrape_errors`. Seul un échec sur
//!   `SYNO.API.Info` ou sur la connexion condamne l'interrogation entière.
//! * **Les erreurs sont classées pour l'alerting.** DSM répond HTTP 200 même en cas
//!   d'échec : c'est le code du corps JSON qui fait foi. Un mot de passe refusé ou
//!   une vérification en deux étapes obligatoire donnent `ProbeError::Auth`, un
//!   certificat auto-signé donne `ProbeError::Config` — jamais `Unreachable`, sans
//!   quoi l'interface afficherait comme éteint un NAS qui répond parfaitement.
//! * **Aucun secret ne sort d'ici.** Ni mot de passe, ni `sid`, ni jeton anti-CSRF
//!   n'apparaît dans un journal, un message d'erreur ou une sortie `Debug`.
//!
//! # Ce qu'il faut côté DSM
//!
//! Un compte dédié, **sans vérification en deux étapes** — aucune supervision
//! automatique ne peut saisir un code à usage unique. L'API web de DSM ne propose
//! pas de jeton d'API pour les fonctions du cœur du système : seul le couple
//! identifiant / mot de passe est accepté.
//!
//! Les droits nécessaires ne sont pas les mêmes selon la métrique :
//!
//! * `SYNO.Core.System` — modèle, version, température, disponibilité — répond à
//!   n'importe quel compte DSM ;
//! * `SYNO.Core.System.Utilization` et `SYNO.Storage.CGI.Storage` — processeur,
//!   mémoire, volumes et surtout état S.M.A.R.T. des disques — exigent le groupe
//!   `administrators`. Sur DSM 7, la délégation « Surveillance du système »
//!   (Panneau de configuration › Utilisateur et groupe › Délégation
//!   d'administration) suffit pour l'utilisation, mais pas pour l'inventaire du
//!   stockage.
//!
//! Autrement dit : un compte sans droits particuliers donne un NAS « vivant » mais
//! aucune information sur ses disques, ce qui est justement l'essentiel.
//!
//! # Réglages, portés par les étiquettes de la cible
//!
//! | Étiquette | Défaut | Rôle |
//! |---|---|---|
//! | `insecure_tls` | `false` | Accepte un certificat non vérifiable (auto-signé). |
//! | `scheme` | `https` | `https` ou `http`. |
//! | `port` | `5001` en HTTPS, `5000` en HTTP | Port de DSM, si l'adresse n'en précise pas. |
//! | `request_timeout_seconds` | `15` | Délai par requête HTTP. |

mod auth;
mod backup;
mod client;
mod error;
mod metrics;
mod model;
mod options;

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use ezymonit_proto::{Collector, Credential, MetricKind, ProbeError, Sample, Target, TargetId};
use tracing::{debug, warn};

use auth::{Credentials, SessionSlot};
use client::DsmClient;
use options::Options;

/// Identifiant de profil renvoyé par la découverte.
const PROFILE_ID: &str = "synology-dsm";

/// Délai d'établissement de la connexion TCP + TLS.
///
/// Fixé une fois pour toutes au niveau du client mutualisé : un NAS éteint se
/// manifeste par un `SYN` sans réponse, et cinq secondes suffisent à le constater.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// État général du système. Donne le modèle, la version de DSM, la durée de
/// fonctionnement et, selon les modèles, la température du boîtier.
const API_SYSTEM: &str = "SYNO.Core.System";

/// Version visée de `SYNO.Core.System`. La 3 est la plus riche sur DSM 7 ; le
/// catalogue la ramène à 1 sur un DSM plus ancien.
const VERSION_SYSTEM: u32 = 3;

/// Charge processeur, mémoire et trafic réseau instantanés.
const API_UTILIZATION: &str = "SYNO.Core.System.Utilization";
const VERSION_UTILIZATION: u32 = 1;

/// Inventaire du stockage : volumes, groupes de stockage et disques, avec l'état
/// S.M.A.R.T. de chacun.
const API_STORAGE: &str = "SYNO.Storage.CGI.Storage";
const VERSION_STORAGE: u32 = 1;

/// Tâches Hyper Backup. Absente si le paquet n'est pas installé, ce qui n'est pas
/// une erreur : le NAS n'annonce alors tout simplement pas l'API.
const API_BACKUP: &str = "SYNO.Backup.Task";
const VERSION_BACKUP: u32 = 1;

/// API interrogées à chaque passage, déclarées à `SYNO.API.Info` en une fois.
///
/// `SYNO.API.Auth` en fait partie : son chemin et sa version se découvrent comme
/// les autres, ce qui évite de supposer `auth.cgi` ou `entry.cgi` selon la version
/// de DSM.
const WANTED_APIS: &[&str] =
    &["SYNO.API.Auth", API_SYSTEM, API_UTILIZATION, API_STORAGE, API_BACKUP];

#[derive(Default)]
pub struct SynologyCollector {
    /// Deux clients seulement, construits à la demande : `reqwest` mutualise le
    /// pool de connexions, et la politique TLS ne peut pas être changée par requête.
    http_verified: OnceLock<reqwest::Client>,
    http_unverified: OnceLock<reqwest::Client>,
    /// Sessions en cache, une par cible. Sans ce cache, chaque interrogation
    /// ouvrirait une session sur le NAS, que DSM journalise et affiche dans son
    /// historique de connexion.
    sessions: Mutex<HashMap<TargetId, SessionSlot>>,
}

/// `Debug` manuel : la carte des sessions contient des `sid`, qui valent mot de
/// passe tant qu'ils sont valides.
impl std::fmt::Debug for SynologyCollector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let sessions = self.sessions.lock().map(|map| map.len()).unwrap_or_default();
        write!(f, "SynologyCollector {{ sessions: {sessions} cached, values: <redacted> }}")
    }
}

impl SynologyCollector {
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

    fn credentials(&self, target: &Target, options: &Options) -> Result<Credentials, ProbeError> {
        match &target.credential {
            Credential::UsernamePassword { username, password } => Ok(Credentials {
                username: username.clone(),
                password: password.clone(),
                session_name: options.session_name.clone(),
                cached: self.session_slot(target.id),
            }),
            // DSM ne délivre pas de jeton d'API pour les fonctions du cœur du
            // système : accepter `ApiToken` ici ne ferait que produire un refus
            // incompréhensible au premier appel.
            Credential::ApiToken { .. } => Err(ProbeError::Config(
                "The DSM web API does not accept an API token for system and disk \
                 status. Enter a DSM account and its password: preferably an \
                 account dedicated to monitoring, member of the \"administrators\" \
                 group and without 2-step verification."
                    .to_string(),
            )),
            other => Err(ProbeError::Config(format!(
                "Synology DSM expects a username / password pair, \
                 configured credential: {other}"
            ))),
        }
    }

    fn session_slot(&self, id: TargetId) -> SessionSlot {
        let mut cache = self.sessions.lock().unwrap_or_else(|poison| poison.into_inner());
        cache.entry(id).or_default().clone()
    }

    async fn connect(&self, target: &Target) -> Result<DsmClient, ProbeError> {
        let options = Options::from_target(target)?;
        let dsm = DsmClient::new(
            self.http(options.insecure_tls)?,
            options.base_url.clone(),
            self.credentials(target, &options)?,
            options.request_timeout,
        );

        // Le catalogue est le point d'entrée principal : il ne demande aucune
        // session, donc son échec distingue « le NAS ne répond pas » de « le NAS
        // refuse mes identifiants ». La connexion vient ensuite, pour que l'erreur
        // d'authentification soit remontée une seule fois et clairement.
        dsm.load_catalog(WANTED_APIS).await?;
        dsm.ensure_session().await?;
        Ok(dsm)
    }
}

#[async_trait]
impl Collector for SynologyCollector {
    fn kind(&self) -> &'static str {
        "synology"
    }

    async fn probe(&self, target: &Target) -> Result<Vec<Sample>, ProbeError> {
        let started = std::time::Instant::now();
        let dsm = self.connect(target).await?;
        let ts_ms = chrono::Utc::now().timestamp_millis();

        let mut outcome = Outcome::default();
        outcome.samples.push(Sample::new("synology_up", 1.0, MetricKind::Gauge, ts_ms));

        // Les trois inventaires sont indépendants : les enchaîner tripleraient le
        // temps passé sur un NAS dont les disques sortent de veille.
        let (system, utilization, storage) = futures::join!(
            dsm.call::<model::SystemInfo>(API_SYSTEM, VERSION_SYSTEM, "info", &[]),
            dsm.call::<model::Utilization>(API_UTILIZATION, VERSION_UTILIZATION, "get", &[]),
            dsm.call::<model::StorageInfo>(API_STORAGE, VERSION_STORAGE, "load_info", &[]),
        );

        // L'horloge du NAS sert de référence aux anciennetés de sauvegarde. Elle est
        // lue avant que la réponse ne soit consommée, et vaut `None` si l'état
        // système n'a pas pu être obtenu.
        let nas_clock = system
            .as_ref()
            .ok()
            .and_then(|info| info.time.as_deref())
            .and_then(backup::parse_nas_clock);

        outcome.absorb(
            target.id,
            "état système",
            system.map(|info| metrics::system_samples(&info, ts_ms)),
        );
        outcome.absorb(
            target.id,
            "utilisation",
            utilization.map(|usage| metrics::utilization_samples(&usage, ts_ms)),
        );
        outcome.absorb(
            target.id,
            "inventaire du stockage",
            storage.map(|storage| {
                let mut samples = metrics::volume_samples(&storage.volumes, ts_ms);
                samples.extend(metrics::disk_samples(&storage.disks, ts_ms));
                if let Some(env) = &storage.env {
                    samples.extend(metrics::env_samples(env, ts_ms));
                }
                samples.push(metrics::storage_health_sample(&storage, ts_ms));
                samples
            }),
        );

        // Hyper Backup n'est pas installé sur tous les NAS : une API absente du
        // catalogue n'est pas une erreur, simplement une métrique en moins. La
        // compter ferait remonter `scrape_errors` en permanence sur ces NAS.
        if dsm.supports(API_BACKUP) {
            let tasks = collect_backups(&dsm).await;
            outcome.absorb(
                target.id,
                "tâches Hyper Backup",
                tasks.map(|tasks| backup::task_samples(&tasks, nas_clock, ts_ms)),
            );
        }

        let mut samples = outcome.samples;
        samples.push(Sample::new(
            "synology_scrape_errors",
            f64::from(outcome.errors),
            MetricKind::Gauge,
            ts_ms,
        ));
        samples.push(Sample::new(
            "synology_scrape_duration_seconds",
            started.elapsed().as_secs_f64(),
            MetricKind::Gauge,
            ts_ms,
        ));
        Ok(samples)
    }

    async fn discover(&self, target: &Target) -> Result<Option<String>, ProbeError> {
        let dsm = self.connect(target).await?;
        let info: model::SystemInfo = dsm.call(API_SYSTEM, VERSION_SYSTEM, "info", &[]).await?;

        debug!(
            target_id = target.id,
            compte = dsm.username(),
            modele = info.model.as_deref().unwrap_or("inconnu"),
            dsm = info.firmware_ver.as_deref().unwrap_or("inconnue"),
            "NAS Synology détecté"
        );
        Ok(Some(PROFILE_ID.to_string()))
    }
}

/// Ce qu'une interrogation a réuni.
///
/// Le type existe pour rendre testable la promesse centrale du collecteur : un
/// point de l'API en échec est compté et journalisé, jamais propagé. Sans lui,
/// cette règle ne vivrait que dans quatre blocs `match` recopiés, et rien ne
/// garantirait qu'ils se comportent tous pareil.
#[derive(Default)]
struct Outcome {
    samples: Vec<Sample>,
    errors: u32,
}

impl Outcome {
    fn absorb(&mut self, target_id: TargetId, quoi: &str, result: Result<Vec<Sample>, ProbeError>) {
        match result {
            Ok(samples) => self.samples.extend(samples),
            Err(error) => {
                self.errors += 1;
                warn!(target_id, quoi, %error, "point de l'API Synology indisponible");
            }
        }
    }
}

/// Paramètre `additional` de `SYNO.Backup.Task&method=status`.
///
/// Sans lui, la réponse ne contient ni date ni résultat. La liste reprend
/// exactement celle qu'emploie l'interface de DSM : une clé inconnue ferait
/// répondre au NAS une erreur de paramètre plutôt qu'un champ en moins.
const BACKUP_STATUS_ADDITIONAL: &str = r#"["last_bkp_time","next_bkp_time","last_bkp_result","is_modified","last_bkp_progress","last_bkp_success_version"]"#;

/// Interroge l'inventaire Hyper Backup, puis l'état de chaque tâche.
///
/// Seul l'échec de l'inventaire est remonté : une tâche dont l'état est illisible
/// est rapportée sans date plutôt que d'annuler les autres.
async fn collect_backups(
    dsm: &DsmClient,
) -> Result<Vec<(model::BackupTask, Option<model::BackupStatus>)>, ProbeError> {
    let list: model::BackupTaskList = dsm.call(API_BACKUP, VERSION_BACKUP, "list", &[]).await?;
    let tasks: Vec<model::BackupTask> =
        list.task_list.into_iter().take(backup::MAX_TASKS).collect();

    let statuses = futures::future::join_all(tasks.iter().map(|task| async {
        let params = [
            ("task_id", (task.task_id.0 as i64).to_string()),
            // `blOnline=false` interdit à DSM de contacter la destination : sans
            // cela, une destination distante injoignable fait durer l'appel
            // plusieurs secondes, voire échouer toute l'interrogation.
            ("blOnline", "false".to_string()),
            ("additional", BACKUP_STATUS_ADDITIONAL.to_string()),
        ];
        dsm.call::<model::BackupStatus>(API_BACKUP, VERSION_BACKUP, "status", &params).await
    }))
    .await;

    Ok(tasks
        .into_iter()
        .zip(statuses)
        .map(|(task, status)| {
            let status = status
                .inspect_err(|error| {
                    warn!(tache = task.task_id.0, %error, "état d'une tâche Hyper Backup illisible");
                })
                .ok();
            (task, status)
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use ezymonit_proto::Credential;

    use super::*;

    fn cible(credential: Credential) -> Target {
        Target {
            id: 12,
            name: "nas".into(),
            address: "192.168.1.10".into(),
            kind: "synology".into(),
            profile_id: None,
            parent_id: None,
            interval: Duration::from_secs(60),
            enabled: true,
            tags: BTreeMap::new(),
            credential,
        }
    }

    fn options() -> Options {
        Options::from_target(&cible(Credential::None)).unwrap()
    }

    #[test]
    fn le_collecteur_annonce_son_type() {
        assert_eq!(SynologyCollector::new().kind(), "synology");
    }

    #[test]
    fn un_couple_identifiants_est_accepte() {
        let collector = SynologyCollector::new();
        let credential = Credential::UsernamePassword {
            username: "supervision".into(),
            password: "secret".into(),
        };
        let credentials = collector.credentials(&cible(credential), &options()).unwrap();
        assert_eq!(credentials.username, "supervision");
        assert_eq!(credentials.session_name, "DumbMonit");
    }

    #[test]
    fn un_jeton_dapi_est_refuse_avec_une_explication_utile() {
        let collector = SynologyCollector::new();
        let credential = Credential::ApiToken { token: "SECRET-JETON".into() };
        let error = collector.credentials(&cible(credential), &options()).unwrap_err();

        assert!(matches!(error, ProbeError::Config(_)));
        let message = error.to_string();
        assert!(message.contains("password"), "{message}");
        assert!(!message.contains("SECRET-JETON"), "le jeton ne doit pas fuiter : {message}");
    }

    #[test]
    fn un_identifiant_inadapte_est_refuse_avant_tout_appel_reseau() {
        let collector = SynologyCollector::new();
        for credential in
            [Credential::None, Credential::SnmpCommunity { community: "SECRET-COMMUNITY".into() }]
        {
            let error = collector.credentials(&cible(credential), &options()).unwrap_err();
            assert!(matches!(error, ProbeError::Config(_)), "{error}");
            assert!(!error.to_string().contains("SECRET-COMMUNITY"), "{error}");
        }
    }

    #[test]
    fn la_session_est_partagee_entre_deux_interrogations_de_la_meme_cible() {
        let collector = SynologyCollector::new();
        let premiere = collector.session_slot(12);
        let seconde = collector.session_slot(12);
        assert!(
            std::sync::Arc::ptr_eq(&premiere, &seconde),
            "la session doit survivre à l'interrogation"
        );
        assert!(
            !std::sync::Arc::ptr_eq(&premiere, &collector.session_slot(13)),
            "une cible, une session"
        );
    }

    #[test]
    fn le_debug_du_collecteur_ne_laisse_fuir_aucune_session() {
        let collector = SynologyCollector::new();
        let slot = collector.session_slot(12);
        slot.blocking_lock().replace(auth::Session {
            sid: "SECRET-SID".into(),
            syno_token: Some("SECRET-JETON".into()),
            acquired_at: 1_700_000_000,
        });

        let rendu = format!("{collector:?}");
        assert!(!rendu.contains("SECRET-SID"), "{rendu}");
        assert!(!rendu.contains("SECRET-JETON"), "{rendu}");
        assert!(rendu.contains('1'), "le nombre de sessions reste utile : {rendu}");
    }

    #[test]
    fn un_point_de_lapi_en_echec_nefface_pas_les_autres_mesures() {
        let mut outcome = Outcome::default();
        outcome.absorb(
            7,
            "état système",
            Ok(vec![Sample::new("synology_temperature_celsius", 41.0, MetricKind::Gauge, 1)]),
        );
        outcome.absorb(
            7,
            "inventaire du stockage",
            Err(ProbeError::Auth("droits insuffisants".into())),
        );
        outcome.absorb(
            7,
            "utilisation",
            Ok(vec![Sample::new("synology_cpu_usage_percent", 18.0, MetricKind::Gauge, 1)]),
        );

        assert_eq!(outcome.errors, 1, "l'échec est compté sans masquer le reste");
        assert_eq!(outcome.samples.len(), 2, "les deux points sains ont livré leurs mesures");
        assert!(outcome.samples.iter().any(|s| s.metric == "synology_temperature_celsius"));
    }

    #[test]
    fn une_collecte_sans_incident_ne_compte_aucune_erreur() {
        let mut outcome = Outcome::default();
        outcome.absorb(7, "état système", Ok(Vec::new()));
        assert_eq!(outcome.errors, 0);
    }

    #[test]
    fn toutes_les_api_utilisees_sont_demandees_au_catalogue() {
        // Une API oubliée ici se traduirait par « API absente » à l'appel, alors
        // que le NAS la propose.
        for api in [API_SYSTEM, API_UTILIZATION, API_STORAGE, API_BACKUP] {
            assert!(WANTED_APIS.contains(&api), "{api} manque à la requête SYNO.API.Info");
        }
        assert!(WANTED_APIS.contains(&"SYNO.API.Auth"));
    }
}
