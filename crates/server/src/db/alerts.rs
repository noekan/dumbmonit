//! Accès aux règles, à l'état des alertes, aux silences, aux canaux et aux
//! baselines. Jalons 4 et 5.
//!
//! Comme le reste de la couche base, on utilise l'API d'exécution de sqlx et non les
//! macros `query!` : la version 0.9 refuse toute requête construite dynamiquement,
//! et surtout les macros exigeraient une base de référence à la compilation, ce qui
//! compliquerait la construction du conteneur pour zéro bénéfice ici.

use std::collections::HashMap;

use anyhow::{Context, Result};
use chrono::{DateTime, SecondsFormat, Utc};
use serde_json::Value;
use sqlx::{Row, SqlitePool};

use crate::alerting::baseline::{Bucket, SeriesBaseline};
use crate::alerting::cycle::{BaselineStore, FORGOTTEN_TARGET_REASON, HistoryEntry, StoredAlert};
use crate::alerting::group::AlertOutcome;
use crate::alerting::machine::{AlertState, Phase};
use crate::alerting::model::{
    Operator, Rule, RuleKind, Severity, TargetId, TargetNode, TargetSelector,
};
use crate::alerting::rules;
use crate::alerting::silence::Silence;
use crate::crypto::Cipher;
use crate::notify::{ChannelConfig, DeliveryReport};

/// Horodatage textuel, à la milliseconde.
///
/// La précision compte : la purge des empreintes obsolètes compare `last_eval_at` à
/// l'instant du cycle, et deux cycles peuvent se suivre dans la même seconde.
fn to_sql(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn from_sql(raw: Option<String>) -> Option<DateTime<Utc>> {
    raw.and_then(|value| DateTime::parse_from_rfc3339(&value).ok())
        .map(|value| value.with_timezone(&Utc))
}

fn json_or_default<T: serde::de::DeserializeOwned + Default>(raw: &str) -> T {
    serde_json::from_str(raw).unwrap_or_default()
}

// --------------------------------------------------------------------------
// Règles
// --------------------------------------------------------------------------

/// Insère les règles livrées absentes de la base.
///
/// `INSERT OR IGNORE` sur `uid` : une règle que l'utilisateur a modifiée, désactivée
/// ou réglée à un autre seuil n'est jamais réécrite. Seule une règle qu'il a
/// supprimée réapparaît — compromis assumé, il peut la désactiver pour de bon.
pub async fn seed_builtin_rules(pool: &SqlitePool) -> Result<usize> {
    let mut inserted = 0;
    for rule in rules::builtin_rules() {
        let result = sqlx::query(
            "INSERT OR IGNORE INTO alert_rules
                 (uid, name, description, kind, query, operator, threshold, clear_threshold,
                  for_secs, severity, selector, channels, params, unit, repeat_secs,
                  escalate_after_secs, enabled, builtin)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 1, 1)",
        )
        .bind(&rule.uid)
        .bind(&rule.name)
        .bind(&rule.description)
        .bind(rule.kind.as_str())
        .bind(&rule.query)
        .bind(rule.operator.as_str())
        .bind(rule.threshold)
        .bind(rule.clear_threshold)
        .bind(rule.for_duration.as_secs() as i64)
        .bind(rule.severity.as_str())
        .bind(serde_json::to_string(&rule.selector)?)
        .bind(serde_json::to_string(&rule.channels)?)
        .bind(serde_json::to_string(&rule.params)?)
        .bind(&rule.unit)
        .bind(rule.repeat_interval.map(|d| d.as_secs() as i64))
        .bind(rule.escalate_after.map(|d| d.as_secs() as i64))
        .execute(pool)
        .await
        .with_context(|| format!("insertion de la règle livrée « {} »", rule.uid))?;
        inserted += result.rows_affected() as usize;

        // Le libellé d'une règle livrée est du texte produit, pas un réglage : il suit
        // les versions (traduction, reformulation) sans toucher aux seuils, canaux ou
        // à l'activation, qui restent la propriété de l'utilisateur.
        // La requête d'une règle livrée est aussi du code produit : une correction
        // (par exemple `== bool 0`) doit atteindre les bases existantes.
        sqlx::query(
            "UPDATE alert_rules SET name = ?, description = ?, unit = ?, query = ?
             WHERE uid = ? AND builtin = 1",
        )
        .bind(&rule.name)
        .bind(&rule.description)
        .bind(&rule.unit)
        .bind(&rule.query)
        .bind(&rule.uid)
        .execute(pool)
        .await
        .with_context(|| format!("mise à jour du libellé de la règle livrée « {} »", rule.uid))?;
    }
    Ok(inserted)
}

