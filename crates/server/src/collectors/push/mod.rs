//! Collecteur « push » : le moniteur en poussée, ou interrupteur d'homme mort.
//!
//! La panne silencieuse la plus courante chez soi n'est pas un équipement qui ne
//! répond plus : c'est un travail qui ne tourne plus. Le cron de sauvegarde
//! désactivé par une mise à jour, le script qui plante avant d'écrire, la
//! routine domotique cassée par un renommage — rien ne se plaint, et on s'en
//! aperçoit le jour où l'on a besoin de la sauvegarde.
//!
//! Le principe est celui d'healthchecks.io ou du moniteur « push » d'Uptime Kuma :
//! le travail appelle une URL secrète à chaque exécution (`api/push.rs`),
//! et c'est *l'absence* d'appel qui déclenche l'alerte. Comme le collecteur
//! « agent », `probe` n'interroge rien : il relit la date du dernier appel et
//! la compare à la période attendue.
//!
//! # Pourquoi `probe_success` et non une série qui s'interrompt
//!
//! Le socle détecte une cible injoignable par l'interruption de sa série `up`.
//! Ici, la cible n'est pas un équipement : `probe` renvoie toujours `Ok` (le
//! contrôle a tourné), et c'est `probe_success` qui porte le verdict, comme pour
//! les moniteurs de disponibilité. On obtient ainsi un taux de disponibilité
//! (`avg_over_time`), un historique daté des retards, et l'alerte « équipement
//! injoignable » ne se déclenche jamais en doublon : la règle livrée
//! `push_missed` est la seule à parler.
//!
//! # Métriques produites
//!
//! Étiquetées `probe="push"` en plus des étiquettes d'identité :
//!
//! | Métrique | Sens |
//! |---|---|
//! | `probe_success` | 1 si le dernier appel est dans les temps et n'a pas signalé d'échec, 0 sinon. Absente tant qu'aucun appel n'a été reçu. |
//! | `probe_failure_info` | Présence ; `reason` vaut `missed` (appel en retard) ou `reported_down` (le script a dit `status=down`). |
//! | `push_last_seen_seconds` | Âge du dernier appel. |
//! | `push_received_total` | Nombre d'appels reçus depuis la création du jeton. |

pub mod store;
pub mod token;

use std::time::Duration;

use async_trait::async_trait;
use dumbmonit_proto::{Collector, MetricKind, ProbeError, Sample, Target};
use sqlx::SqlitePool;

pub use store::{Monitor, Status};

/// Type de cible, tel qu'enregistré dans `Target::kind`.
pub const KIND: &str = "push";

/// Étiquette d'option : période attendue entre deux appels.
pub const OPTION_EXPECTED_INTERVAL: &str = "expected_interval";
/// Étiquette d'option : tolérance au-delà de la période avant de conclure au
/// retard — une durée (`15m`) ou un pourcentage de la période (`10%`).
pub const OPTION_GRACE: &str = "grace";

pub const DEFAULT_EXPECTED_INTERVAL: &str = "24h";
pub const DEFAULT_GRACE: &str = "10%";

/// Période attendue minimale. En dessous, ce n'est plus un travail planifié
/// qu'on surveille mais un flux, et le contrôle de fraîcheur (à la période de la
/// cible, dix secondes au mieux) ne suivrait pas.
const MIN_EXPECTED: Duration = Duration::from_secs(10);
/// Période attendue maximale : un an. Au-delà, l'option est plus sûrement une
/// faute de frappe qu'un travail annuel.
const MAX_EXPECTED: Duration = Duration::from_secs(366 * 24 * 3600);
/// Tolérance plancher. Un cron « toutes les cinq minutes » avec dix pour cent
/// de tolérance serait déclaré en retard pour trente secondes de dérive — la
/// charge de la machine à ce moment-là, rien de plus.
const MIN_GRACE: Duration = Duration::from_secs(60);

/// Réglages d'un moniteur, lus dans les étiquettes de la cible.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Settings {
    pub expected: Duration,
    pub grace: Duration,
}

impl Settings {
    /// Âge du dernier appel au-delà duquel le travail est en retard.
    pub fn deadline(self) -> Duration {
        self.expected + self.grace
    }

