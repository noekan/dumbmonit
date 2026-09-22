//! Collecteur Proxmox Mail Gateway.
//!
//! Interroge l'API REST d'une passerelle de messagerie PMG et en tire ce qu'un
//! administrateur de messagerie regarde, dans l'ordre où il le regarde : les
//! files d'attente Postfix, le trafic et ce qui y a été filtré, l'occupation des
//! quarantaines, les services et la grappe, l'état de la machine, et l'âge des
//! bases de signatures antivirus et antispam.
//!
//! # Principes
//!
//! Les mêmes que pour Proxmox VE et PBS :
//!
//! * **Une panne partielle reste une collecte réussie.** Un nœud de grappe
//!   injoignable produit `pmg_cluster_node_healthy = 0` et les autres livrent
//!   leurs métriques. Seul un échec sur `/version` — l'API ne répond pas ou
//!   refuse l'authentification — fait échouer l'interrogation.
//! * **Les erreurs sont classées pour l'alerting.** Un mot de passe invalide
//!   donne `ProbeError::Auth`, jamais « équipement hors ligne ».
//! * **Aucun secret ne sort d'ici.** Ni jeton, ni ticket, ni mot de passe
//!   n'apparaît dans un journal, un message d'erreur ou une sortie `Debug`.
//! * **Le contenu des messages ne sort jamais.** Les quarantaines sont comptées,
//!   jamais lues : aucun sujet, aucun expéditeur, aucun destinataire ne quitte la
//!   passerelle. Les statistiques par adresse ne sont pas interrogées.
//! * **La cardinalité est bornée.** Dix virus au plus produisent une série, vingt
//!   domaines au plus entrent dans la vue d'une file d'attente, et seize nœuds au
//!   plus sont interrogés.
//! * **Un appel facultatif ne manque pas bruyamment.** ClamAV peut ne pas être
//!   installé, l'abonnement peut être absent, un rôle limité peut refuser les
//!   mises à jour : un 403 ou un 404 sur ces appels ne produit ni série, ni
//!   erreur de collecte.
//!
//! # Le jour courant, et pas les vingt-quatre dernières heures
//!
//! `GET /statistics/mail` agrège par **journée locale du serveur** : PMG ignore
//! la fin de la fenêtre demandée, seul `starttime` choisit le jour. Interroger
//! avec `starttime = maintenant` donne donc les totaux depuis minuit, ce que
//! montre le tableau de bord de PMG lui-même — et ce que la sonde publie. Les
//! séries correspondantes sont des `Gauge` qui retombent à zéro à minuit, pas des
//! compteurs. La courbe des dernières heures (`/statistics/recent`), elle, lit la
//! table brute et roule vraiment.
//!
//! # Réglages, portés par les étiquettes de la cible
//!
//! | Étiquette | Défaut | Rôle |
//! |---|---|---|
//! | `insecure_tls` | `false` | Accepte un certificat non vérifiable (auto-signé). |
//! | `port` | `8006` | Port de l'API, si l'adresse n'en précise pas. |
//! | `request_timeout_seconds` | `15` | Délai par requête HTTP. |
//! | `node` | tous | Restreint la collecte à un nœud de la grappe. |
//! | `recent_hours` | `12` | Fenêtre de la courbe de trafic, de 1 à 24. |
//! | `queues` | `true` | Interroge les files d'attente Postfix (`qshape`). |
//! | `quarantine` | `true` | Interroge l'occupation des quarantaines. |
//! | `attachment_quarantine` | `false` | Compte aussi la quarantaine de pièces jointes. |
//! | `signatures` | `true` | Interroge l'âge des bases ClamAV et SpamAssassin. |
//! | `services` | `true` | Interroge l'état des unités systemd. |
//! | `certificates` | `true` | Interroge les certificats servis par l'interface. |
//! | `updates` | `true` | Interroge les mises à jour de paquets en attente. |
//! | `subscription` | `true` | Interroge l'abonnement du nœud. |
//!
//! # La vue, au-delà des métriques
//!
//! Le tableau de bord de l'interface a besoin des réponses elles-mêmes : la
//! ventilation d'une file d'attente par domaine, les noms des virus du jour, la
//! version de chaque base de signatures. Chaque interrogation réussie livre donc
//! une [`ProbeView`] à l'observateur enregistré par [`PmgCollector::with_observer`]
//! — côté serveur, il la range en base.

