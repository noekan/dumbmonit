//! Collecteur Proxmox Datacenter Manager.
//!
//! PDM est la console qui fédère plusieurs clusters Proxmox VE et plusieurs
//! serveurs de sauvegarde PBS. Un équipement de ce type dans DumbMonit donne, en
//! une seule cible et un seul identifiant, l'état de tout le parc : quelles
//! instances la console atteint, ce qu'elles font tourner, et ce qui a échoué
//! dernièrement — plus l'état de la console elle-même.
//!
//! Ce n'est pas un remplaçant des équipements Proxmox VE et PBS : la console
//! n'expose qu'un résumé, là où un équipement dédié lit les instantanés, les
//! travaux planifiés, SMART et les pools ZFS. Voir `docs/devices/pdm.md`.
//!
//! # Principes
//!
//! Les mêmes que pour Proxmox VE et PBS :
//!
//! * **Une panne partielle reste une collecte réussie.** Une instance fédérée
//!   injoignable produit `pdm_remote_reachable = 0`, avec le message reçu par la
//!   console, et les autres livrent leurs mesures. Seul un échec sur `/version` —
//!   l'API ne répond pas ou refuse l'authentification — fait échouer
//!   l'interrogation entière.
//! * **Les erreurs sont classées pour l'alerting.** Un jeton invalide donne
//!   `ProbeError::Auth`, jamais « équipement hors ligne ».
//! * **Aucun secret ne sort d'ici.** Ni le jeton de la console, ni les jetons
//!   qu'elle détient pour joindre ses instances (jamais lus).
//! * **Un privilège facultatif ne manque pas bruyamment.** L'état de la collecte
//!   de métriques et le résumé des mises à jour distantes demandent des droits
//!   que le minimum documenté ne donne pas ; un 403 sur ces appels ne produit ni
//!   série, ni erreur de collecte.
//! * **La charge reste celle d'un seul appelant.** Les inventaires acceptent le
//!   cache que la console tient déjà (`max_age_seconds`) : ouvrir une page ne
//!   déclenche pas une rafale d'appels vers dix clusters.
//!
//! # Réglages, portés par les étiquettes de la cible
//!
//! | Étiquette | Défaut | Rôle |
//! |---|---|---|
//! | `insecure_tls` | `false` | Accepte un certificat non vérifiable (auto-signé). |
//! | `port` | `8443` | Port de l'API, si l'adresse n'en précise pas. |
//! | `request_timeout_seconds` | `20` | Délai par requête HTTP. |
//! | `node` | `localhost` | Nom du nœud qui porte la console. |
//! | `task_lookback_hours` | `24` | Fenêtre d'examen des tâches. |
//! | `max_age_seconds` | `60` | Fraîcheur acceptée du cache de la console. |
//! | `remotes` | toutes | Restreint la collecte à une liste d'instances. |
//! | `max_remotes` | `100` | Plafond d'instances interrogées pour leur version. |
//! | `versions` | `true` | Demande la version de chaque instance fédérée. |
//! | `tasks` | `true` | Interroge les tâches de toutes les instances. |
//! | `node_status` | `true` | Interroge l'hôte de la console (processeur, disque, certificats, abonnement). |
//! | `updates` | `true` | Compte les mises à jour en attente sur la console. |
//! | `remote_updates` | `false` | Résumé des mises à jour des instances fédérées (demande `Resource.Modify`). |
//!
//! # La vue, au-delà des métriques
//!
//! Le panneau de l'interface a besoin des messages d'erreur, des adresses et des
//! tâches elles-mêmes, pas seulement de séries. Chaque interrogation réussie
//! livre donc une [`ProbeView`] à l'observateur enregistré par
//! [`PdmCollector::with_observer`] — côté serveur, il la range en base.

mod auth;
mod client;
mod metrics;
mod model;
mod options;
mod view;

pub use view::{
    CertificateView, EstateView, HISTORY_DAYS, NodeView, ProbeObserver, ProbeView, RemoteView,
    SubscriptionView, TaskView,
};

