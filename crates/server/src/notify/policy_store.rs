//! Persistance de la politique de notification : politique globale (table
//! `settings`), politique par canal (`notification_channels.policy`), surcharges
//! par équipement (`rule_overrides`), file d'attente et registre des envois.
//!
//! Le module ne raisonne pas : il charge un [`Ledger`] pour
//! [`crate::alerting::notify_policy::plan`] et applique le [`Plan`] rendu.

use std::collections::HashMap;

use anyhow::{Context, Result};
use chrono::{DateTime, SecondsFormat, TimeDelta, Utc};
use sqlx::{Row, SqlitePool};

use crate::alerting::group::{GroupItem, NotifyReason};
use crate::alerting::model::TargetId;
use crate::alerting::notify_policy::{
    ChannelPolicy, GlobalPolicy, Hold, LEDGER_HOUR_SECS, Ledger, LogEntry, Plan, QueuedItem,
};
use crate::alerting::overrides::RuleOverride;
use crate::auth::settings;

/// Clé de la politique globale dans `settings`.
pub const POLICY_KEY: &str = "notify_policy";

fn to_sql(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn from_sql(raw: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(raw).map(|at| at.with_timezone(&Utc)).unwrap_or_default()
}

// --------------------------------------------------------------------------
// Politique globale et par canal
// --------------------------------------------------------------------------

pub async fn load_global(pool: &SqlitePool) -> Result<GlobalPolicy> {
    Ok(settings::get::<GlobalPolicy>(pool, POLICY_KEY).await?.unwrap_or_default())
}

pub async fn save_global(pool: &SqlitePool, policy: &GlobalPolicy) -> Result<()> {
    settings::set(pool, POLICY_KEY, policy).await
}

/// Politique de chaque canal, canal par canal. Un JSON illisible retombe sur la
/// politique par défaut : mieux vaut un canal trop bavard qu'un canal muet.
pub async fn load_channel_policies(pool: &SqlitePool) -> Result<HashMap<i64, ChannelPolicy>> {
    let rows = sqlx::query("SELECT id, policy FROM notification_channels")
        .fetch_all(pool)
        .await
        .context("reading channel policies")?;
    rows.iter()
        .map(|row| {
            let id: i64 = row.try_get("id")?;
            let raw: String = row.try_get("policy")?;
            Ok((id, serde_json::from_str(&raw).unwrap_or_default()))
        })
        .collect()
}

pub async fn save_channel_policy(pool: &SqlitePool, id: i64, policy: &ChannelPolicy) -> Result<()> {
    sqlx::query("UPDATE notification_channels SET policy = ? WHERE id = ?")
        .bind(serde_json::to_string(policy)?)
        .bind(id)
        .execute(pool)
        .await
        .context("saving channel policy")?;
    Ok(())
}

// --------------------------------------------------------------------------
// Surcharges par équipement
// --------------------------------------------------------------------------

fn override_from_row(row: &sqlx::sqlite::SqliteRow) -> Result<RuleOverride> {
    let enabled: Option<i64> = row.try_get("enabled")?;
    Ok(RuleOverride {
        rule_uid: row.try_get("rule_uid")?,
        target_id: row.try_get("target_id")?,
        threshold: row.try_get("threshold")?,
        clear_threshold: row.try_get("clear_threshold")?,
        enabled: enabled.map(|value| value != 0),
    })
}

/// Toutes les surcharges, ou celles d'une règle ou d'un équipement.
pub async fn list_overrides(
    pool: &SqlitePool,
    rule_uid: Option<&str>,
    target_id: Option<TargetId>,
) -> Result<Vec<RuleOverride>> {
    let rows = sqlx::query(
        "SELECT rule_uid, target_id, threshold, clear_threshold, enabled
         FROM rule_overrides
         WHERE (? IS NULL OR rule_uid = ?) AND (? IS NULL OR target_id = ?)
         ORDER BY rule_uid, target_id",
    )
    .bind(rule_uid)
    .bind(rule_uid)
    .bind(target_id)
    .bind(target_id)
    .fetch_all(pool)
    .await
    .context("reading rule overrides")?;
    rows.iter().map(override_from_row).collect()
}

/// Crée ou remplace une surcharge. Une surcharge vide est effacée : elle ne
/// changerait rien et encombrerait l'interface.
pub async fn upsert_override(pool: &SqlitePool, over: &RuleOverride) -> Result<()> {
    if over.is_empty() {
        delete_override(pool, &over.rule_uid, over.target_id).await?;
        return Ok(());
    }
    sqlx::query(
        "INSERT INTO rule_overrides (rule_uid, target_id, threshold, clear_threshold, enabled)
         VALUES (?, ?, ?, ?, ?)
         ON CONFLICT(rule_uid, target_id) DO UPDATE SET
             threshold = excluded.threshold, clear_threshold = excluded.clear_threshold,
             enabled = excluded.enabled, updated_at = datetime('now')",
    )
    .bind(&over.rule_uid)
    .bind(over.target_id)
    .bind(over.threshold)
    .bind(over.clear_threshold)
    .bind(over.enabled.map(i64::from))
    .execute(pool)
    .await
    .context("saving rule override")?;
    Ok(())
}

pub async fn delete_override(
    pool: &SqlitePool,
    rule_uid: &str,
    target_id: TargetId,
) -> Result<bool> {
    let result = sqlx::query("DELETE FROM rule_overrides WHERE rule_uid = ? AND target_id = ?")
        .bind(rule_uid)
        .bind(target_id)
        .execute(pool)
        .await
        .context("deleting rule override")?;
    Ok(result.rows_affected() > 0)
}

// --------------------------------------------------------------------------
// File d'attente et registre
// --------------------------------------------------------------------------

/// Charge ce dont un cycle a besoin. `lookback` couvre la fenêtre la plus
/// longue parmi l'heure du plafond, la fenêtre de battement et les délais
/// minimaux des canaux.
pub async fn load_ledger(
    pool: &SqlitePool,
    now: DateTime<Utc>,
    lookback: TimeDelta,
) -> Result<Ledger> {
    let since = to_sql(now - lookback.max(TimeDelta::seconds(LEDGER_HOUR_SECS)));

    let queue_rows = sqlx::query(
        "SELECT id, channel_id, target_id, target_name, hold, item, resolved_meanwhile, queued_at
         FROM notify_queue ORDER BY queued_at, id",
    )
    .fetch_all(pool)
    .await
    .context("reading the notification queue")?;
    let mut queue = Vec::with_capacity(queue_rows.len());
    for row in &queue_rows {
        let raw: String = row.try_get("item")?;
        // Une ligne illisible (format d'une version antérieure) est abandonnée
        // plutôt que de bloquer la file entière.
        let Ok(item) = serde_json::from_str::<GroupItem>(&raw) else { continue };
        let hold: String = row.try_get("hold")?;
        let queued_at: String = row.try_get("queued_at")?;
        queue.push(QueuedItem {
            id: Some(row.try_get("id")?),
            channel_id: row.try_get("channel_id")?,
            target_id: row.try_get("target_id")?,
            target_name: row.try_get("target_name")?,
            hold: Hold::parse(&hold),
            item,
            resolved_meanwhile: row.try_get::<i64, _>("resolved_meanwhile")? != 0,
            queued_at: from_sql(&queued_at),
        });
    }

    let log_rows =
        sqlx::query("SELECT channel_id, fingerprint, reason, at FROM notify_log WHERE at > ?")
            .bind(&since)
            .fetch_all(pool)
            .await
            .context("reading the notification log")?;
    let mut log = Vec::with_capacity(log_rows.len());
    for row in &log_rows {
        let reason: String = row.try_get("reason")?;
        let at: String = row.try_get("at")?;
        let Some(reason) = NotifyReason::parse(&reason) else { continue };
        log.push(LogEntry {
            channel_id: row.try_get("channel_id")?,
            fingerprint: row.try_get("fingerprint")?,
            reason,
            at: from_sql(&at),
        });
    }

    let flap_rows =
        sqlx::query("SELECT fingerprint, held_until FROM notify_flap WHERE held_until > ?")
            .bind(to_sql(now))
            .fetch_all(pool)
            .await
            .context("reading flap holds")?;
    let mut flap_holds = HashMap::with_capacity(flap_rows.len());
    for row in &flap_rows {
        let until: String = row.try_get("held_until")?;
        flap_holds.insert(row.try_get::<String, _>("fingerprint")?, from_sql(&until));
    }

    let message_rows = sqlx::query("SELECT channel_id, at FROM notify_messages WHERE at > ?")
        .bind(to_sql(now - TimeDelta::seconds(LEDGER_HOUR_SECS)))
        .fetch_all(pool)
        .await
        .context("reading sent messages")?;
    let mut messages = Vec::with_capacity(message_rows.len());
    for row in &message_rows {
        let at: String = row.try_get("at")?;
        messages.push((row.try_get::<i64, _>("channel_id")?, from_sql(&at)));
    }

    Ok(Ledger { queue, log, flap_holds, messages })
}

/// Écrit ce qui ne dépend pas des envois : file, retenues, entrées de pipeline.
pub async fn apply_plan(pool: &SqlitePool, plan: &Plan) -> Result<()> {
    let mut tx = pool.begin().await?;
    for id in &plan.dequeue {
        sqlx::query("DELETE FROM notify_queue WHERE id = ?").bind(id).execute(&mut *tx).await?;
    }
    for item in &plan.enqueue {
        upsert_queued(&mut tx, item).await?;
    }
    for (fingerprint, until) in &plan.flap_holds {
        sqlx::query(
            "INSERT INTO notify_flap (fingerprint, held_until) VALUES (?, ?)
             ON CONFLICT(fingerprint) DO UPDATE SET held_until = excluded.held_until",
        )
        .bind(fingerprint)
        .bind(to_sql(*until))
        .execute(&mut *tx)
        .await?;
    }
    for entry in &plan.intake {
        insert_log(&mut tx, entry).await?;
    }
    tx.commit().await.context("applying the notification plan")
}

async fn upsert_queued(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    item: &QueuedItem,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO notify_queue
             (channel_id, fingerprint, target_id, target_name, hold, item, resolved_meanwhile,
              queued_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(channel_id, fingerprint) DO UPDATE SET
             target_id = excluded.target_id, target_name = excluded.target_name,
             hold = excluded.hold, item = excluded.item,
             resolved_meanwhile = excluded.resolved_meanwhile,
             queued_at = excluded.queued_at",
    )
    .bind(item.channel_id)
    .bind(&item.item.fingerprint)
    .bind(item.target_id)
    .bind(&item.target_name)
    .bind(item.hold.as_str())
    .bind(serde_json::to_string(&item.item)?)
    .bind(i64::from(item.resolved_meanwhile))
    .bind(to_sql(item.queued_at))
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn insert_log(tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>, entry: &LogEntry) -> Result<()> {
    sqlx::query("INSERT INTO notify_log (channel_id, fingerprint, reason, at) VALUES (?, ?, ?, ?)")
        .bind(entry.channel_id)
        .bind(&entry.fingerprint)
        .bind(entry.reason.as_str())
        .bind(to_sql(entry.at))
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// Après un envoi réussi : la file est vidée de ce qui est parti, le message et
/// ses lignes sont consignés.
pub async fn record_sent(
    pool: &SqlitePool,
    channel_id: i64,
    queue_ids: &[i64],
    sent: &[(String, NotifyReason)],
    now: DateTime<Utc>,
) -> Result<()> {
    let mut tx = pool.begin().await?;
    for id in queue_ids {
        sqlx::query("DELETE FROM notify_queue WHERE id = ?").bind(id).execute(&mut *tx).await?;
    }
    sqlx::query("INSERT INTO notify_messages (channel_id, items, at) VALUES (?, ?, ?)")
        .bind(channel_id)
        .bind(sent.len() as i64)
        .bind(to_sql(now))
        .execute(&mut *tx)
        .await?;
    for (fingerprint, reason) in sent {
        insert_log(
            &mut tx,
            &LogEntry { channel_id, fingerprint: fingerprint.clone(), reason: *reason, at: now },
        )
        .await?;
    }
    tx.commit().await.context("recording a sent notification")
}

/// Après un envoi raté : ce qui devait partir retourne en file, daté d'assez
/// loin pour repartir au cycle suivant sans attendre une nouvelle fenêtre.
pub async fn requeue(pool: &SqlitePool, items: &[QueuedItem], window: TimeDelta) -> Result<()> {
    let mut tx = pool.begin().await?;
    for item in items {
        let mut retry = item.clone();
        retry.queued_at = retry.queued_at.min(Utc::now() - window);
        upsert_queued(&mut tx, &retry).await?;
    }
    tx.commit().await.context("requeueing after a failed delivery")
}

/// Entretien : registre et messages plus vieux que la fenêtre utile, retenues
/// expirées.
pub async fn purge(pool: &SqlitePool, now: DateTime<Utc>, lookback: TimeDelta) -> Result<u64> {
    let cutoff = to_sql(now - lookback.max(TimeDelta::seconds(LEDGER_HOUR_SECS)));
    let mut removed = 0;
    removed += sqlx::query("DELETE FROM notify_log WHERE at < ?")
        .bind(&cutoff)
        .execute(pool)
        .await?
        .rows_affected();
    removed += sqlx::query("DELETE FROM notify_messages WHERE at < ?")
        .bind(to_sql(now - TimeDelta::seconds(LEDGER_HOUR_SECS)))
        .execute(pool)
        .await?
        .rows_affected();
    removed += sqlx::query("DELETE FROM notify_flap WHERE held_until < ?")
        .bind(to_sql(now))
        .execute(pool)
        .await?
        .rows_affected();
    Ok(removed)
}

/// Fenêtre de registre à charger pour un cycle : la plus longue des fenêtres
/// dont dépend une décision.
pub fn lookback(global: &GlobalPolicy, channels: &HashMap<i64, ChannelPolicy>) -> TimeDelta {
    let longest_interval = channels.values().map(|p| p.min_interval_secs).max().unwrap_or(0);
    let secs = [LEDGER_HOUR_SECS, i64::from(global.flap_window_secs), i64::from(longest_interval)]
        .into_iter()
        .max()
        .unwrap_or(LEDGER_HOUR_SECS);
    TimeDelta::seconds(secs)
}
