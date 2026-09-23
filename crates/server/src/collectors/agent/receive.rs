//! Réception des lots poussés par les agents.
//!
//! Ce module porte toute la logique — authentification, enregistrement
//! automatique, mise en forme des échantillons — pour que le gestionnaire HTTP ne
//! soit qu'une façade et reste vérifiable d'un coup d'œil.

use dumbmonit_proto::{
    MAX_BATCH_SAMPLES, MetricKind, PUSH_PROTOCOL_VERSION, PushAck, PushBatch, Sample, Target,
};
use sqlx::SqlitePool;

use crate::crypto::Cipher;
use crate::db;
use crate::tsdb::SampleSink;

use super::store::RegisterError;
use super::{store, token};

/// Antériorité maximale d'un échantillon accepté.
///
/// Un agent rattrape au plus quelques heures de coupure. Au-delà, l'horloge de la
/// machine est fausse, et écrire ces points polluerait durablement les graphes
/// d'une période déjà passée.
const MAX_BACKFILL_MS: i64 = 30 * 24 * 3_600 * 1_000;

/// Avance maximale tolérée sur l'horloge du serveur.
const MAX_SKEW_AHEAD_MS: i64 = 3_600 * 1_000;

#[derive(Debug)]
pub enum IngestError {
    /// Jeton absent, inconnu ou révoqué.
    Unauthorized,
    /// Jeton valide, mais cette machine-là n'est pas celle qu'il prétend être —
    /// ou son jeton ne peut plus enrôler. Distinct de [`Self::Unauthorized`] :
    /// changer de jeton n'y changerait rien, il faut une décision humaine.
    Forbidden(String),
    /// Lot compréhensible mais invalide. Le réémettre à l'identique échouerait
    /// pareillement : l'agent doit l'abandonner.
    BadRequest(String),
    Internal(anyhow::Error),
}

/// Ce que le serveur répond à un agent lié dont il ne reconnaît pas le secret.
///
/// Écrit pour la personne qui lira les journaux de la machine, pas pour la
/// machine : il faut qu'elle sache quoi faire sans ouvrir la documentation.
pub const BINDING_MISMATCH: &str = "This machine is already enrolled and bound to another agent installation. \
     If you reinstalled it, open its device page in DumbMonit and click \
     'Allow re-enrolment', then restart the agent.";

impl From<RegisterError> for IngestError {
    fn from(error: RegisterError) -> Self {
        match error {
            RegisterError::BindingMismatch => Self::Forbidden(BINDING_MISMATCH.to_string()),
            RegisterError::EnrolmentDenied(denied) => Self::Forbidden(denied.message().to_string()),
            RegisterError::Internal(error) => Self::Internal(error),
        }
    }
}

impl<E: Into<anyhow::Error>> From<E> for IngestError {
    fn from(error: E) -> Self {
        Self::Internal(error.into())
    }
}

/// Accepte un lot : vérifie le jeton, rattache la machine, range les mesures.
pub async fn ingest(
    pool: &SqlitePool,
    cipher: &Cipher,
    sink: &SampleSink,
    bearer: Option<&str>,
    agent_secret: Option<&str>,
    batch: PushBatch,
) -> Result<PushAck, IngestError> {
    validate(&batch).map_err(IngestError::BadRequest)?;

    let presented = token::extract_bearer(bearer).ok_or(IngestError::Unauthorized)?;
    let token_id = store::find_active_token(pool, &token::fingerprint(presented))
        .await?
        .ok_or(IngestError::Unauthorized)?;

    let registration = store::register(
        pool,
        cipher,
        &batch.identity,
        token_id,
        token::extract_secret(agent_secret),
    )
    .await?;
    if registration.created {
        tracing::info!(
            cible = registration.target_id,
            hote = batch.identity.hostname,
            systeme = batch.identity.os,
            "nouvelle machine enregistrée par son agent"
        );
    }

    let target =
        db::targets::get(pool, cipher, registration.target_id).await?.ok_or_else(|| {
            IngestError::Internal(anyhow::anyhow!("target not found after registration"))
        })?;

    let received_at_ms = chrono::Utc::now().timestamp_millis();
    let (samples, accepted) = prepare(&target, batch.samples, received_at_ms);

    sink.send(samples).await;
    store::record_batch(pool, target.id, received_at_ms, accepted).await?;
    store::touch_token(pool, token_id).await?;
    // La même trace que pour une interrogation classique : l'interface affiche
    // « vu à » sans avoir à connaître la particularité des cibles en poussée.
    db::targets::record_probe(pool, target.id, None).await?;

    Ok(PushAck {
        target_id: target.id,
        accepted,
        interval_secs: target.interval.as_secs(),
        registered: registration.created,
        // Seule occasion de voir le secret : le serveur n'en garde que
        // l'empreinte, et ne saura jamais le redonner.
        agent_secret: registration.issued_secret,
        bound: registration.bound,
    })
}

