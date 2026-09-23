//! Lecture de la base vers un lot exportable.
//!
//! Tout ce qui est chiffré avec la clé d'instance est **déchiffré ici** :
//! identifiants d'équipement, secrets de canaux, jetons des moniteurs en
//! poussée. Ils repartent chiffrés par la phrase de passe, dans
//! [`crate::backup::bundle::seal`]. C'est ce qui permet de restaurer sur une
//! instance neuve, dont la clé n'a rien à voir avec celle d'ici — et c'est
//! exactement pourquoi `secret.key` n'a pas sa place dans un lot.
//!
//! Ce qui n'y est pas, volontairement : les séries de VictoriaMetrics, l'état
//! et l'historique des alertes, les baselines, le journal d'audit, les sessions
//! ouvertes, l'enregistrement des agents et les caches de sonde. Ce sont des
//! constats, pas de la configuration : ils se reconstituent tout seuls.

use std::collections::{BTreeMap, HashMap};

use anyhow::{Context, Result};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde_json::Value;
use sqlx::{Row, SqlitePool};

use crate::backup::bundle::*;
use crate::crypto::Cipher;

/// Ce que l'opérateur choisit d'emporter.
#[derive(Debug, Clone, Copy, Default)]
pub struct ExportOptions {
    /// Empreintes de mots de passe, secrets TOTP et codes de secours des
    /// comptes.
    ///
    /// Faux par défaut : les comptes partent avec leur nom, leur rôle et leur
    /// rattachement SSO, mais sans de quoi se connecter. Un lot est fait pour
    /// voyager, et une empreinte Argon2 volée s'attaque hors ligne aussi
    /// longtemps qu'on veut. L'opérateur peut demander l'inverse quand il
    /// déménage une instance entière.
    pub include_account_secrets: bool,
}

/// Construit le lot complet à partir de la base.
pub async fn collect(pool: &SqlitePool, cipher: &Cipher, options: ExportOptions) -> Result<Bundle> {
    let refs = target_refs(pool).await?;
    let channel_names = channel_names(pool).await?;

    Ok(Bundle {
        version: VERSION,
        created_at: chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string(),
        source_version: env!("CARGO_PKG_VERSION").to_string(),
        targets: targets(pool, cipher, &refs).await?,
        rules: rules(pool, &refs, &channel_names).await?,
        rule_overrides: overrides(pool, &refs).await?,
        channels: channels(pool, cipher).await?,
        notify_policy: crate::auth::settings::get::<Value>(
            pool,
            crate::notify::policy_store::POLICY_KEY,
        )
        .await?,
        silences: silences(pool, &refs).await?,
        status_pages: status_pages(pool, &refs).await?,
        incidents: incidents(pool).await?,
        users: users(pool, cipher, options).await?,
        agent_tokens: agent_tokens(pool).await?,
        api_tokens: api_tokens(pool).await?,
        push_monitors: push_monitors(pool, cipher, &refs).await?,
    })
}

/// Identifiant d'équipement → référence portable (`kind|address`).
async fn target_refs(pool: &SqlitePool) -> Result<HashMap<i64, String>> {
    let rows = sqlx::query("SELECT id, kind, address FROM targets")
        .fetch_all(pool)
        .await
        .context("lecture des équipements")?;
    rows.iter()
        .map(|row| {
            let kind: String = row.try_get("kind")?;
            let address: String = row.try_get("address")?;
            Ok((row.try_get("id")?, target_ref(&kind, &address)))
        })
        .collect()
}

async fn channel_names(pool: &SqlitePool) -> Result<HashMap<i64, String>> {
    let rows = sqlx::query("SELECT id, name FROM notification_channels")
        .fetch_all(pool)
        .await
        .context("lecture des canaux")?;
    rows.iter().map(|row| Ok((row.try_get("id")?, row.try_get("name")?))).collect()
}