mod auth;
mod client;
mod metrics;
mod model;
mod options;
mod view;

pub use options::QUEUES;
pub use view::{
    CertificateView, ClusterNodeView, MAX_QUEUE_DOMAINS, MAX_VIRUSES, MailView, NodeView,
    ProbeObserver, ProbeView, QuarantineView, QueueDomainView, QueueView, RecentPointView,
    ServiceView, SignatureView, SpamScoreView, SubscriptionView, VirusView,
};

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use dumbmonit_proto::{Collector, Credential, MetricKind, ProbeError, Sample, Target, TargetId};
use serde::de::DeserializeOwned;
use tracing::{debug, warn};

use auth::{AuthMode, Ticket};
use client::PmgClient;
use model::{
    AptUpdate, CertificateInfo, ClamavDatabase, ClusterNode, MailStats, NodeEntry, NodeStatus,
    QshapeRow, QuarantineStatus, RecentPoint, ServiceEntry, SpamScore, SpamassassinChannel,
    Subscription, VirusStat,
};
use options::Options;

/// Identifiant de profil renvoyé par la découverte.
const PROFILE_ID: &str = "proxmox-mail-gateway";

/// Nom de nœud replié quand `/nodes` ne répond pas.
///
/// PMG accepte `localhost` là où il attend un nom de nœud et le résout sur la
/// machine qui reçoit l'appel : sans cette issue, une passerelle qui refuse
/// `/nodes` ne livrerait rien du tout de son système.
const FALLBACK_NODE: &str = "localhost";

#[derive(Default)]
pub struct PmgCollector {
    /// Tickets en cache, un par cible, pour ne pas ouvrir une session — que PMG
    /// journalise — à chaque interrogation.
    tickets: Mutex<HashMap<TargetId, Arc<tokio::sync::Mutex<Option<Ticket>>>>>,
    /// Destinataire de la vue de chaque interrogation ; sans lui, la sonde ne
    /// produit que des métriques.
    observer: Option<Arc<dyn ProbeObserver>>,
}

impl PmgCollector {
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
            Credential::UsernamePassword { username, password } => Ok(AuthMode::Ticket {
                username: username.clone(),
                password: password.clone(),
                cached: self.ticket_slot(target.id),
            }),
            Credential::ApiToken { token } => Ok(AuthMode::Token(auth::token_header_value(token)?)),
            other => Err(ProbeError::Config(format!(
                "Proxmox Mail Gateway expects a username / password pair, \
                 configured credential: {other}"
            ))),
        }
    }

    fn ticket_slot(&self, id: TargetId) -> Arc<tokio::sync::Mutex<Option<Ticket>>> {
        let mut cache = self.tickets.lock().unwrap_or_else(|poison| poison.into_inner());
        cache.entry(id).or_default().clone()
    }

    fn client(&self, target: &Target, options: &Options) -> Result<PmgClient, ProbeError> {
        Ok(PmgClient::new(
            crate::http::client(options.insecure_tls)?,
            options.base_url.clone(),
            self.auth_mode(target)?,
            options.request_timeout,
        ))
    }
}

