//! Boucle d'évaluation et orchestration des entrées/sorties.
//!
//! Tout le raisonnement vit dans [`crate::alerting::cycle`] ; ce module se contente
//! d'aller chercher les données, d'appeler le cycle, puis d'écrire et de notifier.
//! La règle qui gouverne le fichier : rien de ce qui échoue ici — VictoriaMetrics
//! muet, canal en panne, base verrouillée — ne doit interrompre la boucle.

use std::time::Duration;

use anyhow::Result;
use chrono::{DateTime, TimeDelta, Utc};
use sqlx::SqlitePool;
use tokio::time::{MissedTickBehavior, interval};
use tracing::{debug, info, warn};

use crate::alerting::baseline;
use crate::alerting::cycle::{self, CycleInput, RuleObservations};
use crate::alerting::notify_policy::{self, Outgoing, Recipient};
use crate::alerting::source::MetricSource;
use crate::crypto::Cipher;
use crate::db;
use crate::notify;
use crate::notify::policy_store;
use crate::state::AppState;

/// Réglages de la boucle, lus dans l'environnement.
///
/// Ils ne passent pas par `Config` : ce fichier appartient à un autre lot de
/// travail, et l'alerting n'a pas besoin d'y ajouter des champs pour fonctionner.
#[derive(Debug, Clone, Copy)]
pub struct AlertingConfig {
    /// Période d'évaluation.
    pub interval: Duration,
    /// Durée de conservation de l'historique des transitions.
    pub history_retention: Duration,
    /// Durée au-delà de laquelle la baseline d'une série disparue est oubliée.
    pub baseline_retention: Duration,
}

impl Default for AlertingConfig {
    fn default() -> Self {
        Self {
            // Trente secondes : assez réactif pour une alerte « injoignable » avec un
            // `for` d'une minute, assez espacé pour ne pas marteler VictoriaMetrics.
            interval: Duration::from_secs(30),
            history_retention: Duration::from_secs(90 * 24 * 3600),
            baseline_retention: Duration::from_secs(60 * 24 * 3600),
        }
    }
}

impl AlertingConfig {
    pub fn from_env() -> Self {
        let default = Self::default();
        Self {
            interval: env_secs("DUMBMONIT_ALERT_INTERVAL_SECS", default.interval)
                // Descendre sous dix secondes ne rendrait rien plus réactif : la
                // période d'interrogation minimale d'une cible est déjà de dix secondes.
                .max(Duration::from_secs(10)),
            // La variable est exprimée en jours, comme son nom l'indique.
            history_retention: env_days("DUMBMONIT_ALERT_HISTORY_DAYS", default.history_retention),
            baseline_retention: default.baseline_retention,
        }
    }
}

fn env_days(key: &str, default: Duration) -> Duration {
    crate::config::env_var(key)
        .and_then(|raw| raw.parse::<u64>().ok())
        .filter(|days| *days > 0)
        .map_or(default, |days| Duration::from_secs(days * 86_400))
}

fn env_secs(key: &str, default: Duration) -> Duration {
    crate::config::env_var(key)
        .and_then(|raw| raw.parse::<u64>().ok())
        .filter(|secs| *secs > 0)
        .map_or(default, Duration::from_secs)
}

/// Bilan d'un cycle, renvoyé pour les tests et les journaux.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct EvalReport {
    pub rules_evaluated: usize,
    pub rules_failed: usize,
    pub alerts_tracked: usize,
    pub firing: usize,
    pub suppressed: usize,
    pub silenced: usize,
    pub learning: usize,
    pub groups: usize,
    pub notifications_sent: usize,
    pub notifications_failed: usize,
}

/// Démarre la boucle d'évaluation.
///
/// À appeler depuis `main.rs`, exactement comme `scheduler::spawn`.
pub fn spawn(state: AppState) {
    let config = AlertingConfig::from_env();
    tokio::spawn(async move {
        run(state, config).await;
    });
}