async fn targets(
    pool: &SqlitePool,
    cipher: &Cipher,
    refs: &HashMap<i64, String>,
) -> Result<Vec<BundleTarget>> {
    let rows = sqlx::query(
        "SELECT id, name, address, kind, profile_id, parent_id, via_agent, interval_secs,
                enabled, tags, credential_enc
         FROM targets ORDER BY id",
    )
    .fetch_all(pool)
    .await
    .context("lecture des équipements")?;

    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        let id: i64 = row.try_get("id")?;
        let tags: String = row.try_get("tags")?;
        let encrypted: Option<Vec<u8>> = row.try_get("credential_enc")?;
        let credential = match encrypted {
            None => None,
            Some(bytes) => match cipher.decrypt(&bytes) {
                Ok(plain) => serde_json::from_slice::<Value>(&plain).ok(),
                // Un identifiant illisible ne doit pas faire échouer tout
                // l'export : l'équipement part sans, et le compte rendu de
                // restauration le dira.
                Err(error) => {
                    tracing::warn!(target = id, %error, "identifiants illisibles, exportés vides");
                    None
                }
            },
        };
        out.push(BundleTarget {
            kind: row.try_get("kind")?,
            address: row.try_get("address")?,
            name: row.try_get("name")?,
            profile_id: row.try_get("profile_id")?,
            parent: row.try_get::<Option<i64>, _>("parent_id")?.and_then(|p| refs.get(&p).cloned()),
            via_agent: row
                .try_get::<Option<i64>, _>("via_agent")?
                .and_then(|p| refs.get(&p).cloned()),
            interval_secs: row.try_get("interval_secs")?,
            enabled: row.try_get::<i64, _>("enabled")? != 0,
            tags: serde_json::from_str(&tags).unwrap_or_default(),
            credential,
        });
    }
    Ok(out)
}

/// Remplace les identifiants d'équipement d'un sélecteur par des références.
fn selector_to_bundle(raw: &str, refs: &HashMap<i64, String>) -> Value {
    let mut value: Value = serde_json::from_str(raw).unwrap_or(serde_json::json!({"kind": "all"}));
    if value.get("kind").and_then(Value::as_str) == Some("ids")
        && let Some(ids) = value.get("ids").and_then(Value::as_array)
    {
        let named: Vec<Value> = ids
            .iter()
            .filter_map(Value::as_i64)
            .filter_map(|id| refs.get(&id))
            .map(|r| Value::String(r.clone()))
            .collect();
        value = serde_json::json!({ "kind": "refs", "refs": named });
    }
    value
}

async fn rules(
    pool: &SqlitePool,
    refs: &HashMap<i64, String>,
    channels: &HashMap<i64, String>,
) -> Result<Vec<BundleRule>> {
    let rows = sqlx::query(
        "SELECT uid, name, description, kind, query, operator, threshold, clear_threshold,
                for_secs, severity, selector, channels, params, unit, repeat_secs,
                escalate_after_secs, enabled, builtin
         FROM alert_rules ORDER BY uid",
    )
    .fetch_all(pool)
    .await
    .context("lecture des règles")?;

    rows.iter()
        .map(|row| {
            let selector: String = row.try_get("selector")?;
            let channel_ids: String = row.try_get("channels")?;
            let params: String = row.try_get("params")?;
            let ids: Vec<i64> = serde_json::from_str(&channel_ids).unwrap_or_default();
            Ok(BundleRule {
                uid: row.try_get("uid")?,
                name: row.try_get("name")?,
                description: row.try_get("description")?,
                kind: row.try_get("kind")?,
                query: row.try_get("query")?,
                operator: row.try_get("operator")?,
                threshold: row.try_get("threshold")?,
                clear_threshold: row.try_get("clear_threshold")?,
                for_secs: row.try_get("for_secs")?,
                severity: row.try_get("severity")?,
                selector: selector_to_bundle(&selector, refs),
                channels: ids.iter().filter_map(|id| channels.get(id).cloned()).collect(),
                params: serde_json::from_str(&params).unwrap_or(Value::Null),
                unit: row.try_get("unit")?,
                repeat_secs: row.try_get("repeat_secs")?,
                escalate_after_secs: row.try_get("escalate_after_secs")?,
                enabled: row.try_get::<i64, _>("enabled")? != 0,
                builtin: row.try_get::<i64, _>("builtin")? != 0,
            })
        })
        .collect()
}