fn rule_from_row(row: &sqlx::sqlite::SqliteRow) -> Result<Rule> {
    let for_secs: i64 = row.try_get("for_secs")?;
    let repeat: Option<i64> = row.try_get("repeat_secs")?;
    let escalate: Option<i64> = row.try_get("escalate_after_secs")?;
    let selector: String = row.try_get("selector")?;
    let channels: String = row.try_get("channels")?;
    let params: String = row.try_get("params")?;
    let kind: String = row.try_get("kind")?;
    let operator: String = row.try_get("operator")?;
    let severity: String = row.try_get("severity")?;

    Ok(Rule {
        id: row.try_get("id")?,
        uid: row.try_get("uid")?,
        name: row.try_get("name")?,
        description: row.try_get("description")?,
        kind: RuleKind::parse(&kind),
        query: row.try_get("query")?,
        operator: Operator::parse(&operator),
        threshold: row.try_get("threshold")?,
        clear_threshold: row.try_get("clear_threshold")?,
        for_duration: std::time::Duration::from_secs(for_secs.max(0) as u64),
        severity: Severity::parse(&severity),
        // Une règle dont le JSON serait corrompu retombe sur le sélecteur le plus
        // large plutôt que de faire échouer tout le chargement : mieux vaut une règle
        // trop bavarde qu'un moteur d'alerting qui refuse de démarrer.
        selector: json_or_default::<TargetSelector>(&selector),
        channels: json_or_default::<Vec<i64>>(&channels),
        params: serde_json::from_str(&params).unwrap_or_default(),
        unit: row.try_get("unit")?,
        repeat_interval: repeat
            .filter(|secs| *secs > 0)
            .map(|secs| std::time::Duration::from_secs(secs as u64)),
        escalate_after: escalate
            .filter(|secs| *secs > 0)
            .map(|secs| std::time::Duration::from_secs(secs as u64)),
        enabled: row.try_get::<i64, _>("enabled")? != 0,
        builtin: row.try_get::<i64, _>("builtin")? != 0,
    })
}

pub async fn list_rules(pool: &SqlitePool) -> Result<Vec<Rule>> {
    let rows = sqlx::query(
        "SELECT id, uid, name, description, kind, query, operator, threshold, clear_threshold,
                for_secs, severity, selector, channels, params, unit, repeat_secs,
                escalate_after_secs, enabled, builtin
         FROM alert_rules ORDER BY builtin DESC, name",
    )
    .fetch_all(pool)
    .await
    .context("lecture des règles d'alerte")?;
    rows.iter().map(rule_from_row).collect()
}

pub async fn list_enabled_rules(pool: &SqlitePool) -> Result<Vec<Rule>> {
    let rows = sqlx::query(
        "SELECT id, uid, name, description, kind, query, operator, threshold, clear_threshold,
                for_secs, severity, selector, channels, params, unit, repeat_secs,
                escalate_after_secs, enabled, builtin
         FROM alert_rules WHERE enabled = 1 ORDER BY id",
    )
    .fetch_all(pool)
    .await
    .context("lecture des règles actives")?;
    rows.iter().map(rule_from_row).collect()
}

/// Crée ou remplace une règle, désignée par son `uid`.
pub async fn upsert_rule(pool: &SqlitePool, rule: &Rule) -> Result<i64> {
    let row = sqlx::query(
        "INSERT INTO alert_rules
             (uid, name, description, kind, query, operator, threshold, clear_threshold,
              for_secs, severity, selector, channels, params, unit, repeat_secs,
              escalate_after_secs, enabled, builtin, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, datetime('now'))
         ON CONFLICT(uid) DO UPDATE SET
             name = excluded.name, description = excluded.description, kind = excluded.kind,
             query = excluded.query, operator = excluded.operator,
             threshold = excluded.threshold, clear_threshold = excluded.clear_threshold,
             for_secs = excluded.for_secs,
             severity = excluded.severity, selector = excluded.selector,
             channels = excluded.channels, params = excluded.params, unit = excluded.unit,
             repeat_secs = excluded.repeat_secs,
             escalate_after_secs = excluded.escalate_after_secs,
             enabled = excluded.enabled, updated_at = datetime('now')
         RETURNING id",
    )
    .bind(&rule.uid)
    .bind(&rule.name)
    .bind(&rule.description)
    .bind(rule.kind.as_str())
    .bind(&rule.query)
    .bind(rule.operator.as_str())
    .bind(rule.threshold)
    .bind(rule.clear_threshold)
    .bind(rule.for_duration.as_secs() as i64)
    .bind(rule.severity.as_str())
    .bind(serde_json::to_string(&rule.selector)?)
    .bind(serde_json::to_string(&rule.channels)?)
    .bind(serde_json::to_string(&rule.params)?)
    .bind(&rule.unit)
    .bind(rule.repeat_interval.map(|d| d.as_secs() as i64))
    .bind(rule.escalate_after.map(|d| d.as_secs() as i64))
    .bind(i64::from(rule.enabled))
    .bind(i64::from(rule.builtin))
    .fetch_one(pool)
    .await
    .context("enregistrement de la règle")?;
    Ok(row.try_get("id")?)
}

/// Supprime une règle et tout ce qui s'y rattache.
pub async fn delete_rule(pool: &SqlitePool, id: i64) -> Result<bool> {
    let uid: Option<String> = sqlx::query("SELECT uid FROM alert_rules WHERE id = ?")
        .bind(id)
        .fetch_optional(pool)
        .await
        .context("lecture de la règle à supprimer")?
        .map(|row| row.try_get("uid"))
        .transpose()?;

    let Some(uid) = uid else { return Ok(false) };

    sqlx::query("DELETE FROM alert_rules WHERE id = ?").bind(id).execute(pool).await?;
    // Les états orphelins seraient purgés au cycle suivant, mais les laisser
    // traîner ferait apparaître des alertes fantômes dans l'interface entre-temps.
    sqlx::query("DELETE FROM alert_state WHERE rule_uid = ?").bind(&uid).execute(pool).await?;
    Ok(true)
}

