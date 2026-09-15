//! Collecteur « agent » : les mesures arrivent en poussée, elles ne sont pas
//! cherchées.
//!
//! Ce collecteur est particulier : il n'interroge rien. L'agent installé sur la
//! machine surveillée pousse ses relevés sur `/api/ingest`, et [`receive::ingest`]
//! les range. Que reste-t-il à faire à `probe` ?
//!
//! Le socle considère qu'une cible est injoignable quand sa série s'interrompt :
//! le registre écrit `up` après chaque interrogation réussie, et n'écrit rien
//! quand elle échoue. Un collecteur en poussée qui renverrait toujours `Ok`
//! écrirait donc `up 1` éternellement, y compris pour une machine éteinte depuis
//! trois jours — l'alerte « équipement injoignable » ne se déclencherait jamais
//! pour la moitié du parc.
//!
//! `probe` est donc un **contrôle de fraîcheur** : il relit la date du dernier lot
//! reçu et échoue en `Unreachable` quand elle est trop ancienne. Aucun accès
//! réseau, une seule lecture indexée en base, et la même sémantique que pour un
//! équipement SNMP — c'est ce qui permet aux règles d'alerte, à l'anti-cascade et
//! à l'interface de traiter les deux mondes sans distinction.

pub mod commands;
pub mod policy;
mod receive;
mod store;
mod token;

use std::time::Duration;

use async_trait::async_trait;
use ezymonit_proto::{Collector, MetricKind, ProbeError, Sample, Target};
use sqlx::SqlitePool;

pub use policy::spawn_policy_scheduler;
pub use receive::{IngestError, ingest};
pub use store::{TokenRecord, create_token, list_tokens, revoke_token};

/// Vérifie un en-tête `Authorization` d'agent et renvoie l'identifiant du jeton.
///
/// Partagé entre l'ingestion et le canal de commandes : les deux routes sont
/// ouvertes aux machines distantes et se protègent de la même façon.
pub async fn authenticate_token(
    pool: &SqlitePool,
    bearer: Option<&str>,
) -> Result<Option<i64>, anyhow::Error> {
    let Some(presented) = token::extract_bearer(bearer) else {
        return Ok(None);
    };
    store::find_active_token(pool, &token::fingerprint(presented)).await
}

/// Note l'usage d'un jeton (canal de commandes).
pub async fn touch_token(pool: &SqlitePool, id: i64) -> Result<(), anyhow::Error> {
    store::touch_token(pool, id).await
}

/// Tolérance minimale avant de déclarer une machine muette.
///
/// Un agent qui échantillonne toutes les dix secondes peut manquer un envoi sans
/// que rien n'aille mal : une coupure Wi-Fi de trente secondes ne doit pas
/// réveiller quelqu'un la nuit.
const MIN_GRACE: Duration = Duration::from_secs(90);

/// Nombre de périodes manquées avant de conclure à une machine muette.
const MISSED_INTERVALS: u32 = 3;

pub struct AgentCollector {
    pool: SqlitePool,
}

impl AgentCollector {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

/// Ancienneté au-delà de laquelle une machine est déclarée injoignable.
///
/// Proportionnelle à la période de la cible : une machine échantillonnée toutes
/// les cinq minutes ne doit pas être déclarée morte au bout de quatre-vingt-dix
/// secondes de silence.
fn staleness_deadline(interval: Duration) -> Duration {
    (interval * MISSED_INTERVALS).max(MIN_GRACE)
}

#[async_trait]
impl Collector for AgentCollector {
    fn kind(&self) -> &'static str {
        "agent"
    }

    async fn probe(&self, target: &Target) -> Result<Vec<Sample>, ProbeError> {
        let last_seen = store::last_seen_ms(&self.pool, target.id)
            .await
            .map_err(ProbeError::Other)?
            .ok_or_else(|| {
                ProbeError::Unreachable(
                    "no measurement received from this agent: check that it is running \
                     and that the server URL it uses is reachable"
                        .to_string(),
                )
            })?;

        let now_ms = chrono::Utc::now().timestamp_millis();
        let age = Duration::from_millis(now_ms.saturating_sub(last_seen).max(0) as u64);
        let deadline = staleness_deadline(target.interval);

        if age > deadline {
            return Err(ProbeError::Unreachable(format!(
                "no measurement for {} s (limit {} s)",
                age.as_secs(),
                deadline.as_secs()
            )));
        }

        // Le registre pose `up` lui-même ; on n'ajoute que ce qu'il ne sait pas :
        // depuis combien de temps cette machine s'est manifestée. C'est la mesure
        // qui rend visible un agent qui ralentit avant qu'il ne disparaisse.
        Ok(vec![Sample::new(
            "agent_last_batch_age_seconds",
            age.as_secs_f64(),
            MetricKind::Gauge,
            now_ms,
        )])
    }
}