async fn overrides(pool: &SqlitePool, refs: &HashMap<i64, String>) -> Result<Vec<BundleOverride>> {
    let rows = sqlx::query(
        "SELECT rule_uid, target_id, threshold, clear_threshold, enabled
         FROM rule_overrides ORDER BY rule_uid, target_id",
    )
    .fetch_all(pool)
    .await
    .context("lecture des surcharges")?;

    let mut out = Vec::new();
    for row in &rows {
        let target_id: i64 = row.try_get("target_id")?;
        let Some(target) = refs.get(&target_id).cloned() else { continue };
        let enabled: Option<i64> = row.try_get("enabled")?;
        out.push(BundleOverride {
            rule_uid: row.try_get("rule_uid")?,
            target,
            threshold: row.try_get("threshold")?,
            clear_threshold: row.try_get("clear_threshold")?,
            enabled: enabled.map(|v| v != 0),
        });
    }
    Ok(out)
}

async fn channels(pool: &SqlitePool, cipher: &Cipher) -> Result<Vec<BundleChannel>> {
    let rows = sqlx::query(
        "SELECT name, kind, enabled, settings, secret_enc, policy
         FROM notification_channels ORDER BY name",
    )
    .fetch_all(pool)
    .await
    .context("lecture des canaux")?;

    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        let name: String = row.try_get("name")?;
        let settings: String = row.try_get("settings")?;
        let policy: String = row.try_get("policy")?;
        let encrypted: Option<Vec<u8>> = row.try_get("secret_enc")?;
        let secrets = match encrypted {
            None => None,
            Some(bytes) => match cipher.decrypt(&bytes) {
                Ok(plain) => serde_json::from_slice::<Value>(&plain).ok(),
                Err(error) => {
                    tracing::warn!(channel = %name, %error, "secrets illisibles, exportés vides");
                    None
                }
            },
        };
        out.push(BundleChannel {
            name,
            kind: row.try_get("kind")?,
            enabled: row.try_get::<i64, _>("enabled")? != 0,
            settings: serde_json::from_str(&settings).unwrap_or(Value::Null),
            secrets,
            policy: serde_json::from_str(&policy).unwrap_or(Value::Null),
        });
    }
    Ok(out)
}

async fn silences(pool: &SqlitePool, refs: &HashMap<i64, String>) -> Result<Vec<BundleSilence>> {
    let rows = sqlx::query(
        "SELECT name, comment, target_id, matchers, schedule, enabled FROM silences ORDER BY id",
    )
    .fetch_all(pool)
    .await
    .context("lecture des silences")?;

    rows.iter()
        .map(|row| {
            let matchers: String = row.try_get("matchers")?;
            let schedule: String = row.try_get("schedule")?;
            Ok(BundleSilence {
                name: row.try_get("name")?,
                comment: row.try_get("comment")?,
                target: row
                    .try_get::<Option<i64>, _>("target_id")?
                    .and_then(|id| refs.get(&id).cloned()),
                matchers: serde_json::from_str(&matchers).unwrap_or(Value::Null),
                schedule: serde_json::from_str(&schedule).unwrap_or(Value::Null),
                enabled: row.try_get::<i64, _>("enabled")? != 0,
            })
        })
        .collect()
}