pub async fn set_rule_enabled(pool: &SqlitePool, id: i64, enabled: bool) -> Result<bool> {
    let result = sqlx::query(
        "UPDATE alert_rules SET enabled = ?, updated_at = datetime('now') WHERE id = ?",
    )
    .bind(i64::from(enabled))
    .bind(id)
    .execute(pool)
    .await
    .context("activation de la règle")?;
    Ok(result.rows_affected() > 0)
}

// --------------------------------------------------------------------------
// Topologie des cibles
// --------------------------------------------------------------------------

/// Charge la vue des cibles utile à l'alerting.
///
/// Les cibles désactivées sont incluses : elles peuvent être le parent d'une cible
/// active, et les retirer casserait la chaîne de suppression.
pub async fn list_target_nodes(pool: &SqlitePool) -> Result<Vec<TargetNode>> {
    let rows = sqlx::query(
        "SELECT id, name, address, parent_id, via_agent, tags, enabled, last_error FROM targets",
    )
    .fetch_all(pool)
    .await
    .context("lecture de la topologie des cibles")?;

    rows.iter()
        .map(|row| {
            let tags: String = row.try_get("tags")?;
            let enabled = row.try_get::<i64, _>("enabled")? != 0;
            let last_error: Option<String> = row.try_get("last_error")?;
            Ok(TargetNode {
                id: row.try_get("id")?,
                name: row.try_get("name")?,
                address: row.try_get("address")?,
                parent_id: row.try_get("parent_id")?,
                via_agent: row.try_get("via_agent")?,
                tags: json_or_default(&tags),
                enabled,
                // Une cible en pause garde son dernier verdict, qui ne dit plus
                // rien du présent : elle ne peut pas étouffer ses descendants.
                unreachable: enabled
                    && last_error.as_deref().is_some_and(crate::db::targets::error_means_down),
            })
        })
        .collect()
}

/// Oublie sur-le-champ les alertes d'une cible supprimée ou désactivée.
///
/// Sans notification : l'utilisateur vient de dire que cet équipement ne compte
/// plus, un « résolu » ou un « en panne » à son sujet serait du bruit. Les
/// alertes actives laissent tout de même une transition dans l'historique, pour
/// que la chronologie de l'équipement reste lisible, et les lignes retenues dans
/// la file de notification sont retirées avant de partir. Renvoie le nombre
/// d'alertes effacées.
pub async fn forget_target(
    pool: &SqlitePool,
    target_id: TargetId,
    now: DateTime<Utc>,
) -> Result<u64> {
    let mut tx = pool.begin().await.context("ouverture de la transaction d'oubli")?;
    sqlx::query(
        "INSERT INTO alert_history
             (fingerprint, rule_uid, target_id, from_phase, to_phase, severity, value,
              notified, reason, at)
         SELECT fingerprint, rule_uid, target_id, phase, 'resolved', severity, value,
                0, ?, ?
         FROM alert_state
         WHERE target_id = ? AND phase IN ('pending', 'firing')",
    )
    .bind(FORGOTTEN_TARGET_REASON)
    .bind(to_sql(now))
    .bind(target_id)
    .execute(&mut *tx)
    .await
    .context("journalisation des alertes oubliées")?;
    let removed = sqlx::query("DELETE FROM alert_state WHERE target_id = ?")
        .bind(target_id)
        .execute(&mut *tx)
        .await
        .context("effacement des alertes de la cible")?
        .rows_affected();
    sqlx::query("DELETE FROM notify_queue WHERE target_id = ?")
        .bind(target_id)
        .execute(&mut *tx)
        .await
        .context("retrait des notifications en attente de la cible")?;
    tx.commit().await.context("validation de l'oubli")?;
    Ok(removed)
}

// --------------------------------------------------------------------------
// État des alertes
// --------------------------------------------------------------------------

pub async fn load_states(pool: &SqlitePool) -> Result<Vec<StoredAlert>> {
    let rows = sqlx::query(
        "SELECT fingerprint, rule_uid, target_id, series_key, labels, phase, suppressed,
                suppressed_by, silenced, learning, value, score, condition_since,
                firing_since, last_eval_at, last_notified_at, notify_count, resolved_at,
                acked_until, acked_by, ack_note
         FROM alert_state",
    )
    .fetch_all(pool)
    .await
    .context("lecture de l'état des alertes")?;

    rows.iter()
        .map(|row| {
            let labels: String = row.try_get("labels")?;
            let phase: String = row.try_get("phase")?;
            Ok(StoredAlert {
                fingerprint: row.try_get("fingerprint")?,
                rule_uid: row.try_get("rule_uid")?,
                target_id: row.try_get("target_id")?,
                series_key: row.try_get("series_key")?,
                labels: json_or_default(&labels),
                state: AlertState {
                    phase: Phase::parse(&phase),
                    condition_since: from_sql(row.try_get("condition_since")?),
                    firing_since: from_sql(row.try_get("firing_since")?),
                    resolved_at: from_sql(row.try_get("resolved_at")?),
                    last_eval_at: from_sql(row.try_get("last_eval_at")?),
                    last_notified_at: from_sql(row.try_get("last_notified_at")?),
                    notify_count: row.try_get::<i64, _>("notify_count")?.max(0) as u32,
                    suppressed: row.try_get::<i64, _>("suppressed")? != 0,
                    suppressed_by: row.try_get("suppressed_by")?,
                    silenced: row.try_get::<i64, _>("silenced")? != 0,
                    learning: row.try_get::<i64, _>("learning")? != 0,
                    value: row.try_get("value")?,
                    score: row.try_get("score")?,
                    acked_until: from_sql(row.try_get("acked_until")?),
                    acked_by: row.try_get("acked_by")?,
                    ack_note: row.try_get("ack_note")?,
                },
            })
        })
        .collect()
}