#[cfg(test)]
mod tests {
    use ezymonit_proto::Credential;

    use super::*;

    fn target(interval_secs: u64) -> Target {
        Target {
            id: 1,
            name: "nas".into(),
            address: "id-nas".into(),
            kind: "agent".into(),
            profile_id: None,
            parent_id: None,
            interval: Duration::from_secs(interval_secs),
            enabled: true,
            tags: Default::default(),
            credential: Credential::None,
        }
    }

    async fn pool_with_agent(last_seen_ms: Option<i64>) -> (SqlitePool, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("répertoire temporaire");
        let pool = crate::db::open(&dir.path().join("test.db")).await.expect("base");
        let cipher = crate::db::init_cipher(&pool, "secret-de-test-suffisamment-long")
            .await
            .expect("chiffrement");

        let (token_record, _) = store::create_token(&pool, "parc").await.expect("jeton");
        let identity = ezymonit_proto::AgentIdentity {
            hostname: "nas".into(),
            os: "linux".into(),
            os_version: None,
            kernel_version: None,
            arch: None,
            agent_version: "0.1.0".into(),
            machine_id: Some("id-nas".into()),
            tags: Default::default(),
        };
        let registration =
            store::register(&pool, &cipher, &identity, token_record.id).await.expect("machine");

        if let Some(ts) = last_seen_ms {
            store::record_batch(&pool, registration.target_id, ts, 10).await.expect("lot");
        }
        (pool, dir)
    }

    #[test]
    fn the_tolerance_follows_the_target_period() {
        // Trois périodes manquées, mais jamais moins que la tolérance minimale.
        assert_eq!(staleness_deadline(Duration::from_secs(10)), MIN_GRACE);
        assert_eq!(staleness_deadline(Duration::from_secs(30)), MIN_GRACE);
        assert_eq!(staleness_deadline(Duration::from_secs(300)), Duration::from_secs(900));
    }

    #[tokio::test]
    async fn a_fresh_agent_is_reachable_and_reports_its_age() {
        let now = chrono::Utc::now().timestamp_millis();
        let (pool, _dir) = pool_with_agent(Some(now - 5_000)).await;

        let samples = AgentCollector::new(pool).probe(&target(30)).await.expect("cible joignable");
        let age = samples.iter().find(|s| s.metric == "agent_last_batch_age_seconds").expect("âge");
        assert!((4.0..=10.0).contains(&age.value), "âge inattendu : {}", age.value);
    }

    #[tokio::test]
    async fn a_silent_agent_is_reported_unreachable() {
        let now = chrono::Utc::now().timestamp_millis();
        let (pool, _dir) = pool_with_agent(Some(now - 600_000)).await;

        let error = AgentCollector::new(pool).probe(&target(30)).await.unwrap_err();
        // C'est cette qualification qui alimente l'alerte « équipement injoignable »
        // et l'anti-cascade : une erreur de configuration ne les déclencherait pas.
        assert!(error.means_down(), "erreur inattendue : {error}");
    }

    #[tokio::test]
    async fn an_agent_that_has_never_pushed_is_not_declared_up() {
        let (pool, _dir) = pool_with_agent(None).await;

        let error = AgentCollector::new(pool).probe(&target(30)).await.unwrap_err();
        assert!(error.means_down());
        assert!(error.to_string().contains("no measurement received"), "message : {error}");
    }

    #[tokio::test]
    async fn a_target_without_an_agent_row_is_unreachable_rather_than_a_crash() {
        let (pool, _dir) = pool_with_agent(None).await;
        // Cible « agent » créée à la main et jamais réclamée par une machine.
        let error =
            AgentCollector::new(pool).probe(&Target { id: 9_999, ..target(30) }).await.unwrap_err();
        assert!(error.means_down());
    }
}