/// Contrôles qui ne dépendent d'aucun accès à la base.
fn validate(batch: &PushBatch) -> Result<(), String> {
    if batch.protocol > PUSH_PROTOCOL_VERSION {
        return Err(format!(
            "protocol {} is not supported by this server (maximum version \
             {PUSH_PROTOCOL_VERSION}), update the server",
            batch.protocol
        ));
    }
    if batch.identity.hostname.trim().is_empty() {
        return Err("hostname is required".to_string());
    }
    if batch.samples.len() > MAX_BATCH_SAMPLES {
        return Err(format!(
            "batch too large: {} samples, maximum is {MAX_BATCH_SAMPLES}",
            batch.samples.len()
        ));
    }
    Ok(())
}

/// Prépare les échantillons reçus pour l'écriture, et renvoie combien de mesures
/// de l'agent ont été retenues.
///
/// Ce décompte exclut le témoin `up` ajouté ici : l'accusé de réception doit dire
/// à l'agent ce qu'il est advenu de *ses* mesures, pas y mêler celles du serveur.
///
/// Les étiquettes d'identité de la cible sont appliquées ici, et elles priment sur
/// celles de l'agent : c'est ce qui empêche une machine d'écrire dans les séries
/// d'une autre, même en forgeant ses étiquettes. Le pendant exact de ce que le
/// registre fait pour les collecteurs en interrogation.
fn prepare(target: &Target, samples: Vec<Sample>, received_at_ms: i64) -> (Vec<Sample>, usize) {
    let base_labels = target.base_labels();
    let oldest = received_at_ms - MAX_BACKFILL_MS;
    let newest = received_at_ms + MAX_SKEW_AHEAD_MS;

    let mut prepared: Vec<Sample> = samples
        .into_iter()
        .filter(|sample| {
            // Une valeur non finie ferait rejeter tout le lot par VictoriaMetrics,
            // et une horloge déréglée écrirait dans un passé ou un futur arbitraire.
            sample.value.is_finite() && sample.ts_ms >= oldest && sample.ts_ms <= newest
        })
        .map(|mut sample| {
            for (key, value) in &base_labels {
                sample.labels.insert(key.clone(), value.clone());
            }
            sample
        })
        .collect();
    let accepted = prepared.len();

    // Témoin de disponibilité posé à l'heure du serveur, et non à celle des
    // mesures : un lot de rattrapage prouve que la machine est joignable
    // maintenant, pas qu'elle l'était pendant la coupure.
    let mut up = Sample::new("up", 1.0, MetricKind::Gauge, received_at_ms);
    up.labels = base_labels;
    prepared.push(up);

    (prepared, accepted)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use dumbmonit_proto::{AgentIdentity, Credential};

    use super::*;

    fn target() -> Target {
        Target {
            id: 7,
            name: "nas".into(),
            address: "id-nas".into(),
            kind: "agent".into(),
            profile_id: None,
            parent_id: None,
            interval: std::time::Duration::from_secs(30),
            enabled: true,
            tags: BTreeMap::from([("salle".to_string(), "cave".to_string())]),
            credential: Credential::None,
        }
    }

    fn batch(samples: Vec<Sample>) -> PushBatch {
        PushBatch::new(
            AgentIdentity {
                hostname: "nas".into(),
                os: "linux".into(),
                os_version: None,
                kernel_version: None,
                arch: None,
                agent_version: "0.1.0".into(),
                commands_enabled: Some(true),
                relay: false,
                site: None,
                machine_id: None,
                tags: BTreeMap::new(),
                binding_supported: true,
            },
            samples,
            0,
        )
    }

    #[test]
    fn identity_labels_are_applied_and_cannot_be_overridden() {
        // L'agent tente de se faire passer pour une autre cible : ses étiquettes
        // doivent être écrasées, sans quoi l'authentification par jeton ne
        // protégerait plus rien.
        let samples = vec![
            Sample::new("cpu_usage_percent", 12.0, MetricKind::Gauge, 1_000)
                .with_label("target", "1")
                .with_label("host", "usurpateur"),
        ];
        let (prepared, accepted) = prepare(&target(), samples, 1_000);
        assert_eq!(accepted, 1, "le témoin « up » ne compte pas comme une mesure de l'agent");

        let cpu = prepared.iter().find(|s| s.metric == "cpu_usage_percent").expect("mesure");
        assert_eq!(cpu.labels.get("target").map(String::as_str), Some("7"));
        assert_eq!(cpu.labels.get("host").map(String::as_str), Some("nas"));
        assert_eq!(cpu.labels.get("tag_salle").map(String::as_str), Some("cave"));
    }

    #[test]
    fn a_batch_carries_its_own_proof_of_life() {
        let (prepared, accepted) = prepare(&target(), Vec::new(), 1_700_000_000_000);
        assert_eq!(accepted, 0);
        let up = prepared.iter().find(|s| s.metric == "up").expect("témoin");

        assert_eq!(up.value, 1.0);
        // Horodaté à la réception : un lot de rattrapage ne doit pas laisser croire
        // que la machine était joignable pendant la coupure.
        assert_eq!(up.ts_ms, 1_700_000_000_000);
        assert_eq!(up.labels.get("host").map(String::as_str), Some("nas"));
    }

    #[test]
    fn non_finite_values_are_dropped_rather_than_failing_the_batch() {
        let samples = vec![
            Sample::new("bon", 1.0, MetricKind::Gauge, 1_000),
            Sample::new("mauvais", f64::NAN, MetricKind::Gauge, 1_000),
        ];
        let (prepared, accepted) = prepare(&target(), samples, 1_000);
        assert_eq!(accepted, 1, "seule la mesure valide est retenue");
        assert!(prepared.iter().all(|s| s.value.is_finite()));
        assert!(prepared.iter().any(|s| s.metric == "bon"));
        assert!(!prepared.iter().any(|s| s.metric == "mauvais"));
    }

    #[test]
    fn samples_from_a_wildly_wrong_clock_are_discarded() {
        let now = 1_700_000_000_000;
        let samples = vec![
            Sample::new("passe_lointain", 1.0, MetricKind::Gauge, now - MAX_BACKFILL_MS - 1),
            Sample::new("futur", 1.0, MetricKind::Gauge, now + MAX_SKEW_AHEAD_MS + 1),
            Sample::new("rattrapage", 1.0, MetricKind::Gauge, now - 3_600_000),
        ];
        let (prepared, accepted) = prepare(&target(), samples, now);
        assert_eq!(accepted, 1, "seul le rattrapage légitime est retenu");

        // Le rattrapage légitime passe, l'aberration non.
        assert!(prepared.iter().any(|s| s.metric == "rattrapage"));
        assert!(!prepared.iter().any(|s| s.metric == "passe_lointain"));
        assert!(!prepared.iter().any(|s| s.metric == "futur"));
    }

    #[test]
    fn a_batch_from_a_newer_agent_is_refused_with_a_useful_message() {
        let mut too_new = batch(Vec::new());
        too_new.protocol = PUSH_PROTOCOL_VERSION + 1;
        let error = validate(&too_new).unwrap_err();
        assert!(error.contains("update the server"), "message inattendu : {error}");
    }

    #[test]
    fn a_nameless_machine_is_refused() {
        let mut anonymous = batch(Vec::new());
        anonymous.identity.hostname = "   ".into();
        assert!(validate(&anonymous).is_err());
    }

    #[test]
    fn an_oversized_batch_is_refused() {
        let sample = Sample::new("x", 1.0, MetricKind::Gauge, 0);
        let huge = batch(vec![sample; MAX_BATCH_SAMPLES + 1]);
        assert!(validate(&huge).is_err());
        // La borne elle-même reste acceptée.
        let sample = Sample::new("x", 1.0, MetricKind::Gauge, 0);
        assert!(validate(&batch(vec![sample; MAX_BATCH_SAMPLES])).is_ok());
    }

    #[test]
    fn an_empty_batch_is_valid() {
        // Un agent qui ne trouve rien à mesurer prouve tout de même qu'il est vivant.
        assert!(validate(&batch(Vec::new())).is_ok());
    }
}