/// Écrit l'état issu d'un cycle. Une seule transaction : un cycle interrompu ne
/// laisse jamais la moitié des alertes dans l'état d'avant et l'autre dans celui
/// d'après.
///
/// L'acquittement est la seule donnée de la ligne que le cycle ne réécrit pas :
/// il est posé par l'API, entre deux cycles, et un cycle parti avant le clic
/// l'écraserait avec l'état d'avant. Sur une ligne existante, le cycle ne fait
/// que l'effacer — quand l'alerte n'est plus active, ou quand l'échéance est
/// passée — ; une ligne neuve reprend ce qu'il porte en mémoire (le cas d'une
/// alerte reprise sous une nouvelle empreinte).
pub async fn save_states(pool: &SqlitePool, alerts: &[AlertOutcome]) -> Result<()> {
    if alerts.is_empty() {
        return Ok(());
    }
    let mut tx = pool.begin().await.context("ouverture de la transaction d'état")?;

    for alert in alerts {
        sqlx::query(
            "INSERT INTO alert_state
                 (fingerprint, rule_uid, target_id, series_key, labels, phase, suppressed,
                  suppressed_by, silenced, learning, severity, value, score, condition_since,
                  firing_since, last_eval_at, last_notified_at, notify_count, resolved_at,
                  acked_until, acked_by, ack_note)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(fingerprint) DO UPDATE SET
                 rule_uid = excluded.rule_uid, target_id = excluded.target_id,
                 series_key = excluded.series_key, labels = excluded.labels,
                 phase = excluded.phase, suppressed = excluded.suppressed,
                 suppressed_by = excluded.suppressed_by, silenced = excluded.silenced,
                 learning = excluded.learning, severity = excluded.severity,
                 value = excluded.value, score = excluded.score,
                 condition_since = excluded.condition_since,
                 firing_since = excluded.firing_since, last_eval_at = excluded.last_eval_at,
                 last_notified_at = excluded.last_notified_at,
                 notify_count = excluded.notify_count, resolved_at = excluded.resolved_at,
                 acked_until = CASE WHEN excluded.phase IN ('pending', 'firing')
                                     AND alert_state.acked_until > excluded.last_eval_at
                                    THEN alert_state.acked_until END,
                 acked_by = CASE WHEN excluded.phase IN ('pending', 'firing')
                                  AND alert_state.acked_until > excluded.last_eval_at
                                 THEN alert_state.acked_by END,
                 ack_note = CASE WHEN excluded.phase IN ('pending', 'firing')
                                  AND alert_state.acked_until > excluded.last_eval_at
                                 THEN alert_state.ack_note END",
        )
        .bind(&alert.fingerprint)
        .bind(&alert.rule_uid)
        .bind(alert.target_id)
        .bind(&alert.series_key)
        .bind(serde_json::to_string(&alert.labels)?)
        .bind(alert.state.phase.as_str())
        .bind(i64::from(alert.state.suppressed))
        .bind(alert.state.suppressed_by)
        .bind(i64::from(alert.state.silenced))
        .bind(i64::from(alert.state.learning))
        .bind(alert.severity.as_str())
        .bind(alert.value)
        .bind(alert.score)
        .bind(alert.state.condition_since.map(to_sql))
        .bind(alert.state.firing_since.map(to_sql))
        .bind(alert.state.last_eval_at.map(to_sql))
        .bind(alert.state.last_notified_at.map(to_sql))
        .bind(i64::from(alert.state.notify_count))
        .bind(alert.state.resolved_at.map(to_sql))
        .bind(alert.state.acked_until.map(to_sql))
        .bind(&alert.state.acked_by)
        .bind(&alert.state.ack_note)
        .execute(&mut *tx)
        .await
        .context("écriture de l'état d'une alerte")?;
    }

    tx.commit().await.context("validation de la transaction d'état")
}