async fn status_pages(
    pool: &SqlitePool,
    refs: &HashMap<i64, String>,
) -> Result<Vec<BundleStatusPage>> {
    let rows = sqlx::query(
        "SELECT id, slug, title, description, published, theme, show_uptime_days
         FROM status_pages ORDER BY id",
    )
    .fetch_all(pool)
    .await
    .context("lecture des pages de statut")?;

    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        let id: i64 = row.try_get("id")?;
        let items = sqlx::query(
            "SELECT target_id, label, group_name, position FROM status_page_items
             WHERE page_id = ? ORDER BY position, id",
        )
        .bind(id)
        .fetch_all(pool)
        .await
        .context("lecture des services d'une page de statut")?;

        let mut listed = Vec::with_capacity(items.len());
        for item in &items {
            let target_id: i64 = item.try_get("target_id")?;
            let Some(target) = refs.get(&target_id).cloned() else { continue };
            listed.push(BundleStatusItem {
                target,
                label: item.try_get("label")?,
                group_name: item.try_get("group_name")?,
                position: item.try_get("position")?,
            });
        }

        out.push(BundleStatusPage {
            slug: row.try_get("slug")?,
            title: row.try_get("title")?,
            description: row.try_get("description")?,
            published: row.try_get::<i64, _>("published")? != 0,
            theme: row.try_get("theme")?,
            show_uptime_days: row.try_get("show_uptime_days")?,
            items: listed,
        });
    }
    Ok(out)
}

async fn incidents(pool: &SqlitePool) -> Result<Vec<BundleIncident>> {
    let slugs: HashMap<i64, String> = sqlx::query("SELECT id, slug FROM status_pages")
        .fetch_all(pool)
        .await
        .context("lecture des pages de statut")?
        .iter()
        .map(|row| Ok((row.try_get("id")?, row.try_get("slug")?)))
        .collect::<Result<_>>()?;

    let rows = sqlx::query(
        "SELECT id, page_id, title, kind, status, severity, starts_at, ends_at
         FROM incidents ORDER BY id",
    )
    .fetch_all(pool)
    .await
    .context("lecture des incidents")?;

    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        let id: i64 = row.try_get("id")?;
        let updates = sqlx::query(
            "SELECT status, body, created_at FROM incident_updates
             WHERE incident_id = ? ORDER BY id",
        )
        .bind(id)
        .fetch_all(pool)
        .await
        .context("lecture du fil d'un incident")?;

        out.push(BundleIncident {
            page: row.try_get::<Option<i64>, _>("page_id")?.and_then(|p| slugs.get(&p).cloned()),
            title: row.try_get("title")?,
            kind: row.try_get("kind")?,
            status: row.try_get("status")?,
            severity: row.try_get("severity")?,
            starts_at: row.try_get("starts_at")?,
            ends_at: row.try_get("ends_at")?,
            updates: updates
                .iter()
                .map(|u| {
                    Ok(BundleIncidentUpdate {
                        status: u.try_get("status")?,
                        body: u.try_get("body")?,
                        created_at: u.try_get("created_at")?,
                    })
                })
                .collect::<Result<_>>()?,
        });
    }
    Ok(out)
}

async fn users(
    pool: &SqlitePool,
    cipher: &Cipher,
    options: ExportOptions,
) -> Result<Vec<BundleUser>> {
    let rows = sqlx::query(
        "SELECT id, username, display_name, role, password_hash, oidc_subject, oidc_issuer,
                created_at, disabled, totp_secret, totp_enabled
         FROM users ORDER BY id",
    )
    .fetch_all(pool)
    .await
    .context("lecture des comptes")?;

    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        let id: i64 = row.try_get("id")?;
        let mut user = BundleUser {
            username: row.try_get("username")?,
            display_name: row.try_get("display_name")?,
            role: row.try_get("role")?,
            disabled: row.try_get::<i64, _>("disabled")? != 0,
            oidc_subject: row.try_get("oidc_subject")?,
            oidc_issuer: row.try_get("oidc_issuer")?,
            created_at: row.try_get("created_at")?,
            password_hash: None,
            totp_secret: None,
            totp_enabled: false,
            recovery_codes: Vec::new(),
        };

        if options.include_account_secrets {
            user.password_hash = row.try_get("password_hash")?;
            user.totp_enabled = row.try_get::<i64, _>("totp_enabled")? != 0;
            if let Some(secret) = row.try_get::<Option<Vec<u8>>, _>("totp_secret")?
                && let Ok(plain) = cipher.decrypt(&secret)
            {
                user.totp_secret = Some(BASE64.encode(plain));
            }
            let codes = sqlx::query(
                "SELECT code_hash FROM totp_recovery_codes WHERE user_id = ? AND used_at IS NULL",
            )
            .bind(id)
            .fetch_all(pool)
            .await
            .context("lecture des codes de secours")?;
            user.recovery_codes = codes
                .iter()
                .map(|c| Ok(BASE64.encode(c.try_get::<Vec<u8>, _>("code_hash")?)))
                .collect::<Result<_>>()?;
        }
        out.push(user);
    }
    Ok(out)
}