async fn run(state: AppState, config: AlertingConfig) {
    match db::alerts::seed_builtin_rules(&state.pool).await {
        Ok(0) => debug!("built-in rules already present"),
        Ok(count) => info!(count, "built-in alert rules installed"),
        // Sans les règles livrées, l'utilisateur peut toujours créer les siennes :
        // ce n'est pas une raison pour renoncer à alerter.
        Err(error) => warn!(?error, "could not install built-in rules"),
    }

    let http = notify::http_client();
    let mut ticker = interval(config.interval);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
    info!(interval = ?config.interval, "alerting engine started");

    let mut last_maintenance = Utc::now();

    loop {
        ticker.tick().await;
        let now = Utc::now();

        match evaluate_once(&state.pool, &state.cipher, &state.victoria, &http, now).await {
            Ok(report) => {
                if report.groups > 0 || report.rules_failed > 0 {
                    info!(
                        firing = report.firing,
                        supprimees = report.suppressed,
                        messages = report.groups,
                        envois = report.notifications_sent,
                        echecs = report.notifications_failed,
                        regles_en_echec = report.rules_failed,
                        "alerting cycle"
                    );
                } else {
                    debug!(alertes = report.alerts_tracked, "alerting cycle");
                }
            }
            // Une erreur ici vient de la base : on la journalise et on retente au
            // cycle suivant plutôt que de tuer la tâche pour de bon.
            Err(error) => warn!(?error, "alerting cycle failed"),
        }

        // L'entretien est horaire : purger à chaque cycle ferait des écritures
        // inutiles toutes les trente secondes pour supprimer zéro ligne.
        if now.signed_duration_since(last_maintenance) >= TimeDelta::hours(1) {
            last_maintenance = now;
            if let Err(error) = maintenance(&state.pool, &config, now).await {
                warn!(?error, "alerting tables maintenance failed");
            }
        }
    }
}

/// Purges périodiques.
async fn maintenance(pool: &SqlitePool, config: &AlertingConfig, now: DateTime<Utc>) -> Result<()> {
    let history_cutoff = now - TimeDelta::from_std(config.history_retention).unwrap_or_default();
    let removed = db::alerts::purge_history(pool, history_cutoff).await?;

    let baseline_cutoff = now - TimeDelta::from_std(config.baseline_retention).unwrap_or_default();
    let forgotten = db::alerts::purge_stale_baselines(pool, baseline_cutoff).await?;

    let expired = db::alerts::purge_expired_silences(pool, now).await?;

    // Le registre des notifications ne sert qu'aux fenêtres glissantes du pipeline
    // (plafond horaire, battement, délai minimal) : au-delà, il ne vaut rien.
    let global = policy_store::load_global(pool).await?;
    let channel_policies = policy_store::load_channel_policies(pool).await?;
    let lookback = policy_store::lookback(&global, &channel_policies);
    let ledger = policy_store::purge(pool, now, lookback).await?;

    debug!(
        historique = removed,
        baselines = forgotten,
        silences = expired,
        registre = ledger,
        "maintenance done"
    );
    Ok(())
}