use std::sync::Arc;

use async_trait::async_trait;
use dumbmonit_proto::{Collector, Credential, MetricKind, ProbeError, Sample, Target, TargetId};
use serde::de::DeserializeOwned;
use tokio::sync::Semaphore;
use tracing::{debug, warn};

use auth::AuthMode;
use client::PdmClient;
use model::{
    AptUpdate, CertificateInfo, MetricCollection, NodeStatus, RemoteEntry, RemoteResources,
    RemoteSubscription, RemoteVersion, ResourcesStatus, Subscription, TaskEntry, UpdatesSummary,
};
use options::Options;

/// Identifiant de profil renvoyé par la découverte.
const PROFILE_ID: &str = "proxmox-datacenter-manager";

/// Nombre maximal de versions d'instances demandées en parallèle.
///
/// Chaque appel traverse jusqu'au cluster distant : en lancer trente de front
/// reviendrait à ouvrir trente connexions sortantes depuis la console à chaque
/// interrogation.
const VERSION_CONCURRENCY: usize = 6;

#[derive(Default)]
pub struct PdmCollector {
    /// Destinataire de la vue de chaque interrogation ; sans lui, la sonde ne
    /// produit que des métriques.
    observer: Option<Arc<dyn ProbeObserver>>,
}

impl PdmCollector {
    pub fn new() -> Self {
        Self::default()
    }

    /// Enregistre le destinataire des vues d'interrogation.
    pub fn with_observer(mut self, observer: Arc<dyn ProbeObserver>) -> Self {
        self.observer = Some(observer);
        self
    }

    fn auth_mode(&self, target: &Target) -> Result<AuthMode, ProbeError> {
        match &target.credential {
            Credential::ApiToken { token } => auth::token_mode(token),
            other => Err(ProbeError::Config(format!(
                "Proxmox Datacenter Manager expects an API token. PDM only hands its session \
                 ticket back in an HttpOnly cookie, so a user name and password would buy \
                 nothing over a token. Configured credential: {other}"
            ))),
        }
    }

    fn client(&self, target: &Target, options: &Options) -> Result<PdmClient, ProbeError> {
        Ok(PdmClient::new(
            crate::http::client(options.insecure_tls)?,
            options.base_url.clone(),
            self.auth_mode(target)?,
            options.request_timeout,
        ))
    }
}