async fn agent_tokens(pool: &SqlitePool) -> Result<Vec<BundleAgentToken>> {
    let rows = sqlx::query(
        "SELECT name, token_hash, prefix, created_at, revoked_at FROM agent_tokens ORDER BY id",
    )
    .fetch_all(pool)
    .await
    .context("lecture des jetons d'agent")?;

    rows.iter()
        .map(|row| {
            Ok(BundleAgentToken {
                name: row.try_get("name")?,
                token_hash: row.try_get("token_hash")?,
                prefix: row.try_get("prefix")?,
                created_at: row.try_get("created_at")?,
                revoked_at: row.try_get("revoked_at")?,
            })
        })
        .collect()
}

async fn api_tokens(pool: &SqlitePool) -> Result<Vec<BundleApiToken>> {
    let rows = sqlx::query(
        "SELECT t.name, t.prefix, t.token_hash, t.scope, t.created_at, t.revoked_at,
                u.username AS owner
         FROM api_tokens t LEFT JOIN users u ON u.id = t.user_id ORDER BY t.id",
    )
    .fetch_all(pool)
    .await
    .context("lecture des jetons d'API")?;

    rows.iter()
        .map(|row| {
            Ok(BundleApiToken {
                name: row.try_get("name")?,
                prefix: row.try_get("prefix")?,
                token_hash: BASE64.encode(row.try_get::<Vec<u8>, _>("token_hash")?),
                scope: row.try_get("scope")?,
                created_at: row.try_get("created_at")?,
                revoked_at: row.try_get("revoked_at")?,
                user: row.try_get("owner")?,
            })
        })
        .collect()
}

async fn push_monitors(
    pool: &SqlitePool,
    cipher: &Cipher,
    refs: &HashMap<i64, String>,
) -> Result<Vec<BundlePushMonitor>> {
    let rows = sqlx::query("SELECT target_id, token_enc FROM push_monitors ORDER BY target_id")
        .fetch_all(pool)
        .await
        .context("lecture des moniteurs en poussée")?;

    let mut out = Vec::new();
    for row in &rows {
        let target_id: i64 = row.try_get("target_id")?;
        let Some(target) = refs.get(&target_id).cloned() else { continue };
        let encrypted: Vec<u8> = row.try_get("token_enc")?;
        let Ok(plain) = cipher.decrypt(&encrypted) else {
            tracing::warn!(target = target_id, "jeton de poussée illisible, non exporté");
            continue;
        };
        let Ok(token) = String::from_utf8(plain) else { continue };
        out.push(BundlePushMonitor { target, token });
    }
    Ok(out)
}

/// Sections du lot et ce qu'elles contiennent, telles que l'interface et la
/// documentation les présentent. Figé ici pour que les trois ne divergent pas.
pub fn sections() -> BTreeMap<&'static str, &'static str> {
    BTreeMap::from([
        ("targets", "Devices, their options and their credentials."),
        ("rules", "Alert rules, built-in ones included when you changed them."),
        ("rule_overrides", "Per-device thresholds and exclusions."),
        ("channels", "Notification channels and their secrets."),
        ("silences", "Maintenance windows."),
        ("status_pages", "Public status pages and the services they list."),
        ("incidents", "Incidents and maintenance announcements."),
        ("users", "Accounts, their role and their SSO link."),
        ("agent_tokens", "Agent enrolment tokens, as hashes: installed agents keep working."),
        ("api_tokens", "API tokens, as hashes: existing tokens keep working."),
        ("push_monitors", "Heartbeat tokens, so the URLs your cron jobs call stay the same."),
    ])
}