/// Rafraîchit l'horodatage d'évaluation des empreintes gelées.
///
/// Sans cela, une panne prolongée de VictoriaMetrics ferait passer ces empreintes
/// pour obsolètes et la purge les effacerait — perdant au passage l'ancienneté des
/// alertes en cours.
pub async fn touch_states(
    pool: &SqlitePool,
    fingerprints: &[String],
    now: DateTime<Utc>,
) -> Result<()> {
    if fingerprints.is_empty() {
        return Ok(());
    }
    let stamp = to_sql(now);
    let mut tx = pool.begin().await?;
    for fingerprint in fingerprints {
        sqlx::query("UPDATE alert_state SET last_eval_at = ? WHERE fingerprint = ?")
            .bind(&stamp)
            .bind(fingerprint)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await.context("rafraîchissement des empreintes gelées")
}

/// Consigne l'envoi d'une notification pour une empreinte.
pub async fn mark_notified(
    pool: &SqlitePool,
    fingerprints: &[String],
    now: DateTime<Utc>,
) -> Result<()> {
    if fingerprints.is_empty() {
        return Ok(());
    }
    let stamp = to_sql(now);
    let mut tx = pool.begin().await?;
    for fingerprint in fingerprints {
        sqlx::query(
            "UPDATE alert_state
             SET last_notified_at = ?, notify_count = notify_count + 1
             WHERE fingerprint = ?",
        )
        .bind(&stamp)
        .bind(fingerprint)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await.context("enregistrement des notifications envoyées")
}

/// Supprime les empreintes que le cycle n'a ni évaluées ni gelées.
pub async fn purge_states(pool: &SqlitePool, cycle_start: DateTime<Utc>) -> Result<u64> {
    let result = sqlx::query("DELETE FROM alert_state WHERE last_eval_at < ?")
        .bind(to_sql(cycle_start))
        .execute(pool)
        .await
        .context("purge des empreintes obsolètes")?;
    Ok(result.rows_affected())
}

/// Alertes actives, pour l'API et l'interface.
pub async fn list_active(pool: &SqlitePool) -> Result<Vec<StoredAlert>> {
    let rows = sqlx::query(
        "SELECT fingerprint, rule_uid, target_id, series_key, labels, phase, suppressed,
                suppressed_by, silenced, learning, value, score, condition_since,
                firing_since, last_eval_at, last_notified_at, notify_count, resolved_at,
                acked_until, acked_by, ack_note
         FROM alert_state
         WHERE phase IN ('pending', 'firing')
         ORDER BY firing_since DESC",
    )
    .fetch_all(pool)
    .await
    .context("lecture des alertes actives")?;

    rows.iter()
        .map(|row| {
            let labels: String = row.try_get("labels")?;
            let phase: String = row.try_get("phase")?;
            Ok(StoredAlert {
                fingerprint: row.try_get("fingerprint")?,
                rule_uid: row.try_get("rule_uid")?,
                target_id: row.try_get("target_id")?,
                series_key: row.try_get("series_key")?,
                labels: json_or_default(&labels),
                state: AlertState {
                    phase: Phase::parse(&phase),
                    condition_since: from_sql(row.try_get("condition_since")?),
                    firing_since: from_sql(row.try_get("firing_since")?),
                    resolved_at: from_sql(row.try_get("resolved_at")?),
                    last_eval_at: from_sql(row.try_get("last_eval_at")?),
                    last_notified_at: from_sql(row.try_get("last_notified_at")?),
                    notify_count: row.try_get::<i64, _>("notify_count")?.max(0) as u32,
                    suppressed: row.try_get::<i64, _>("suppressed")? != 0,
                    suppressed_by: row.try_get("suppressed_by")?,
                    silenced: row.try_get::<i64, _>("silenced")? != 0,
                    learning: row.try_get::<i64, _>("learning")? != 0,
                    value: row.try_get("value")?,
                    score: row.try_get("score")?,
                    acked_until: from_sql(row.try_get("acked_until")?),
                    acked_by: row.try_get("acked_by")?,
                    ack_note: row.try_get("ack_note")?,
                },
            })
        })
        .collect()
}

/// Acquittement à poser sur une alerte.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ack {
    pub until: DateTime<Utc>,
    pub by: String,
    pub note: Option<String>,
}

/// Pose ou lève l'acquittement d'une alerte active.
///
/// `None` lève l'acquittement. Renvoie `false` si l'empreinte est inconnue ou
/// n'est plus active : acquitter une alerte résolue n'aurait pas de sens, et
/// l'interface ne la propose plus. En acquittant, les lignes de cette alerte
/// encore retenues dans la file de notification (fenêtre de regroupement,
/// heures calmes) sont retirées : l'utilisateur vient de dire qu'il sait, le
/// résumé n'a pas à le lui répéter. La mention d'une résolution survenue
/// pendant l'attente, elle, reste.
pub async fn set_ack(pool: &SqlitePool, fingerprint: &str, ack: Option<&Ack>) -> Result<bool> {
    let mut tx = pool.begin().await.context("ouverture de la transaction d'acquittement")?;
    let updated = sqlx::query(
        "UPDATE alert_state SET acked_until = ?, acked_by = ?, ack_note = ?
         WHERE fingerprint = ? AND phase IN ('pending', 'firing')",
    )
    .bind(ack.map(|ack| to_sql(ack.until)))
    .bind(ack.map(|ack| ack.by.as_str()))
    .bind(ack.and_then(|ack| ack.note.as_deref()))
    .bind(fingerprint)
    .execute(&mut *tx)
    .await
    .context("acquittement de l'alerte")?
    .rows_affected();
    if updated == 0 {
        return Ok(false);
    }
    if ack.is_some() {
        sqlx::query("DELETE FROM notify_queue WHERE fingerprint = ? AND resolved_meanwhile = 0")
            .bind(fingerprint)
            .execute(&mut *tx)
            .await
            .context("retrait des notifications en attente de l'alerte acquittée")?;
    }
    tx.commit().await.context("validation de l'acquittement")?;
    Ok(true)
}

/// Une alerte active, par son empreinte, pour l'API.
pub async fn get_active(pool: &SqlitePool, fingerprint: &str) -> Result<Option<StoredAlert>> {
    let alerts = list_active(pool).await?;
    Ok(alerts.into_iter().find(|alert| alert.fingerprint == fingerprint))
}

// --------------------------------------------------------------------------
// Historique
// --------------------------------------------------------------------------

pub async fn record_history(
    pool: &SqlitePool,
    entries: &[HistoryEntry],
    notified: &std::collections::HashSet<String>,
) -> Result<()> {
    if entries.is_empty() {
        return Ok(());
    }
    let mut tx = pool.begin().await?;
    for entry in entries {
        sqlx::query(
            "INSERT INTO alert_history
                 (fingerprint, rule_uid, target_id, from_phase, to_phase, severity, value,
                  notified, reason, at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&entry.fingerprint)
        .bind(&entry.rule_uid)
        .bind(entry.target_id)
        .bind(entry.transition.from.as_str())
        .bind(entry.transition.to.as_str())
        .bind(entry.severity.as_str())
        .bind(entry.value)
        .bind(i64::from(notified.contains(&entry.fingerprint)))
        .bind(&entry.reason)
        .bind(to_sql(entry.at))
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await.context("écriture de l'historique des alertes")
}

/// Efface l'historique antérieur à la date donnée.
pub async fn purge_history(pool: &SqlitePool, before: DateTime<Utc>) -> Result<u64> {
    let result = sqlx::query("DELETE FROM alert_history WHERE at < ?")
        .bind(to_sql(before))
        .execute(pool)
        .await
        .context("purge de l'historique")?;
    Ok(result.rows_affected())
}

// --------------------------------------------------------------------------
// Silences
// --------------------------------------------------------------------------

pub async fn list_silences(pool: &SqlitePool) -> Result<Vec<Silence>> {
    let rows = sqlx::query(
        "SELECT id, name, comment, target_id, matchers, schedule, enabled
         FROM silences ORDER BY id",
    )
    .fetch_all(pool)
    .await
    .context("lecture des silences")?;

    Ok(rows
        .iter()
        .filter_map(|row| {
            let matchers: String = row.try_get("matchers").ok()?;
            let schedule: String = row.try_get("schedule").ok()?;
            Some(Silence {
                id: row.try_get("id").ok()?,
                name: row.try_get("name").ok()?,
                comment: row.try_get("comment").ok()?,
                target_id: row.try_get("target_id").ok()?,
                matchers: json_or_default(&matchers),
                // Un silence dont la planification est illisible est ignoré plutôt
                // que traité comme permanent : se taire par accident est le pire
                // défaut possible pour un outil de supervision.
                schedule: serde_json::from_str(&schedule).ok()?,
                enabled: row.try_get::<i64, _>("enabled").ok()? != 0,
            })
        })
        .collect())
}

pub async fn create_silence(pool: &SqlitePool, silence: &Silence) -> Result<i64> {
    let row = sqlx::query(
        "INSERT INTO silences (name, comment, target_id, matchers, schedule, enabled)
         VALUES (?, ?, ?, ?, ?, ?) RETURNING id",
    )
    .bind(&silence.name)
    .bind(&silence.comment)
    .bind(silence.target_id)
    .bind(serde_json::to_string(&silence.matchers)?)
    .bind(serde_json::to_string(&silence.schedule)?)
    .bind(i64::from(silence.enabled))
    .fetch_one(pool)
    .await
    .context("création du silence")?;
    Ok(row.try_get("id")?)
}

pub async fn delete_silence(pool: &SqlitePool, id: i64) -> Result<bool> {
    let result = sqlx::query("DELETE FROM silences WHERE id = ?").bind(id).execute(pool).await?;
    Ok(result.rows_affected() > 0)
}

/// Retire les fenêtres ponctuelles entièrement passées.
pub async fn purge_expired_silences(pool: &SqlitePool, now: DateTime<Utc>) -> Result<u64> {
    let silences = list_silences(pool).await?;
    let mut removed = 0;
    for silence in silences.iter().filter(|s| s.schedule.is_expired(now)) {
        if delete_silence(pool, silence.id).await? {
            removed += 1;
        }
    }
    Ok(removed)
}

// --------------------------------------------------------------------------
// Canaux de notification
// --------------------------------------------------------------------------

/// Charge les canaux avec leurs secrets déchiffrés.
///
/// Un canal dont les secrets sont indéchiffrables — secret d'instance changé — est
/// chargé sans eux : le canal apparaîtra en erreur de configuration, ce qui est plus
/// exploitable qu'une disparition silencieuse.
pub async fn list_channels(pool: &SqlitePool, cipher: &Cipher) -> Result<Vec<ChannelConfig>> {
    let rows = sqlx::query(
        "SELECT id, name, kind, enabled, settings, secret_enc FROM notification_channels
         ORDER BY name",
    )
    .fetch_all(pool)
    .await
    .context("lecture des canaux de notification")?;

    let mut channels = Vec::with_capacity(rows.len());
    for row in &rows {
        let settings: String = row.try_get("settings")?;
        let encrypted: Option<Vec<u8>> = row.try_get("secret_enc")?;
        let name: String = row.try_get("name")?;

        let secrets = match encrypted {
            Some(bytes) => match cipher.decrypt(&bytes) {
                Ok(plain) => serde_json::from_slice(&plain).unwrap_or(Value::Null),
                Err(error) => {
                    // `error` ne contient jamais le clair : le déchiffrement échoue
                    // avant toute exploitation du contenu.
                    tracing::warn!(channel = %name, %error, "secrets de canal illisibles");
                    Value::Null
                }
            },
            None => Value::Null,
        };

        channels.push(ChannelConfig {
            id: row.try_get("id")?,
            name,
            kind: row.try_get("kind")?,
            enabled: row.try_get::<i64, _>("enabled")? != 0,
            settings: serde_json::from_str(&settings).unwrap_or(Value::Null),
            secrets,
        });
    }
    Ok(channels)
}

pub async fn get_channel(
    pool: &SqlitePool,
    cipher: &Cipher,
    id: i64,
) -> Result<Option<ChannelConfig>> {
    Ok(list_channels(pool, cipher).await?.into_iter().find(|channel| channel.id == id))
}

/// Canal tel que l'API le soumet, avant chiffrement.
#[derive(Debug, Clone)]
pub struct ChannelDraft {
    /// `None` pour une création.
    pub id: Option<i64>,
    pub name: String,
    pub kind: String,
    pub enabled: bool,
    pub settings: Value,
    /// `None` conserve les secrets déjà enregistrés : l'interface peut ainsi
    /// renommer un canal sans redemander le jeton à l'utilisateur.
    pub secrets: Option<Value>,
}

/// Crée ou met à jour un canal. Les secrets sont chiffrés avant d'atteindre le disque.
pub async fn upsert_channel(
    pool: &SqlitePool,
    cipher: &Cipher,
    draft: &ChannelDraft,
) -> Result<i64> {
    let ChannelDraft { id, name, kind, enabled, settings, secrets } = draft;
    let encrypted = match secrets {
        Some(value) => Some(cipher.encrypt(serde_json::to_string(value)?.as_bytes())?),
        None => None,
    };

    if let Some(id) = *id {
        sqlx::query(
            "UPDATE notification_channels
             SET name = ?, kind = ?, enabled = ?, settings = ?,
                 secret_enc = COALESCE(?, secret_enc), updated_at = datetime('now')
             WHERE id = ?",
        )
        .bind(name)
        .bind(kind)
        .bind(i64::from(*enabled))
        .bind(serde_json::to_string(settings)?)
        .bind(encrypted)
        .bind(id)
        .execute(pool)
        .await
        .context("mise à jour du canal")?;
        return Ok(id);
    }

    let row = sqlx::query(
        "INSERT INTO notification_channels (name, kind, enabled, settings, secret_enc)
         VALUES (?, ?, ?, ?, ?) RETURNING id",
    )
    .bind(name)
    .bind(kind)
    .bind(i64::from(*enabled))
    .bind(serde_json::to_string(settings)?)
    .bind(encrypted)
    .fetch_one(pool)
    .await
    .context("création du canal")?;
    Ok(row.try_get("id")?)
}

pub async fn delete_channel(pool: &SqlitePool, id: i64) -> Result<bool> {
    let result = sqlx::query("DELETE FROM notification_channels WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}

/// Consigne le résultat d'un envoi, pour l'afficher à côté du canal.
pub async fn record_delivery(
    pool: &SqlitePool,
    report: &DeliveryReport,
    now: DateTime<Utc>,
) -> Result<()> {
    if report.is_success() {
        sqlx::query(
            "UPDATE notification_channels SET last_sent_at = ?, last_error = NULL WHERE id = ?",
        )
        .bind(to_sql(now))
        .bind(report.channel_id)
        .execute(pool)
        .await?;
    } else {
        sqlx::query("UPDATE notification_channels SET last_error = ? WHERE id = ?")
            .bind(report.error.as_deref())
            .bind(report.channel_id)
            .execute(pool)
            .await?;
    }
    Ok(())
}

// --------------------------------------------------------------------------
// Baselines d'anomalie
// --------------------------------------------------------------------------

/// Charge les baselines nécessaires au seau saisonnier courant.
///
/// Seul un cent-soixante-huitième des seaux est utile à un cycle donné : les charger
/// tous coûterait cent soixante-huit fois plus de lecture pour rien.
pub async fn load_baselines(pool: &SqlitePool, bucket: usize) -> Result<BaselineStore> {
    let series_rows = sqlx::query("SELECT series_key, first_seen, updates FROM anomaly_series")
        .fetch_all(pool)
        .await
        .context("lecture des séries suivies")?;

    let mut series = HashMap::new();
    for row in &series_rows {
        let key: String = row.try_get("series_key")?;
        let first_seen: String = row.try_get("first_seen")?;
        let Some(first_seen) = from_sql(Some(first_seen)) else { continue };
        series.insert(
            key,
            SeriesBaseline { first_seen, updates: row.try_get::<i64, _>("updates")?.max(0) as u64 },
        );
    }

    let bucket_rows = sqlx::query(
        "SELECT series_key, ewma, mad, samples FROM anomaly_baselines WHERE bucket = ?",
    )
    .bind(bucket as i64)
    .fetch_all(pool)
    .await
    .context("lecture des baselines")?;

    let mut buckets = HashMap::new();
    for row in &bucket_rows {
        let key: String = row.try_get("series_key")?;
        buckets.insert(
            (key, bucket),
            Bucket {
                ewma: row.try_get("ewma")?,
                mad: row.try_get("mad")?,
                samples: row.try_get::<i64, _>("samples")?.max(0) as u32,
            },
        );
    }

    Ok(BaselineStore::new(series, buckets))
}

/// Persiste les seuls seaux modifiés par le cycle.
pub async fn save_baselines(
    pool: &SqlitePool,
    store: &BaselineStore,
    now: DateTime<Utc>,
) -> Result<()> {
    let stamp = to_sql(now);
    let mut tx = pool.begin().await?;

    for (key, series) in store.dirty_series() {
        sqlx::query(
            "INSERT INTO anomaly_series (series_key, first_seen, last_update, updates)
             VALUES (?, ?, ?, ?)
             ON CONFLICT(series_key) DO UPDATE SET
                 last_update = excluded.last_update, updates = excluded.updates",
        )
        .bind(key)
        .bind(to_sql(series.first_seen))
        .bind(&stamp)
        .bind(series.updates as i64)
        .execute(&mut *tx)
        .await?;
    }

    for (key, index, bucket) in store.dirty() {
        sqlx::query(
            "INSERT INTO anomaly_baselines (series_key, bucket, ewma, mad, samples, last_update)
             VALUES (?, ?, ?, ?, ?, ?)
             ON CONFLICT(series_key, bucket) DO UPDATE SET
                 ewma = excluded.ewma, mad = excluded.mad, samples = excluded.samples,
                 last_update = excluded.last_update",
        )
        .bind(key)
        .bind(index as i64)
        .bind(bucket.ewma)
        .bind(bucket.mad)
        .bind(i64::from(bucket.samples))
        .bind(&stamp)
        .execute(&mut *tx)
        .await?;
    }

    tx.commit().await.context("écriture des baselines")
}

/// Oublie les baselines des séries disparues.
///
/// Sans cette purge, renommer un équipement laisserait sa baseline en base pour
/// toujours ; on garde une marge très large, une série saisonnière n'ayant de sens
/// que sur plusieurs semaines.
pub async fn purge_stale_baselines(pool: &SqlitePool, before: DateTime<Utc>) -> Result<u64> {
    let stamp = to_sql(before);
    let removed = sqlx::query("DELETE FROM anomaly_series WHERE last_update < ?")
        .bind(&stamp)
        .execute(pool)
        .await
        .context("purge des séries suivies")?
        .rows_affected();
    sqlx::query(
        "DELETE FROM anomaly_baselines
         WHERE series_key NOT IN (SELECT series_key FROM anomaly_series)",
    )
    .execute(pool)
    .await
    .context("purge des baselines orphelines")?;
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn les_horodatages_font_l_aller_retour_a_la_milliseconde() {
        let at =
            DateTime::parse_from_rfc3339("2026-03-01T12:34:56.789Z").unwrap().with_timezone(&Utc);
        let encoded = to_sql(at);
        assert_eq!(encoded, "2026-03-01T12:34:56.789Z");
        assert_eq!(from_sql(Some(encoded)), Some(at));
    }

    #[test]
    fn un_horodatage_absent_ou_illisible_donne_none() {
        assert_eq!(from_sql(None), None);
        assert_eq!(from_sql(Some("hier".to_string())), None);
    }

    #[test]
    fn un_json_corrompu_retombe_sur_la_valeur_par_defaut() {
        // Une base éditée à la main ne doit pas empêcher le moteur de démarrer.
        assert_eq!(json_or_default::<TargetSelector>("{{{"), TargetSelector::All);
        assert_eq!(json_or_default::<Vec<i64>>("pas du json"), Vec::<i64>::new());
    }

    #[tokio::test]
    async fn la_topologie_porte_le_verdict_injoignable_des_cibles_actives_seulement() {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::db::open(&dir.path().join("t.db")).await.unwrap();
        let cipher =
            crate::db::init_cipher(&pool, "secret-de-test-suffisamment-long").await.unwrap();
        let input = |name: &str| crate::db::targets::TargetInput {
            name: name.into(),
            address: format!("{name}.lan"),
            kind: "agent".into(),
            profile_id: None,
            parent_id: None,
            via_agent: None,
            interval: std::time::Duration::from_secs(30),
            enabled: true,
            tags: Default::default(),
            credential: None,
        };
        let silent = crate::db::targets::create(&pool, &cipher, &input("silent")).await.unwrap();
        let broken = crate::db::targets::create(&pool, &cipher, &input("broken")).await.unwrap();
        let paused = crate::db::targets::create(&pool, &cipher, &input("paused")).await.unwrap();
        let fine = crate::db::targets::create(&pool, &cipher, &input("fine")).await.unwrap();

        let down = "Device unreachable: no measurement for 120 s (limit 90 s)";
        crate::db::targets::record_probe(&pool, silent, Some(down)).await.unwrap();
        crate::db::targets::record_probe(&pool, broken, Some("Bad credentials")).await.unwrap();
        crate::db::targets::record_probe(&pool, paused, Some(down)).await.unwrap();
        crate::db::targets::set_enabled(&pool, paused, false).await.unwrap();
        crate::db::targets::record_probe(&pool, fine, None).await.unwrap();

        let nodes = list_target_nodes(&pool).await.unwrap();
        let unreachable = |id: TargetId| nodes.iter().find(|n| n.id == id).unwrap().unreachable;
        assert!(unreachable(silent), "un délai dépassé ou un silence, c'est injoignable");
        assert!(!unreachable(broken), "une erreur de configuration n'est pas une panne");
        assert!(!unreachable(paused), "une cible en pause garde un verdict périmé");
        assert!(!unreachable(fine));
    }
}