#[async_trait]
impl Collector for PdmCollector {
    fn kind(&self) -> &'static str {
        "pdm"
    }

    async fn probe(&self, target: &Target) -> Result<Vec<Sample>, ProbeError> {
        let options = Options::from_target(target)?;
        let pdm = self.client(target, &options)?;

        let started = std::time::Instant::now();
        let now = chrono::Utc::now();
        let (now_s, ts_ms) = (now.timestamp(), now.timestamp_millis());

        // `/version` sert de sonde de vie et d'authentification : c'est le seul
        // appel dont l'échec condamne l'interrogation entière.
        let version: model::Version = pdm.get("/version", &[]).await?;
        let mut samples = metrics::version_samples(&version, ts_ms);
        samples.push(Sample::new("pdm_up", 1.0, MetricKind::Gauge, ts_ms));
        let mut errors = 0u32;

        let max_age = [("max-age", options.max_age_seconds.to_string())];
        let tasks_query = [
            ("limit", options.task_limit.to_string()),
            ("since", (now_s - options.task_lookback_seconds).to_string()),
        ];
        // Les chemins du nœud sont construits avant le `join!` : une temporaire
        // créée dans la macro ne vivrait pas assez longtemps pour l'attente.
        let node_status_path = format!("/nodes/{}/status", options.node);
        let node_certificates_path = format!("/nodes/{}/certificates/info", options.node);
        let node_updates_path = format!("/nodes/{}/apt/update", options.node);
        let node_subscription_path = format!("/nodes/{}/subscription", options.node);

        // Les inventaires sont indépendants : les enchaîner multiplierait
        // d'autant le temps passé sur une console lente.
        let (
            configured,
            status,
            resources,
            subscriptions,
            collections,
            tasks,
            node,
            certificates,
            updates,
            remote_updates,
        ) = futures::join!(
            pdm.get::<Vec<RemoteEntry>>("/remotes/remote", &[]),
            pdm.get::<ResourcesStatus>("/resources/status", &max_age),
            pdm.get::<Vec<RemoteResources>>("/resources/list", &max_age),
            optional::<Vec<RemoteSubscription>>(&pdm, true, "/resources/subscription", &[]),
            optional::<Vec<MetricCollection>>(&pdm, true, "/remotes/metric-collection/status", &[],),
            optional::<Vec<TaskEntry>>(&pdm, options.tasks, "/remotes/tasks/list", &tasks_query),
            optional::<NodeStatus>(&pdm, options.node_status, &node_status_path, &[]),
            optional::<Vec<CertificateInfo>>(
                &pdm,
                options.node_status,
                &node_certificates_path,
                &[],
            ),
            optional::<Vec<AptUpdate>>(&pdm, options.updates, &node_updates_path, &[]),
            optional::<UpdatesSummary>(
                &pdm,
                options.remote_updates,
                "/remotes/updates/summary",
                &[]
            ),
        );

        // La liste configurée est la seule qui manquerait vraiment : sans elle, une
        // instance injoignable et jamais vue n'apparaîtrait nulle part.
        let configured = match configured {
            Ok(list) => list.into_iter().filter(|e| options.wants_remote(&e.id)).collect(),
            Err(error) => {
                errors += 1;
                warn!(target_id = target.id, %error, "liste des instances fédérées indisponible");
                Vec::new()
            }
        };

        let status = match status {
            Ok(status) => Some(status),
            Err(error) => {
                errors += 1;
                warn!(target_id = target.id, %error, "tableau de bord PDM indisponible");
                None
            }
        };
        let resources = match resources {
            Ok(list) => list.into_iter().filter(|e| options.wants_remote(&e.remote)).collect(),
            Err(error) => {
                errors += 1;
                warn!(target_id = target.id, %error, "inventaire des ressources PDM indisponible");
                Vec::new()
            }
        };
        let subscriptions =
            settle(subscriptions, &mut errors, target.id, "/resources/subscription")
                .unwrap_or_default();
        let collections =
            settle(collections, &mut errors, target.id, "/remotes/metric-collection/status")
                .unwrap_or_default();

        let estate = status.as_ref().map(metrics::estate_view).unwrap_or_default();
        samples.extend(metrics::estate_samples(&estate, ts_ms));

        let mut remotes = metrics::remote_views(
            &configured,
            status.as_ref(),
            &resources,
            &subscriptions,
            &collections,
        );
        remotes.retain(|remote| options.wants_remote(&remote.id));

        if options.versions {
            let permits = Semaphore::new(VERSION_CONCURRENCY);
            let wanted: Vec<String> = remotes
                .iter()
                .filter(|remote| remote.reachable)
                .take(options.max_remotes)
                .map(|remote| remote.id.clone())
                .collect();
            let found = futures::future::join_all(
                wanted.iter().map(|id| remote_version(&pdm, id, &permits)),
            )
            .await;
            for (id, version) in wanted.into_iter().zip(found) {
                if let Some(remote) = remotes.iter_mut().find(|remote| remote.id == id) {
                    match version {
                        Ok(version) => remote.version = version,
                        Err(error) => {
                            // Une version qui ne répond pas est une instance que la
                            // console n'atteint plus : c'est une mesure, pas un défaut
                            // de collecte.
                            remote.reachable = false;
                            if remote.error.is_none() {
                                remote.error = Some(error.to_string());
                            }
                        }
                    }
                }
            }
        }
        metrics::mark_versions_behind(&mut remotes);

        if let Some(summary) =
            settle(remote_updates, &mut errors, target.id, "/remotes/updates/summary")
        {
            for (id, value) in &summary.remotes {
                if let Some(remote) = remotes.iter_mut().find(|remote| remote.id == *id) {
                    remote.updates_pending = model::pending_updates(value);
                }
            }
        }

        let task_views = settle(tasks, &mut errors, target.id, "/remotes/tasks/list")
            .map(|list| metrics::task_views(&list))
            .unwrap_or_default();
        let digest = metrics::digest_tasks(&task_views, now_s, options.task_lookback_seconds);
        for remote in &mut remotes {
            remote.tasks_failed = digest.failed_by_remote.get(&remote.id).copied().unwrap_or(0);
        }
        samples.extend(metrics::task_samples(&digest, &remotes, ts_ms));

        for remote in &remotes {
            samples.extend(metrics::remote_samples(remote, ts_ms));
            samples.extend(metrics::collection_age_samples(remote, now_s, ts_ms));
        }

        let node = settle(node, &mut errors, target.id, "node status").map(|status| {
            let mut node = metrics::node_view(&status);
            if let Some(list) = certificates.as_ref().and_then(|outcome| outcome.as_ref().ok()) {
                node.certificates = metrics::certificate_views(list);
            }
            if let Some(list) = updates.as_ref().and_then(|outcome| outcome.as_ref().ok()) {
                node.updates_pending = Some(metrics::updates_count(list));
            }
            node
        });
        // L'abonnement se lit sur le même chemin que celui du nœud, mais décrit
        // tout le parc : il reste utile même sans état de nœud.
        let mut node = node;
        if options.node_status {
            match pdm.get_optional::<Subscription>(&node_subscription_path, &[]).await {
                Ok(Some(subscription)) => {
                    let view = metrics::subscription_view(&subscription);
                    node.get_or_insert_with(Default::default).subscription = Some(view);
                }
                Ok(None) => {}
                Err(error) => {
                    errors += 1;
                    warn!(target_id = target.id, %error, "abonnement PDM indisponible");
                }
            }
        }
        if let Some(node) = &node {
            samples.extend(metrics::node_samples(node, now_s, ts_ms));
        }

        samples.push(Sample::new("pdm_scrape_errors", f64::from(errors), MetricKind::Gauge, ts_ms));
        samples.push(Sample::new(
            "pdm_scrape_duration_seconds",
            started.elapsed().as_secs_f64(),
            MetricKind::Gauge,
            ts_ms,
        ));

        if let Some(observer) = &self.observer {
            let view = metrics::build_view(
                now_s,
                version.version.clone(),
                remotes,
                estate,
                node,
                task_views,
            );
            observer.observe(target, &view).await;
        }
        Ok(samples)
    }

    async fn discover(&self, target: &Target) -> Result<Option<String>, ProbeError> {
        let options = Options::from_target(target)?;
        let pdm = self.client(target, &options)?;

        let version: model::Version = pdm.get("/version", &[]).await?;
        debug!(
            target_id = target.id,
            version = version.version.as_deref().unwrap_or("inconnue"),
            "Proxmox Datacenter Manager détecté"
        );
        Ok(Some(PROFILE_ID.to_string()))
    }
}