    pub fn from_target(target: &Target) -> Result<Self, ProbeError> {
        let tag = |key: &str| {
            target.tags.get(key).map(|value| value.trim()).filter(|value| !value.is_empty())
        };
        let expected = match tag(OPTION_EXPECTED_INTERVAL) {
            None => parse_duration(DEFAULT_EXPECTED_INTERVAL).expect("default interval"),
            Some(raw) => parse_duration(raw).ok_or_else(|| {
                ProbeError::Config(format!(
                    "\"{OPTION_EXPECTED_INTERVAL}\" expects a duration such as 30m, 1h or 2d, got \"{raw}\""
                ))
            })?,
        };
        if !(MIN_EXPECTED..=MAX_EXPECTED).contains(&expected) {
            return Err(ProbeError::Config(format!(
                "\"{OPTION_EXPECTED_INTERVAL}\" must be between {} s and {} days",
                MIN_EXPECTED.as_secs(),
                MAX_EXPECTED.as_secs() / 86_400
            )));
        }
        let raw_grace = tag(OPTION_GRACE).unwrap_or(DEFAULT_GRACE);
        let grace = parse_grace(raw_grace, expected).ok_or_else(|| {
            ProbeError::Config(format!(
                "\"{OPTION_GRACE}\" expects a duration (15m) or a percentage of the interval (10%), got \"{raw_grace}\""
            ))
        })?;
        Ok(Self { expected, grace: grace.max(MIN_GRACE) })
    }
}

/// Lit une durée courte : un entier (secondes) ou des composantes suffixées —
/// `90`, `30s`, `15m`, `1h`, `2d`, `1h30m`. Les espaces sont tolérés.
pub fn parse_duration(raw: &str) -> Option<Duration> {
    let compact: String = raw.chars().filter(|c| !c.is_whitespace()).collect();
    if compact.is_empty() {
        return None;
    }
    if let Ok(seconds) = compact.parse::<u64>() {
        return Some(Duration::from_secs(seconds));
    }
    let mut total: u64 = 0;
    let mut number = String::new();
    for c in compact.chars() {
        if c.is_ascii_digit() {
            number.push(c);
            continue;
        }
        let unit: u64 = match c {
            's' | 'S' => 1,
            'm' | 'M' => 60,
            'h' | 'H' => 3600,
            'd' | 'D' => 86_400,
            'w' | 'W' => 7 * 86_400,
            _ => return None,
        };
        let value: u64 = number.parse().ok()?;
        number.clear();
        total = total.checked_add(value.checked_mul(unit)?)?;
    }
    if !number.is_empty() {
        return None; // un nombre sans unité après une composante : « 1h30 »
    }
    Some(Duration::from_secs(total))
}

/// Lit la tolérance : un pourcentage de la période, ou une durée.
fn parse_grace(raw: &str, expected: Duration) -> Option<Duration> {
    let trimmed = raw.trim();
    if let Some(percent) = trimmed.strip_suffix('%') {
        let percent: u64 = percent.trim().parse().ok()?;
        if percent > 1000 {
            return None;
        }
        return Some(Duration::from_secs(expected.as_secs() * percent / 100));
    }
    parse_duration(trimmed)
}

/// Ce que le contrôle de fraîcheur conclut.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Aucun appel reçu depuis la création du jeton : le script n'est pas encore
    /// en place. Ni en panne ni en bonne santé — rien n'est écrit.
    Waiting,
    /// Le dernier appel est dans les temps.
    OnTime,
    /// Le dernier appel est plus vieux que la période attendue plus la tolérance.
    Missed,
    /// Le dernier appel a lui-même signalé un échec (`status=down`).
    ReportedDown,
}

impl Verdict {
    /// Motif d'échec, en jeton d'étiquette ; `None` quand tout va bien.
    pub fn failure_reason(self) -> Option<&'static str> {
        match self {
            Self::Missed => Some("missed"),
            Self::ReportedDown => Some("reported_down"),
            Self::Waiting | Self::OnTime => None,
        }
    }
}

/// Juge un moniteur d'après son dernier appel.
///
/// Un appel qui signale `down` l'emporte sur l'horloge : le script a tourné et
/// dit lui-même que ça s'est mal passé, inutile d'attendre qu'il soit en retard.
pub fn evaluate(
    last_seen_ms: Option<i64>,
    status: Status,
    now_ms: i64,
    settings: Settings,
) -> Verdict {
    let Some(last_seen_ms) = last_seen_ms else { return Verdict::Waiting };
    if status == Status::Down {
        return Verdict::ReportedDown;
    }
    if age(last_seen_ms, now_ms) > settings.deadline() { Verdict::Missed } else { Verdict::OnTime }
}