#[async_trait]
impl Collector for PmgCollector {
    fn kind(&self) -> &'static str {
        "pmg"
    }

    async fn probe(&self, target: &Target) -> Result<Vec<Sample>, ProbeError> {
        let options = Options::from_target(target)?;
        let pmg = self.client(target, &options)?;

        let started = std::time::Instant::now();
        let now = chrono::Utc::now();
        let (now_s, ts_ms) = (now.timestamp(), now.timestamp_millis());

        // `/version` sert de sonde de vie et d'authentification : c'est le seul
        // appel dont l'échec condamne l'interrogation entière.
        let version: model::Version = pmg.get("/version", &[]).await?;
        let mut samples = metrics::version_samples(&version, ts_ms);
        samples.push(Sample::new("pmg_up", 1.0, MetricKind::Gauge, ts_ms));
        let mut errors = 0u32;

        let mut view =
            ProbeView { probed_at: now_s, version: version.version.clone(), ..Default::default() };

        // PMG agrège par journée locale et ne lit que `starttime` : demander
        // « maintenant » revient à demander « depuis minuit », ce que montre le
        // tableau de bord de PMG.
        let day = [("starttime", now_s.to_string()), ("endtime", now_s.to_string())];
        let recent_query = [
            ("hours", options.recent_hours.to_string()),
            ("timespan", options.recent_timespan_seconds.to_string()),
        ];

        // Les inventaires sont indépendants : les enchaîner multiplierait
        // d'autant le temps passé sur une passerelle lente.
        let (nodes, mail, recent, scores, viruses, spam_quar, virus_quar, cluster) = futures::join!(
            pmg.get::<Vec<NodeEntry>>("/nodes", &[]),
            optional::<MailStats>(&pmg, true, "/statistics/mail", &day),
            optional::<Vec<RecentPoint>>(&pmg, true, "/statistics/recent", &recent_query),
            optional::<Vec<SpamScore>>(&pmg, true, "/statistics/spamscores", &day),
            optional::<Vec<VirusStat>>(&pmg, true, "/statistics/virus", &day),
            optional::<QuarantineStatus>(&pmg, options.quarantine, "/quarantine/spamstatus", &[]),
            optional::<QuarantineStatus>(&pmg, options.quarantine, "/quarantine/virusstatus", &[]),
            optional::<Vec<ClusterNode>>(&pmg, true, "/config/cluster/status", &[]),
        );

        if let Some(stats) = settle(mail, &mut errors, target.id, "/statistics/mail") {
            let mail = metrics::mail_view(&stats);
            samples.extend(metrics::mail_samples(&mail, ts_ms));
            view.mail = Some(mail);
        }
        if let Some(points) = settle(recent, &mut errors, target.id, "/statistics/recent") {
            view.recent = metrics::recent_views(&points);
            samples.extend(metrics::throughput_samples(&view.recent, ts_ms));
        }
        if let Some(scores) = settle(scores, &mut errors, target.id, "/statistics/spamscores") {
            view.spam_scores = metrics::spam_score_views(&scores);
            samples.extend(metrics::spam_score_samples(&view.spam_scores, ts_ms));
        }
        if let Some(stats) = settle(viruses, &mut errors, target.id, "/statistics/virus") {
            view.viruses = metrics::virus_views(&stats);
            samples.extend(metrics::virus_samples(&view.viruses, ts_ms));
        }

        if options.quarantine {
            let spam = settle(spam_quar, &mut errors, target.id, "/quarantine/spamstatus");
            let virus = settle(virus_quar, &mut errors, target.id, "/quarantine/virusstatus");
            // La quarantaine de pièces jointes n'a pas d'appel de décompte : il
            // faut lister pour compter, donc c'est un choix explicite.
            let attachments = if options.attachment_quarantine {
                attachment_count(&pmg, &day, &mut errors, target.id).await
            } else {
                None
            };
            if spam.is_some() || virus.is_some() || attachments.is_some() {
                let quarantine =
                    metrics::quarantine_view(spam.as_ref(), virus.as_ref(), attachments);
                samples.extend(metrics::quarantine_samples(&quarantine, ts_ms));
                view.quarantine = Some(quarantine);
            }
        }

        // Une installation autonome renvoie une liste vide : ce n'est pas une
        // grappe dégradée, c'est une grappe absente, et rien n'est publié.
        if let Some(nodes) = settle(cluster, &mut errors, target.id, "/config/cluster/status") {
            view.cluster = metrics::cluster_views(&nodes);
            samples.extend(metrics::cluster_samples(&view.cluster, ts_ms));
        }

        // Les noms de nœuds : `/nodes` les liste tous, y compris ceux d'une
        // grappe. Un refus sur cet appel ne doit pas priver la sonde de l'état
        // système, d'où le repli sur `localhost`, que PMG résout lui-même.
        let node_names: Vec<String> = match nodes {
            Ok(entries) => {
                let mut names: Vec<String> = entries
                    .into_iter()
                    .filter_map(|entry| entry.node)
                    .filter(|name| options.wants_node(name))
                    .collect();
                names.sort();
                names.truncate(options.max_nodes);
                if names.is_empty() { vec![FALLBACK_NODE.to_string()] } else { names }
            }
            Err(error) => {
                debug!(target_id = target.id, %error, "liste des nœuds PMG indisponible, repli sur localhost");
                vec![FALLBACK_NODE.to_string()]
            }
        };

        // Les nœuds sont peu nombreux et chacun répond pour lui-même : les
        // interroger en parallèle garde la durée de la sonde proche de celle du
        // nœud le plus lent, et non de leur somme.
        let outcomes = futures::future::join_all(
            node_names.iter().map(|node| collect_node(&pmg, node, &options, now_s, ts_ms)),
        )
        .await;
        for outcome in outcomes {
            errors += outcome.errors;
            samples.extend(outcome.samples);
            view.nodes.push(outcome.node);
            view.queues.extend(outcome.queues);
        }

        // Les files d'attente d'une grappe se lisent nœud par nœud ; la vue les
        // additionne pour que la page montre « la » file d'attente.
        if view.nodes.len() > 1 {
            view.queues = merge_queues(&view.queues);
        }
        for queue in &view.queues {
            samples.extend(metrics::queue_samples(queue, ts_ms));
        }

        samples.push(Sample::new("pmg_scrape_errors", f64::from(errors), MetricKind::Gauge, ts_ms));
        samples.push(Sample::new(
            "pmg_scrape_duration_seconds",
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
        let pmg = self.client(target, &options)?;

        let version: model::Version = pmg.get("/version", &[]).await?;
        debug!(
            target_id = target.id,
            version = version.version.as_deref().unwrap_or("inconnue"),
            "Proxmox Mail Gateway détecté"
        );
        Ok(Some(PROFILE_ID.to_string()))
    }
}

/// Ce qu'un nœud a livré. Les files d'attente sont à part : elles sont produites
/// par nœud mais présentées agrégées.
#[derive(Default)]
struct NodeOutcome {
    node: NodeView,
    queues: Vec<QueueView>,
    samples: Vec<Sample>,
    errors: u32,
}

/// Interroge un nœud. Ne renvoie jamais d'erreur : un nœud qui ne répond pas se
/// traduit par un compteur d'erreurs, jamais par l'abandon des autres.
async fn collect_node(
    pmg: &PmgClient,
    node: &str,
    options: &Options,
    now_s: i64,
    ts_ms: i64,
) -> NodeOutcome {
    let mut outcome = NodeOutcome {
        node: NodeView { name: node.to_string(), ..Default::default() },
        ..Default::default()
    };

    let base = format!("/nodes/{node}");
    // Les chemins sont construits avant l'attente : une `format!` passée en
    // référence directement dans `join!` vivrait moins longtemps que le futur.
    let status_path = format!("{base}/status");
    let services_path = format!("{base}/services");
    let clamav_path = format!("{base}/clamav/database");
    let spamassassin_path = format!("{base}/spamassassin/rules");
    let certificates_path = format!("{base}/certificates/info");
    let subscription_path = format!("{base}/subscription");
    let updates_path = format!("{base}/apt/update");

    let (status, services, clamav, spamassassin, certificates, subscription, updates) = futures::join!(
        pmg.get::<NodeStatus>(&status_path, &[]),
        optional::<Vec<ServiceEntry>>(pmg, options.services, &services_path, &[]),
        optional::<Vec<ClamavDatabase>>(pmg, options.signatures, &clamav_path, &[]),
        optional::<Vec<SpamassassinChannel>>(pmg, options.signatures, &spamassassin_path, &[]),
        optional::<Vec<CertificateInfo>>(pmg, options.certificates, &certificates_path, &[]),
        optional::<Subscription>(pmg, options.subscription, &subscription_path, &[]),
        optional::<Vec<AptUpdate>>(pmg, options.updates, &updates_path, &[]),
    );

    match status {
        Ok(status) => {
            outcome.samples.extend(metrics::node_samples(node, &status, now_s, ts_ms));
            outcome.node.uptime_seconds = status.uptime.map(|n| n.0);
            outcome.node.cpu_percent = status.cpu.map(|n| n.0 * 100.0);
            outcome.node.cpu_count = status.cpuinfo.as_ref().and_then(|i| i.cpus).map(|n| n.0);
            outcome.node.loadavg = status.loadavg.iter().map(|n| n.0).collect();
            outcome.node.memory_used_bytes =
                status.memory.as_ref().and_then(|m| m.used).map(|n| n.0);
            outcome.node.memory_total_bytes =
                status.memory.as_ref().and_then(|m| m.total).map(|n| n.0);
            outcome.node.swap_used_bytes = status.swap.as_ref().and_then(|s| s.used).map(|n| n.0);
            outcome.node.swap_total_bytes = status.swap.as_ref().and_then(|s| s.total).map(|n| n.0);
            outcome.node.rootfs_used_bytes =
                status.rootfs.as_ref().and_then(|r| r.used).map(|n| n.0);
            outcome.node.rootfs_total_bytes =
                status.rootfs.as_ref().and_then(|r| r.total).map(|n| n.0);
            outcome.node.kernel = status.kversion.clone();
            outcome.node.version = metrics::node_version(&status);
            outcome.node.insync = status.insync.map(|n| n.0 != 0.0);
            outcome.node.clock_offset_seconds = metrics::clock_offset(&status, now_s);
        }
        Err(error) => {
            outcome.errors += 1;
            warn!(node, %error, "état du nœud PMG indisponible");
        }
    }

    if let Some(entries) = settle_node(services, &mut outcome.errors, node, "services") {
        outcome.node.services = metrics::service_views(&entries);
        outcome.samples.extend(metrics::service_samples(node, &outcome.node.services, ts_ms));
    }
    if let Some(databases) = settle_node(clamav, &mut outcome.errors, node, "clamav/database") {
        outcome.node.virus_databases = metrics::clamav_views(&databases);
        outcome.samples.extend(metrics::signature_samples(
            node,
            "virus",
            &outcome.node.virus_databases,
            now_s,
            ts_ms,
        ));
    }
    if let Some(channels) =
        settle_node(spamassassin, &mut outcome.errors, node, "spamassassin/rules")
    {
        outcome.node.spam_rules = metrics::spamassassin_views(&channels);
        outcome.samples.extend(metrics::signature_samples(
            node,
            "spam",
            &outcome.node.spam_rules,
            now_s,
            ts_ms,
        ));
    }
    if let Some(infos) = settle_node(certificates, &mut outcome.errors, node, "certificates/info") {
        outcome.node.certificates = metrics::certificate_views(&infos);
        outcome.samples.extend(metrics::certificate_samples(
            node,
            &outcome.node.certificates,
            now_s,
            ts_ms,
        ));
    }
    if let Some(subscription) = settle_node(subscription, &mut outcome.errors, node, "subscription")
        && let Some(view) = metrics::subscription_view(&subscription)
    {
        outcome.samples.extend(metrics::subscription_samples(node, &view, ts_ms));
        outcome.node.subscription = Some(view);
    }
    if let Some(updates) = settle_node(updates, &mut outcome.errors, node, "apt/update") {
        outcome.samples.extend(metrics::updates_samples(node, &updates, ts_ms));
        outcome.node.updates_pending = Some(updates.len() as f64);
        outcome.node.updates_security_pending =
            Some(updates.iter().filter(|u| u.is_security()).count() as f64);
    }

    if options.queues {
        outcome.queues = collect_queues(pmg, node, &base, &mut outcome.errors).await;
    }

    outcome
}

/// Les quatre files d'attente de Postfix, chacune par un appel à `qshape`.
///
/// `qshape` lance un processus sur le serveur : les quatre appels partent
/// ensemble mais restent quatre, et c'est pourquoi l'option existe.
async fn collect_queues(
    pmg: &PmgClient,
    node: &str,
    base: &str,
    errors: &mut u32,
) -> Vec<QueueView> {
    let path = format!("{base}/postfix/qshape");
    let outcomes = futures::future::join_all(QUEUES.iter().map(|queue| {
        let path = path.clone();
        async move {
            let query = [("queue", (*queue).to_string())];
            (*queue, pmg.get_optional::<Vec<QshapeRow>>(&path, &query).await)
        }
    }))
    .await;

    let mut queues = Vec::new();
    for (queue, outcome) in outcomes {
        match outcome {
            Ok(Some(rows)) => queues.push(metrics::queue_view(queue, &rows)),
            // Un 403 : le rôle n'a pas le droit de lire les files. Rien à
            // publier, et ce n'est pas une erreur de collecte.
            Ok(None) => debug!(node, queue, "files d'attente refusées, aucune série"),
            Err(error) => {
                *errors += 1;
                warn!(node, queue, %error, "file d'attente PMG indisponible");
            }
        }
    }
    queues
}

/// Additionne les files d'attente de plusieurs nœuds, une entrée par file.
///
/// L'âge du plus vieux message est le maximum : c'est le message le plus en
/// souffrance de la grappe que l'on veut voir, pas une moyenne qui le noierait.
fn merge_queues(queues: &[QueueView]) -> Vec<QueueView> {
    let mut merged: Vec<QueueView> = Vec::new();
    for queue in queues {
        match merged.iter_mut().find(|m| m.queue == queue.queue) {
            Some(target) => {
                target.messages += queue.messages;
                target.domains += queue.domains;
                target.oldest_age_seconds =
                    match (target.oldest_age_seconds, queue.oldest_age_seconds) {
                        (Some(a), Some(b)) => Some(a.max(b)),
                        (a, b) => a.or(b),
                    };
                target.top_domains.extend(queue.top_domains.iter().cloned());
            }
            None => merged.push(queue.clone()),
        }
    }
    // Les domaines ont pu être comptés deux fois : on fusionne et on replafonne.
    for queue in &mut merged {
        let mut totals: std::collections::BTreeMap<String, f64> = std::collections::BTreeMap::new();
        for domain in queue.top_domains.drain(..) {
            *totals.entry(domain.domain).or_default() += domain.messages;
        }
        let mut domains: Vec<QueueDomainView> = totals
            .into_iter()
            .map(|(domain, messages)| QueueDomainView { domain, messages })
            .collect();
        domains.sort_by(|a, b| {
            b.messages
                .partial_cmp(&a.messages)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.domain.cmp(&b.domain))
        });
        queue.domains = domains.len() as f64;
        domains.truncate(MAX_QUEUE_DOMAINS);
        queue.top_domains = domains;
    }
    merged
}