/// Exécute un cycle complet et renvoie son bilan.
///
/// Testable indépendamment de la boucle : `now` et la source de métriques sont des
/// paramètres, si bien qu'un test peut rejouer une journée entière en quelques
/// millisecondes avec une source factice.
pub async fn evaluate_once(
    pool: &SqlitePool,
    cipher: &Cipher,
    source: &dyn MetricSource,
    http: &reqwest::Client,
    now: DateTime<Utc>,
) -> Result<EvalReport> {
    let rules = db::alerts::list_enabled_rules(pool).await?;
    if rules.is_empty() {
        return Ok(EvalReport::default());
    }

    let targets = db::alerts::list_target_nodes(pool).await?;
    let previous = db::alerts::load_states(pool).await?;
    let silences = db::alerts::list_silences(pool).await?;
    let overrides = policy_store::list_overrides(pool, None, None).await?;
    let mut baselines = db::alerts::load_baselines(pool, baseline::bucket_of(now)).await?;

    let mut report = EvalReport::default();
    let mut observations = Vec::with_capacity(rules.len());

    // Les requêtes sont séquentielles : un homelab a une poignée de règles, et
    // sérialiser garantit qu'on ne submerge jamais VictoriaMetrics au démarrage,
    // quand toutes les règles sont évaluées pour la première fois.
    for rule in rules {
        let series = match source.instant(&rule.query).await {
            Ok(series) => {
                report.rules_evaluated += 1;
                Some(series)
            }
            Err(error) => {
                report.rules_failed += 1;
                // `None` gèle les alertes de la règle : voir `RuleObservations`.
                warn!(rule = %rule.uid, ?error, "alerting query failed");
                None
            }
        };
        observations.push(RuleObservations { rule, series });
    }

    let outcome = cycle::plan_cycle(
        CycleInput { now, observations, targets, previous, silences, overrides },
        &mut baselines,
    );

    report.alerts_tracked = outcome.alerts.len();
    for alert in &outcome.alerts {
        if alert.state.phase.is_active() {
            report.firing += 1;
        }
        if alert.state.suppressed {
            report.suppressed += 1;
        }
        if alert.state.silenced {
            report.silenced += 1;
        }
        if alert.state.learning {
            report.learning += 1;
        }
    }
    report.groups = outcome.groups.len();

    db::alerts::save_states(pool, &outcome.alerts).await?;
    db::alerts::touch_states(pool, &outcome.carried_over, now).await?;
    db::alerts::save_baselines(pool, &baselines, now).await?;

    // Politique de notification : ce qui a le droit de parler passe encore par
    // les filtres de canal, les heures calmes, la fenêtre de regroupement et le
    // plafond horaire avant de partir (voir `notify_policy`).
    let channels = db::alerts::list_channels(pool, cipher).await?;
    let global = policy_store::load_global(pool).await?;
    let channel_policies = policy_store::load_channel_policies(pool).await?;
    let lookback = policy_store::lookback(&global, &channel_policies);
    let ledger = policy_store::load_ledger(pool, now, lookback).await?;
    let recipients: Vec<Recipient> = channels
        .iter()
        .map(|channel| Recipient {
            id: channel.id,
            kind: channel.kind.clone(),
            enabled: channel.enabled,
            policy: channel_policies.get(&channel.id).cloned().unwrap_or_default(),
        })
        .collect();
    let plan = notify_policy::plan(&outcome.groups, &recipients, &global, &ledger, now);
    policy_store::apply_plan(pool, &plan).await?;

    let env_url = crate::config::env_var("DUMBMONIT_PUBLIC_URL");
    let public_url = global.public_url(env_url.as_deref());
    let window = TimeDelta::seconds(i64::from(global.batch_window_secs));
    send_outgoing(
        pool,
        http,
        &channels,
        &plan.outgoing,
        public_url.as_deref(),
        window,
        now,
        &mut report,
    )
    .await;

    // L'historique explique pourquoi une transition n'est pas partie : la raison
    // du cycle (suppression, maintenance, apprentissage) prime, sinon celle de la
    // politique (battement, filtre de canal, délai minimal).
    let mut history = outcome.history;
    for entry in &mut history {
        if entry.reason.is_empty()
            && !plan.announced.contains(&entry.fingerprint)
            && let Some(verdict) = plan.verdicts.get(&entry.fingerprint)
        {
            entry.reason = verdict.clone();
        }
    }
    db::alerts::record_history(pool, &history, &plan.announced).await?;
    // Tout ce que la politique a pris en charge — parti, en attente ou écarté —
    // est marqué : le cycle suivant ne doit pas le représenter.
    let fingerprints: Vec<String> = plan.handled.into_iter().collect();
    db::alerts::mark_notified(pool, &fingerprints, now).await?;

    // La purge vient en dernier : elle s'appuie sur `last_eval_at`, que les écritures
    // précédentes viennent de poser sur toutes les empreintes encore vivantes.
    let purged = db::alerts::purge_states(pool, now).await?;
    if purged > 0 {
        debug!(purged, "stale fingerprints removed");
    }

    Ok(report)
}