/// La version d'une instance fédérée, telle que la console la lit.
async fn remote_version(
    pdm: &PdmClient,
    remote: &str,
    permits: &Semaphore,
) -> Result<Option<String>, ProbeError> {
    let _permit = permits.acquire().await.ok();
    let path = format!("/remotes/remote/{}/version", percent_encode(remote));
    let version: RemoteVersion = pdm.get(&path, &[]).await?;
    Ok(version.version)
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

/// Un appel facultatif : `None` si l'option est désactivée ou si le serveur
/// refuse l'accès — privilège facultatif, absent du minimum documenté —, sinon
/// le résultat de l'appel, erreurs comprises.
async fn optional<T: DeserializeOwned>(
    pdm: &PdmClient,
    enabled: bool,
    path: &str,
    query: &[(&str, String)],
) -> Option<Result<T, ProbeError>> {
    if !enabled {
        return None;
    }
    match pdm.get_optional::<T>(path, query).await {
        Ok(Some(data)) => Some(Ok(data)),
        Ok(None) => {
            debug!(path, "accès refusé ou chemin absent : aucune série, aucune erreur");
            None
        }
        Err(error) => Some(Err(error)),
    }
}

/// Dépouille le résultat d'un appel facultatif : une erreur — autre qu'un refus
/// d'accès, déjà absorbé — compte dans `pdm_scrape_errors` comme pour n'importe
/// quel inventaire.
fn settle<T>(
    outcome: Option<Result<T, ProbeError>>,
    errors: &mut u32,
    target_id: TargetId,
    path: &str,
) -> Option<T> {
    match outcome? {
        Ok(data) => Some(data),
        Err(error) => {
            *errors += 1;
            warn!(target_id, path, %error, "appel PDM facultatif en échec");
            None
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
            name: "pdm".into(),
            address: "10.0.0.40".into(),
            kind: "pdm".into(),
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
        assert_eq!(PdmCollector::new().kind(), "pdm");
    }

    #[test]
    fn seul_un_jeton_dapi_ouvre_une_session() {
        let collector = PdmCollector::new();
        let credential =
            Credential::ApiToken { token: "dumbmonit@pdm!monitor=8f3a1c9e-dead-beef".into() };
        assert!(collector.auth_mode(&cible(credential)).is_ok());
    }

    #[test]
    fn un_identifiant_inadapte_est_refuse_avant_tout_appel_reseau() {
        let collector = PdmCollector::new();
        for credential in [
            Credential::None,
            Credential::SnmpCommunity { community: "public".into() },
            Credential::UsernamePassword {
                username: "dumbmonit@pdm".into(),
                password: "secret".into(),
            },
        ] {
            let error = collector.auth_mode(&cible(credential)).unwrap_err();
            assert!(matches!(error, ProbeError::Config(_)));
        }
    }

    #[test]
    fn le_message_didentifiant_inadapte_ne_divulgue_pas_le_secret() {
        let collector = PdmCollector::new();
        let credential = Credential::UsernamePassword {
            username: "dumbmonit@pdm".into(),
            password: "SECRET-MOT-DE-PASSE".into(),
        };
        let error = collector.auth_mode(&cible(credential)).unwrap_err();
        assert!(!format!("{error}").contains("SECRET-MOT-DE-PASSE"));
    }

    #[test]
    fn le_port_par_defaut_est_celui_de_la_console() {
        let options = Options::from_target(&cible(Credential::None)).unwrap();
        assert_eq!(options.base_url, "https://10.0.0.40:8443");
    }

    #[test]
    fn un_appel_facultatif_en_erreur_compte_une_erreur_de_collecte() {
        let mut errors = 0;
        let absent: Option<Vec<u8>> = settle(None, &mut errors, 7, "/resources/subscription");
        assert!(absent.is_none());
        assert_eq!(errors, 0, "option désactivée ou 403 : rien à compter");

        let ok = settle(Some(Ok(vec![1u8, 2])), &mut errors, 7, "/resources/subscription");
        assert_eq!(ok, Some(vec![1, 2]));
        assert_eq!(errors, 0);

        let failed: Option<Vec<u8>> = settle(
            Some(Err(ProbeError::Unreachable("/remotes/tasks/list: 502".into()))),
            &mut errors,
            7,
            "/remotes/tasks/list",
        );
        assert!(failed.is_none());
        assert_eq!(errors, 1);
    }

    #[test]
    fn un_nom_dinstance_est_echappe_avant_dentrer_dans_une_url() {
        assert_eq!(percent_encode("site-a"), "site-a");
        assert_eq!(percent_encode("site a/b"), "site%20a%2Fb");
        assert_eq!(percent_encode("../etc"), "..%2Fetc");
    }
}