/// Compte les messages en quarantaine de pièces jointes.
///
/// Cette quarantaine n'a pas d'appel de décompte : il faut lister pour compter.
/// Seule la longueur de la liste est retenue — pas un sujet, pas une adresse.
async fn attachment_count(
    pmg: &PmgClient,
    query: &[(&str, String); 2],
    errors: &mut u32,
    target_id: TargetId,
) -> Option<f64> {
    match pmg.get_optional::<Vec<serde::de::IgnoredAny>>("/quarantine/attachment", query).await {
        Ok(Some(list)) => Some(list.len() as f64),
        Ok(None) => None,
        Err(error) => {
            *errors += 1;
            warn!(target_id, %error, "quarantaine de pièces jointes indisponible");
            None
        }
    }
}

/// Un appel facultatif : `None` si l'option est désactivée ou si le serveur
/// répond 403 (droit absent) ou 404 (version ou paquet absent), sinon le
/// résultat de l'appel, erreurs comprises.
async fn optional<T: DeserializeOwned>(
    pmg: &PmgClient,
    enabled: bool,
    path: &str,
    query: &[(&str, String)],
) -> Option<Result<T, ProbeError>> {
    if !enabled {
        return None;
    }
    match pmg.get_optional::<T>(path, query).await {
        Ok(Some(value)) => Some(Ok(value)),
        Ok(None) => {
            debug!(path, "appel refusé ou absent : aucune série");
            None
        }
        Err(error) => Some(Err(error)),
    }
}