/// Envoie les messages décidés par la politique et consigne le résultat.
///
/// Un envoi raté remet ses lignes en file : le cycle suivant retente, exactement
/// le comportement attendu d'une panne passagère du service de notification.
#[allow(clippy::too_many_arguments)]
async fn send_outgoing(
    pool: &SqlitePool,
    http: &reqwest::Client,
    channels: &[notify::ChannelConfig],
    outgoing: &[Outgoing],
    public_url: Option<&str>,
    window: TimeDelta,
    now: DateTime<Utc>,
    report: &mut EvalReport,
) {
    for out in outgoing {
        let Some(config) = channels.iter().find(|channel| channel.id == out.channel_id) else {
            continue;
        };
        let message = notify::render_digest(&out.digest, public_url);
        let delivery = notify::deliver(http, config, &message).await;

        if delivery.is_success() {
            report.notifications_sent += 1;
            if let Err(error) =
                policy_store::record_sent(pool, out.channel_id, &out.queue_ids, &out.sent, now)
                    .await
            {
                warn!(?error, "sent notification not recorded");
            }
        } else {
            report.notifications_failed += 1;
            if let Err(error) = policy_store::requeue(pool, &out.retry, window).await {
                warn!(?error, "failed notification not requeued");
            }
        }
        if let Err(error) = db::alerts::record_delivery(pool, &delivery, now).await {
            warn!(?error, "delivery report not recorded");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};

    use async_trait::async_trait;

    use super::*;
    use crate::alerting::model::RULE_HOST_DOWN;
    use crate::alerting::source::SeriesPoint;

    /// Source de métriques déterministe : le cycle complet — SQL compris — se teste
    /// donc sans VictoriaMetrics ni le moindre accès réseau.
    struct FakeSource {
        answers: Vec<(String, f64)>,
    }

    #[async_trait]
    impl MetricSource for FakeSource {
        async fn instant(&self, query: &str) -> Result<Vec<SeriesPoint>> {
            let Some((_, value)) =
                self.answers.iter().find(|(fragment, _)| query.contains(fragment.as_str()))
            else {
                return Ok(Vec::new());
            };
            let labels: BTreeMap<String, String> =
                [("__name__", "dumbmonit_test"), ("target", "1"), ("host", "nas")]
                    .iter()
                    .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                    .collect();
            Ok(vec![SeriesPoint { labels, value: *value, ts_ms: 0 }])
        }
    }

    /// Source qui échoue systématiquement, pour vérifier qu'une panne de la base de
    /// séries ne résout rien et ne fait pas tomber le cycle.
    struct BrokenSource;

    #[async_trait]
    impl MetricSource for BrokenSource {
        async fn instant(&self, _query: &str) -> Result<Vec<SeriesPoint>> {
            anyhow::bail!("VictoriaMetrics unreachable")
        }
    }

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    fn temp_db() -> PathBuf {
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir()
            .join(format!("dumbmonit-alerting-{}-{unique}/dumbmonit.db", std::process::id()))
    }

    /// Monte une base réelle : migrations appliquées, une cible, les règles livrées.
    async fn fixture() -> (PathBuf, SqlitePool, Cipher) {
        let path = temp_db();
        let pool = db::open(&path).await.expect("open test database");
        let cipher =
            db::init_cipher(&pool, "a-test-secret-that-is-long-enough").await.expect("test cipher");

        sqlx::query(
            "INSERT INTO targets (id, name, address, kind, parent_id, tags)
             VALUES (1, 'nas', '10.0.0.1', 'dummy', NULL, '{\"role\":\"storage\"}')",
        )
        .execute(&pool)
        .await
        .expect("insert test target");

        db::alerts::seed_builtin_rules(&pool).await.expect("built-in rules");
        (path, pool, cipher)
    }

    async fn cleanup(path: &std::path::Path, pool: SqlitePool) {
        pool.close().await;
        if let Some(parent) = path.parent() {
            let _ = std::fs::remove_dir_all(parent);
        }
    }

    fn at(seconds: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_700_000_000 + seconds, 0).expect("valid timestamp")
    }

    #[tokio::test]
    async fn les_regles_livrees_sont_installees_une_seule_fois() {
        let (path, pool, _cipher) = fixture().await;

        let rules = db::alerts::list_enabled_rules(&pool).await.expect("read rules");
        assert!(rules.iter().any(|rule| rule.uid == RULE_HOST_DOWN));
        assert!(rules.iter().any(|rule| rule.uid == "disk_almost_full"));

        // Deuxième appel : aucune règle n'est réinsérée ni réécrite.
        let inserted = db::alerts::seed_builtin_rules(&pool).await.expect("second call");
        assert_eq!(inserted, 0);

        cleanup(&path, pool).await;
    }

    #[tokio::test]
    async fn un_cycle_complet_traverse_la_base_et_fait_avancer_l_etat() {
        let (path, pool, cipher) = fixture().await;
        let http = notify::http_client();

        // La cible est muette depuis dix minutes : la règle « injoignable » doit
        // passer en `pending`, son `for` étant d'une minute.
        let source = FakeSource { answers: vec![("tlast_over_time".to_string(), 600.0)] };

        let report =
            evaluate_once(&pool, &cipher, &source, &http, at(0)).await.expect("first cycle");
        assert!(report.rules_evaluated > 0);
        assert_eq!(report.rules_failed, 0);
        assert_eq!(report.firing, 0, "the one-minute `for` has not elapsed");
        assert_eq!(report.groups, 0);

        // Une minute plus tard, la condition tient : l'alerte part.
        let report =
            evaluate_once(&pool, &cipher, &source, &http, at(60)).await.expect("second cycle");
        assert_eq!(report.firing, 1);
        assert_eq!(report.groups, 1);

        let actives = db::alerts::list_active(&pool).await.expect("active alerts");
        let injoignable = actives
            .iter()
            .find(|alert| alert.rule_uid == RULE_HOST_DOWN)
            .expect("\"unreachable\" alert persisted");
        assert_eq!(injoignable.target_id, Some(1));
        assert_eq!(injoignable.state.notify_count, 1, "recorded despite the absence of a channel");

        cleanup(&path, pool).await;
    }

    #[tokio::test]
    async fn une_panne_de_la_base_de_series_ne_resout_aucune_alerte() {
        let (path, pool, cipher) = fixture().await;
        let http = notify::http_client();
        let source = FakeSource { answers: vec![("tlast_over_time".to_string(), 600.0)] };

        evaluate_once(&pool, &cipher, &source, &http, at(0)).await.expect("initial cycle");
        evaluate_once(&pool, &cipher, &source, &http, at(60)).await.expect("trigger");
        assert_eq!(db::alerts::list_active(&pool).await.unwrap().len(), 1);

        // VictoriaMetrics tombe : l'alerte est gelée, ni résolue ni purgée.
        let report = evaluate_once(&pool, &cipher, &BrokenSource, &http, at(120))
            .await
            .expect("the cycle does not propagate query failures");
        assert!(report.rules_failed > 0);
        assert_eq!(report.groups, 0, "no misleading \"all clear\"");

        let actives = db::alerts::list_active(&pool).await.expect("active alerts");
        assert_eq!(actives.len(), 1, "the alert survives the outage");
        assert_eq!(actives[0].state.phase, crate::alerting::Phase::Firing);

        cleanup(&path, pool).await;
    }

    #[tokio::test]
    async fn les_secrets_d_un_canal_font_l_aller_retour_chiffres() {
        let (path, pool, cipher) = fixture().await;

        let id = db::alerts::upsert_channel(
            &pool,
            &cipher,
            &db::alerts::ChannelDraft {
                id: None,
                name: "home ntfy".to_string(),
                kind: "ntfy".to_string(),
                enabled: true,
                settings: serde_json::json!({"topic": "homelab"}),
                secrets: Some(serde_json::json!({"token": "tk_secret_very_long"})),
            },
        )
        .await
        .expect("create channel");

        let channels = db::alerts::list_channels(&pool, &cipher).await.expect("read");
        let channel = channels.iter().find(|c| c.id == id).expect("channel present");
        assert_eq!(
            channel.secret("token").expect("readable secret").expose(),
            "tk_secret_very_long"
        );

        // Le secret n'est pas stocké en clair : la colonne chiffrée ne le contient pas.
        let row = sqlx::query("SELECT secret_enc FROM notification_channels WHERE id = ?")
            .bind(id)
            .fetch_one(&pool)
            .await
            .expect("raw read");
        let raw: Vec<u8> = sqlx::Row::try_get(&row, "secret_enc").expect("encrypted column");
        assert!(
            !String::from_utf8_lossy(&raw).contains("tk_secret_very_long"),
            "the token must never reach the disk in clear text"
        );

        // Une mise à jour sans secret conserve celui déjà enregistré.
        db::alerts::upsert_channel(
            &pool,
            &cipher,
            &db::alerts::ChannelDraft {
                id: Some(id),
                name: "ntfy renamed".to_string(),
                kind: "ntfy".to_string(),
                enabled: true,
                settings: serde_json::json!({"topic": "homelab"}),
                secrets: None,
            },
        )
        .await
        .expect("rename");
        let channels = db::alerts::list_channels(&pool, &cipher).await.expect("re-read");
        let channel = channels.iter().find(|c| c.id == id).expect("channel present");
        assert_eq!(channel.name, "ntfy renamed");
        assert_eq!(channel.secret("token").expect("secret kept").expose(), "tk_secret_very_long");

        cleanup(&path, pool).await;
    }

    #[tokio::test]
    async fn les_baselines_sont_persistees_et_relues() {
        let (path, pool, cipher) = fixture().await;
        let http = notify::http_client();

        // Une règle d'anomalie sur une requête que la source factice reconnaît.
        let mut rule = crate::alerting::rules::builtin_rules()
            .into_iter()
            .find(|rule| rule.uid == "cpu_anomaly")
            .expect("built-in anomaly rule");
        rule.query = "anomaly_test".to_string();
        db::alerts::upsert_rule(&pool, &rule).await.expect("update query");

        let source = FakeSource { answers: vec![("anomaly_test".to_string(), 50.0)] };
        evaluate_once(&pool, &cipher, &source, &http, at(0)).await.expect("cycle");

        let store = db::alerts::load_baselines(&pool, baseline::bucket_of(at(0)))
            .await
            .expect("re-read baselines");
        let key = "dumbmonit_test{host=\"nas\",target=\"1\"}";
        let bucket = store.bucket(key, baseline::bucket_of(at(0))).expect("bucket persisted");
        assert_eq!(bucket.samples, 1);
        assert_eq!(bucket.ewma, 50.0);
        assert!(store.series(key).is_some(), "the series is dated for learning");

        cleanup(&path, pool).await;
    }

    #[test]
    fn la_periode_d_evaluation_ne_descend_pas_sous_dix_secondes() {
        // SAFETY : les tests d'un même binaire partagent l'environnement du processus ;
        // ce test ne lit qu'une variable qu'aucun autre n'utilise.
        unsafe {
            std::env::set_var("DUMBMONIT_ALERT_INTERVAL_SECS", "1");
        }
        assert_eq!(AlertingConfig::from_env().interval, Duration::from_secs(10));

        unsafe {
            std::env::set_var("DUMBMONIT_ALERT_INTERVAL_SECS", "120");
        }
        assert_eq!(AlertingConfig::from_env().interval, Duration::from_secs(120));

        unsafe {
            std::env::remove_var("DUMBMONIT_ALERT_INTERVAL_SECS");
        }
        assert_eq!(AlertingConfig::from_env().interval, Duration::from_secs(30));
    }

    #[test]
    fn une_valeur_d_environnement_absurde_retombe_sur_le_defaut() {
        unsafe {
            std::env::set_var("DUMBMONIT_ALERT_HISTORY_DAYS", "zero");
        }
        let config = AlertingConfig::from_env();
        assert_eq!(config.history_retention, AlertingConfig::default().history_retention);
        unsafe {
            std::env::remove_var("DUMBMONIT_ALERT_HISTORY_DAYS");
        }
    }
}