/// Âge d'un instant, jamais négatif — une horloge qui recule ne doit pas
/// produire une durée absurde.
pub fn age(then_ms: i64, now_ms: i64) -> Duration {
    Duration::from_millis(now_ms.saturating_sub(then_ms).max(0) as u64)
}

pub struct PushCollector {
    pool: SqlitePool,
}

impl PushCollector {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl Collector for PushCollector {
    fn kind(&self) -> &'static str {
        KIND
    }

    async fn probe(&self, target: &Target) -> Result<Vec<Sample>, ProbeError> {
        let settings = Settings::from_target(target)?;
        let now_ms = chrono::Utc::now().timestamp_millis();
        // Une cible sans moniteur (créée par un client qui ignore ce type) est
        // simplement en attente : la page de l'équipement créera le jeton.
        let freshness = store::freshness(&self.pool, target.id).await.map_err(ProbeError::Other)?;
        let (last_seen_ms, status, received_total) = match freshness {
            Some(f) => (f.last_seen_ms, f.last_status, f.received_total),
            None => (None, Status::Up, 0),
        };

        let verdict = evaluate(last_seen_ms, status, now_ms, settings);
        let gauge = |metric: &str, value: f64| {
            Sample::new(metric, value, MetricKind::Gauge, now_ms).with_label("probe", KIND)
        };
        let mut samples = vec![
            Sample::new("push_received_total", received_total as f64, MetricKind::Counter, now_ms)
                .with_label("probe", KIND),
        ];
        let Some(last_seen_ms) = last_seen_ms else {
            // En attente du premier appel : ni succès ni échec, `up` seul est posé
            // par le registre. L'interface affiche « Waiting ».
            return Ok(samples);
        };
        samples.push(gauge("push_last_seen_seconds", age(last_seen_ms, now_ms).as_secs_f64()));
        samples.push(gauge("probe_success", if verdict == Verdict::OnTime { 1.0 } else { 0.0 }));
        if let Some(reason) = verdict.failure_reason() {
            samples.push(gauge("probe_failure_info", 1.0).with_label("reason", reason));
        }
        Ok(samples)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use dumbmonit_proto::{Credential, TargetId};

    use super::*;

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    fn target(tags: &[(&str, &str)]) -> Target {
        Target {
            id: 1,
            name: "nightly backup".into(),
            address: "nightly-backup".into(),
            kind: KIND.into(),
            profile_id: None,
            parent_id: None,
            interval: secs(60),
            enabled: true,
            tags: tags
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect::<BTreeMap<_, _>>(),
            credential: Credential::None,
        }
    }

    #[test]
    fn les_durees_se_lisent_en_secondes_ou_avec_suffixes() {
        assert_eq!(parse_duration("90"), Some(secs(90)));
        assert_eq!(parse_duration("30s"), Some(secs(30)));
        assert_eq!(parse_duration("15m"), Some(secs(900)));
        assert_eq!(parse_duration("1h"), Some(secs(3600)));
        assert_eq!(parse_duration("24H"), Some(secs(86_400)));
        assert_eq!(parse_duration("2d"), Some(secs(172_800)));
        assert_eq!(parse_duration("1w"), Some(secs(604_800)));
        assert_eq!(parse_duration("1h 30m"), Some(secs(5400)));
        assert_eq!(parse_duration(""), None);
        assert_eq!(parse_duration("abc"), None);
        assert_eq!(parse_duration("1h30"), None, "composante sans unité");
        assert_eq!(parse_duration("-5m"), None);
        assert_eq!(parse_duration("1.5h"), None);
    }

    #[test]
    fn la_tolerance_est_un_pourcentage_ou_une_duree_avec_un_plancher() {
        let s = Settings::from_target(&target(&[])).unwrap();
        assert_eq!(s.expected, secs(86_400), "défaut : un jour");
        assert_eq!(s.grace, secs(8640), "10 % d'un jour");

        let s = Settings::from_target(&target(&[("expected_interval", "1h"), ("grace", "15m")]))
            .unwrap();
        assert_eq!(s.grace, secs(900));
        assert_eq!(s.deadline(), secs(4500));

        // Dix pour cent de cinq minutes font trente secondes : le plancher l'emporte.
        let s = Settings::from_target(&target(&[("expected_interval", "5m")])).unwrap();
        assert_eq!(s.grace, MIN_GRACE);

        let s =
            Settings::from_target(&target(&[("expected_interval", "1h"), ("grace", "0")])).unwrap();
        assert_eq!(s.grace, MIN_GRACE, "zéro reste borné par le plancher");
    }

    #[test]
    fn une_option_illisible_est_une_erreur_de_configuration_et_non_une_panne() {
        for tags in [
            [("expected_interval", "soon"), ("grace", "10%")],
            [("expected_interval", "1h"), ("grace", "beaucoup")],
            [("expected_interval", "5s"), ("grace", "10%")],
            [("expected_interval", "400d"), ("grace", "10%")],
            [("expected_interval", "1h"), ("grace", "5000%")],
        ] {
            let error = Settings::from_target(&target(&tags)).unwrap_err();
            assert!(!error.means_down(), "{tags:?} : {error}");
            assert!(matches!(error, ProbeError::Config(_)), "{tags:?} : {error}");
        }
    }

    #[test]
    fn le_verdict_suit_lage_du_dernier_appel() {
        let settings = Settings { expected: secs(3600), grace: secs(600) };
        let now = 10_000_000_000;
        assert_eq!(evaluate(None, Status::Up, now, settings), Verdict::Waiting);
        assert_eq!(evaluate(Some(now - 5_000), Status::Up, now, settings), Verdict::OnTime);
        // Dans la tolérance : en retard de cinq minutes sur une heure, dix tolérées.
        assert_eq!(evaluate(Some(now - 3_900_000), Status::Up, now, settings), Verdict::OnTime);
        // Juste au-delà de la période plus la tolérance.
        assert_eq!(evaluate(Some(now - 4_200_001), Status::Up, now, settings), Verdict::Missed);
        // Un appel qui déclare `down` n'attend pas l'horloge.
        assert_eq!(evaluate(Some(now - 1_000), Status::Down, now, settings), Verdict::ReportedDown);
        // Une horloge qui recule ne donne pas un âge négatif.
        assert_eq!(evaluate(Some(now + 60_000), Status::Up, now, settings), Verdict::OnTime);
    }

    #[test]
    fn les_motifs_dechec_sont_des_jetons_de_requete() {
        for verdict in [Verdict::Missed, Verdict::ReportedDown] {
            let reason = verdict.failure_reason().unwrap();
            assert!(reason.chars().all(|c| c.is_ascii_lowercase() || c == '_'), "{reason}");
        }
        assert_eq!(Verdict::OnTime.failure_reason(), None);
        assert_eq!(Verdict::Waiting.failure_reason(), None);
    }

    /// Base vierge avec une cible `push` ; renvoie aussi son identifiant.
    async fn pool() -> (SqlitePool, crate::crypto::Cipher, TargetId, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("répertoire temporaire");
        let pool = crate::db::open(&dir.path().join("test.db")).await.expect("base");
        let cipher = crate::db::init_cipher(&pool, "secret-de-test-suffisamment-long")
            .await
            .expect("chiffrement");
        let id = crate::db::targets::create(
            &pool,
            &cipher,
            &crate::db::targets::TargetInput {
                name: "nightly backup".into(),
                address: "nightly-backup".into(),
                kind: KIND.into(),
                profile_id: None,
                parent_id: None,
                via_agent: None,
                interval: secs(60),
                enabled: true,
                tags: Default::default(),
                credential: None,
            },
        )
        .await
        .expect("cible");
        (pool, cipher, id, dir)
    }

    #[tokio::test]
    async fn une_cible_jamais_appelee_attend_sans_ecrire_de_verdict() {
        let (pool, cipher, id, _dir) = pool().await;
        let target = Target { id, ..target(&[("expected_interval", "1h")]) };

        // Sans moniteur du tout, puis avec un moniteur jamais appelé : même chose.
        for _ in 0..2 {
            let samples = PushCollector::new(pool.clone()).probe(&target).await.expect("ok");
            assert!(!samples.iter().any(|s| s.metric == "probe_success"), "{samples:?}");
            let total = samples.iter().find(|s| s.metric == "push_received_total").unwrap();
            assert_eq!(total.value, 0.0);
            store::ensure(&pool, &cipher, target.id).await.expect("moniteur");
        }
    }

    #[tokio::test]
    async fn un_appel_recent_est_un_succes_et_un_appel_ancien_un_echec_date() {
        let (pool, cipher, id, _dir) = pool().await;
        let target = Target { id, ..target(&[("expected_interval", "1h"), ("grace", "5m")]) };
        let monitor = store::ensure(&pool, &cipher, target.id).await.expect("moniteur");
        let hash = token::fingerprint(&monitor.token);
        let now = chrono::Utc::now().timestamp_millis();

        store::record_ping(&pool, &hash, now - 30_000, Status::Up, "").await.unwrap();
        let samples = PushCollector::new(pool.clone()).probe(&target).await.expect("ok");
        let success = samples.iter().find(|s| s.metric == "probe_success").expect("verdict");
        assert_eq!(success.value, 1.0);
        assert_eq!(success.labels.get("probe").map(String::as_str), Some(KIND));
        let age = samples.iter().find(|s| s.metric == "push_last_seen_seconds").unwrap();
        assert!((29.0..=40.0).contains(&age.value), "âge : {}", age.value);
        assert!(!samples.iter().any(|s| s.metric == "probe_failure_info"));

        // Deux heures de silence sur une période d'une heure : en retard, et le
        // point à zéro est bien écrit — c'est lui que la règle `push_missed` lit.
        store::record_ping(&pool, &hash, now - 7_200_000, Status::Up, "").await.unwrap();
        let samples = PushCollector::new(pool.clone()).probe(&target).await.expect("ok");
        assert_eq!(samples.iter().find(|s| s.metric == "probe_success").unwrap().value, 0.0);
        let info = samples.iter().find(|s| s.metric == "probe_failure_info").expect("motif");
        assert_eq!(info.labels.get("reason").map(String::as_str), Some("missed"));
        let total = samples.iter().find(|s| s.metric == "push_received_total").unwrap();
        assert_eq!(total.value, 2.0);

        // Le script signale lui-même un échec.
        store::record_ping(&pool, &hash, now, Status::Down, "rsync exit 23").await.unwrap();
        let samples = PushCollector::new(pool.clone()).probe(&target).await.expect("ok");
        assert_eq!(samples.iter().find(|s| s.metric == "probe_success").unwrap().value, 0.0);
        let info = samples.iter().find(|s| s.metric == "probe_failure_info").expect("motif");
        assert_eq!(info.labels.get("reason").map(String::as_str), Some("reported_down"));
    }

    #[tokio::test]
    async fn regenerer_le_jeton_coupe_lancienne_url_et_garde_le_compteur() {
        let (pool, cipher, id, _dir) = pool().await;
        let before = store::ensure(&pool, &cipher, id).await.expect("moniteur");
        let now = chrono::Utc::now().timestamp_millis();
        let old_hash = token::fingerprint(&before.token);
        assert_eq!(
            store::record_ping(&pool, &old_hash, now, Status::Up, "").await.unwrap(),
            Some(id)
        );

        let after = store::regenerate(&pool, &cipher, id).await.expect("régénération");
        assert_ne!(after.token, before.token);
        assert_eq!(after.received_total, 1);
        assert_eq!(store::record_ping(&pool, &old_hash, now, Status::Up, "").await.unwrap(), None);
        let new_hash = token::fingerprint(&after.token);
        assert_eq!(
            store::record_ping(&pool, &new_hash, now, Status::Up, "").await.unwrap(),
            Some(id)
        );
        assert_eq!(store::get(&pool, &cipher, id).await.unwrap().unwrap().received_total, 2);
    }

    #[tokio::test]
    async fn le_message_dun_appel_est_tronque() {
        let (pool, cipher, id, _dir) = pool().await;
        let monitor = store::ensure(&pool, &cipher, id).await.expect("moniteur");
        let hash = token::fingerprint(&monitor.token);
        let long = "x".repeat(store::MAX_MESSAGE_CHARS * 3);
        store::record_ping(&pool, &hash, 0, Status::Up, &long).await.unwrap();
        let stored = store::get(&pool, &cipher, id).await.unwrap().unwrap();
        assert_eq!(stored.last_message.chars().count(), store::MAX_MESSAGE_CHARS);
    }
}