/// Dépouille le résultat d'un appel facultatif : une erreur — autre qu'un refus
/// ou une absence, déjà absorbés — compte dans `pmg_scrape_errors` comme pour
/// n'importe quel inventaire.
fn settle<T>(
    outcome: Option<Result<T, ProbeError>>,
    errors: &mut u32,
    target_id: TargetId,
    path: &str,
) -> Option<T> {
    match outcome? {
        Ok(value) => Some(value),
        Err(error) => {
            *errors += 1;
            warn!(target_id, path, %error, "appel PMG indisponible");
            None
        }
    }
}

/// Comme [`settle`], en nommant le nœud plutôt que la cible : un message d'un
/// nœud de grappe doit dire lequel.
fn settle_node<T>(
    outcome: Option<Result<T, ProbeError>>,
    errors: &mut u32,
    node: &str,
    path: &str,
) -> Option<T> {
    match outcome? {
        Ok(value) => Some(value),
        Err(error) => {
            *errors += 1;
            warn!(node, path, %error, "appel PMG indisponible");
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
            id: 9,
            name: "pmg".into(),
            address: "10.0.0.40".into(),
            kind: "pmg".into(),
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
        assert_eq!(PmgCollector::new().kind(), "pmg");
    }

    #[test]
    fn un_couple_identifiants_produit_une_session_a_ticket() {
        let collector = PmgCollector::new();
        let credential = Credential::UsernamePassword {
            username: "monitoring@pmg".into(),
            password: "secret".into(),
        };
        let mode = collector.auth_mode(&cible(credential)).unwrap();
        assert!(matches!(mode, AuthMode::Ticket { .. }));
    }

    #[test]
    fn un_jeton_dapi_produit_une_session_sans_ticket() {
        let collector = PmgCollector::new();
        let credential =
            Credential::ApiToken { token: "monitoring@pmg!dumbmonit=8f3a1c9e-dead-beef".into() };
        let mode = collector.auth_mode(&cible(credential)).unwrap();
        assert!(matches!(mode, AuthMode::Token(_)));
    }

    #[test]
    fn le_cache_de_ticket_est_partage_entre_deux_interrogations_de_la_meme_cible() {
        let collector = PmgCollector::new();
        let premier = collector.ticket_slot(9);
        let second = collector.ticket_slot(9);
        assert!(Arc::ptr_eq(&premier, &second), "le ticket doit survivre à l'interrogation");
        assert!(!Arc::ptr_eq(&premier, &collector.ticket_slot(10)), "une cible, un ticket");
    }

    #[test]
    fn un_identifiant_inadapte_est_refuse_avant_tout_appel_reseau() {
        let collector = PmgCollector::new();
        for credential in
            [Credential::None, Credential::SnmpCommunity { community: "public".into() }]
        {
            let error = collector.auth_mode(&cible(credential)).unwrap_err();
            assert!(matches!(error, ProbeError::Config(_)));
        }
    }

    #[test]
    fn le_message_didentifiant_inadapte_ne_divulgue_pas_le_secret() {
        let collector = PmgCollector::new();
        let credential = Credential::SnmpCommunity { community: "SECRET-COMMUNITY".into() };
        let error = collector.auth_mode(&cible(credential)).unwrap_err();
        assert!(!format!("{error}").contains("SECRET-COMMUNITY"));
    }

    #[test]
    fn le_port_par_defaut_est_celui_de_pmg() {
        let options = Options::from_target(&cible(Credential::None)).unwrap();
        assert_eq!(options.base_url, "https://10.0.0.40:8006");
    }

    #[test]
    fn un_appel_facultatif_en_erreur_compte_une_erreur_de_collecte() {
        let mut errors = 0;
        let absent: Option<Vec<u8>> = settle(None, &mut errors, 9, "/statistics/mail");
        assert!(absent.is_none());
        assert_eq!(errors, 0, "option désactivée ou 403 : rien à compter");

        let ok = settle(Some(Ok(vec![1u8, 2])), &mut errors, 9, "/statistics/mail");
        assert_eq!(ok, Some(vec![1, 2]));
        assert_eq!(errors, 0);

        let failed: Option<Vec<u8>> = settle(
            Some(Err(ProbeError::Unreachable("/statistics/virus: 502".into()))),
            &mut errors,
            9,
            "/statistics/virus",
        );
        assert!(failed.is_none());
        assert_eq!(errors, 1);
    }

    #[test]
    fn les_files_dattente_dune_grappe_sadditionnent() {
        let queues = vec![
            QueueView {
                queue: "deferred".into(),
                messages: 4.0,
                domains: 2.0,
                oldest_age_seconds: Some(600.0),
                top_domains: vec![
                    QueueDomainView { domain: "a.net".into(), messages: 3.0 },
                    QueueDomainView { domain: "b.net".into(), messages: 1.0 },
                ],
            },
            QueueView {
                queue: "active".into(),
                messages: 1.0,
                domains: 1.0,
                oldest_age_seconds: None,
                top_domains: vec![QueueDomainView { domain: "a.net".into(), messages: 1.0 }],
            },
            QueueView {
                queue: "deferred".into(),
                messages: 6.0,
                domains: 1.0,
                oldest_age_seconds: Some(9_600.0),
                top_domains: vec![QueueDomainView { domain: "a.net".into(), messages: 6.0 }],
            },
        ];

        let merged = merge_queues(&queues);
        assert_eq!(merged.len(), 2);

        let deferred = merged.iter().find(|q| q.queue == "deferred").unwrap();
        assert_eq!(deferred.messages, 10.0);
        assert_eq!(
            deferred.oldest_age_seconds,
            Some(9_600.0),
            "c'est le message le plus en souffrance de la grappe qui compte"
        );
        assert_eq!(deferred.domains, 2.0, "un domaine vu deux fois n'en fait qu'un");
        assert_eq!(deferred.top_domains[0].domain, "a.net");
        assert_eq!(deferred.top_domains[0].messages, 9.0);

        let active = merged.iter().find(|q| q.queue == "active").unwrap();
        assert!(active.oldest_age_seconds.is_none());
    }

    #[test]
    fn les_quatre_files_de_postfix_sont_celles_quun_administrateur_lit() {
        assert_eq!(QUEUES, ["incoming", "active", "deferred", "hold"]);
    }
}
