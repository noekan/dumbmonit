//! Restauration d'un lot dans une base.
//!
//! Trois principes, et ils tiennent tous les trois dans le compte rendu que
//! l'interface montre avant d'appliquer quoi que ce soit :
//!
//! * **rien n'est jamais supprimé**. Restaurer, c'est faire en sorte que ce que
//!   le lot décrit existe ; ce que l'instance a en plus reste ;
//! * **rien n'est jamais dupliqué**. Chaque chose a une clé naturelle — type +
//!   adresse pour un équipement, `uid` pour une règle, nom pour un canal — et
//!   c'est sur elle que l'on retrouve ce qui est déjà là. Restaurer deux fois
//!   le même lot ne crée rien la seconde fois ;
//! * **la simulation est le geste réel, annulé**. Le même code tourne dans les
//!   deux cas, dans une transaction que l'on valide ou que l'on annule. Le
//!   compte rendu d'une simulation ne peut donc pas mentir sur ce que
//!   l'application ferait.
//!
//! Les comptes font exception à « mettre à jour » : un compte qui existe déjà
//! est laissé tel quel. Écraser le rôle ou le mot de passe de l'administrateur
//! connecté au moyen d'un fichier serait le plus court chemin vers une instance
//! dont plus personne n'a la clé.

use std::collections::HashMap;

use anyhow::{Context, Result};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde::Serialize;
use serde_json::Value;
use sqlx::{Row, Sqlite, SqlitePool, Transaction};

use crate::backup::bundle::*;
use crate::crypto::Cipher;

/// Sort d'une ligne du lot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestoreOutcome {
    Created,
    Updated,
    Skipped,
}

/// Ce qu'une section du lot a donné.
#[derive(Debug, Clone, Default, Serialize)]
pub struct SectionReport {
    pub section: String,
    pub created: usize,
    pub updated: usize,
    pub skipped: usize,
    /// Ce qui mérite d'être lu ligne par ligne : une référence introuvable, une
    /// ligne refusée par la base.
    pub notes: Vec<String>,
}

impl SectionReport {
    fn new(section: &str) -> Self {
        Self { section: section.to_string(), ..Self::default() }
    }

    fn count(&mut self, outcome: RestoreOutcome) {
        match outcome {
            RestoreOutcome::Created => self.created += 1,
            RestoreOutcome::Updated => self.updated += 1,
            RestoreOutcome::Skipped => self.skipped += 1,
        }
    }

    fn refused(&mut self, what: &str, error: &anyhow::Error) {
        self.skipped += 1;
        self.notes.push(format!("{what} was refused by the database: {error}"));
    }
}

/// Compte rendu complet d'une restauration, simulée ou appliquée.
#[derive(Debug, Clone, Default, Serialize)]
pub struct RestoreReport {
    /// Faux : rien n'a été écrit, la transaction a été annulée.
    pub applied: bool,
    /// Date d'écriture du lot et version du serveur qui l'a produit.
    pub created_at: String,
    pub source_version: String,
    pub sections: Vec<SectionReport>,
    pub created: usize,
    pub updated: usize,
    pub skipped: usize,
    /// Ce que l'opérateur doit savoir avant, ou juste après, d'appliquer.
    pub warnings: Vec<String>,
}

impl RestoreReport {
    fn push(&mut self, section: SectionReport) {
        self.created += section.created;
        self.updated += section.updated;
        self.skipped += section.skipped;
        self.sections.push(section);
    }
}

/// Restaure un lot. `apply` faux : tout est calculé puis annulé.
pub async fn restore(
    pool: &SqlitePool,
    cipher: &Cipher,
    bundle: &Bundle,
    apply: bool,
) -> Result<RestoreReport> {
    let mut tx = pool.begin().await.context("ouverture de la transaction de restauration")?;

    let mut report = RestoreReport {
        applied: apply,
        created_at: bundle.created_at.clone(),
        source_version: bundle.source_version.clone(),
        ..RestoreReport::default()
    };

    report.push(restore_users(&mut tx, cipher, bundle).await?);
    report.push(restore_targets(&mut tx, cipher, bundle).await?);

    let refs = target_ids(&mut tx).await?;

    report.push(restore_channels(&mut tx, cipher, bundle).await?);
    let channels = channel_ids(&mut tx).await?;

    report.push(restore_rules(&mut tx, bundle, &refs, &channels).await?);
    report.push(restore_overrides(&mut tx, bundle, &refs).await?);
    report.push(restore_notify_policy(&mut tx, bundle).await?);
    report.push(restore_silences(&mut tx, bundle, &refs).await?);
    report.push(restore_status_pages(&mut tx, bundle, &refs).await?);
    report.push(restore_incidents(&mut tx, bundle).await?);
    report.push(restore_agent_tokens(&mut tx, bundle).await?);
    report.push(restore_api_tokens(&mut tx, bundle).await?);
    report.push(restore_push_monitors(&mut tx, cipher, bundle, &refs).await?);

    warnings(bundle, &mut report);

    if apply {
        tx.commit().await.context("validation de la restauration")?;
    } else {
        tx.rollback().await.context("annulation de la simulation")?;
    }
    Ok(report)
}

fn warnings(bundle: &Bundle, report: &mut RestoreReport) {
    if bundle.users.iter().any(|u| u.password_hash.is_none() && u.oidc_subject.is_none()) {
        report.warnings.push(
            "This backup holds no account passwords. Restored accounts cannot sign in until an \
             administrator sets a password for them, or until they sign in through SSO."
                .to_string(),
        );
    }
    if !bundle.targets.is_empty() {
        report.warnings.push(
            "Device credentials from the backup are now encrypted with this server's own \
             instance secret (/data/secret.key). Back that file up: without it they cannot be \
             read again."
                .to_string(),
        );
    }
    if report.sections.iter().any(|s| !s.notes.is_empty()) {
        report.warnings.push(
            "Some entries could not be restored as they were; read the notes of each section."
                .to_string(),
        );
    }
}

// --------------------------------------------------------------------------
// Équipements
// --------------------------------------------------------------------------

async fn target_ids(tx: &mut Transaction<'_, Sqlite>) -> Result<HashMap<String, i64>> {
    let rows = sqlx::query("SELECT id, kind, address FROM targets")
        .fetch_all(&mut **tx)
        .await
        .context("relecture des équipements")?;
    rows.iter()
        .map(|row| {
            let kind: String = row.try_get("kind")?;
            let address: String = row.try_get("address")?;
            Ok((target_ref(&kind, &address), row.try_get("id")?))
        })
        .collect()
}

async fn channel_ids(tx: &mut Transaction<'_, Sqlite>) -> Result<HashMap<String, i64>> {
    let rows = sqlx::query("SELECT id, name FROM notification_channels")
        .fetch_all(&mut **tx)
        .await
        .context("relecture des canaux")?;
    rows.iter().map(|row| Ok((row.try_get("name")?, row.try_get("id")?))).collect()
}

/// Normalise un identifiant du lot et vérifie qu'il a bien la forme attendue.
///
/// Stocker un JSON quelconque dans `credential_enc` ferait échouer toute
/// lecture de l'équipement plus tard, à un endroit qui n'expliquerait rien.
fn credential_bytes(value: &Value) -> Result<Vec<u8>> {
    let credential: dumbmonit_proto::Credential =
        serde_json::from_value(value.clone()).context("credential not understood")?;
    Ok(serde_json::to_vec(&credential)?)
}

async fn restore_targets(
    tx: &mut Transaction<'_, Sqlite>,
    cipher: &Cipher,
    bundle: &Bundle,
) -> Result<SectionReport> {
    let mut report = SectionReport::new("targets");
    // Le sort de chaque équipement, tenu de côté : le second passage peut
    // encore faire passer un équipement « inchangé » à « modifié » s'il lui
    // rattache un parent différent.
    let mut outcomes: HashMap<String, RestoreOutcome> = HashMap::new();

    // Premier passage : les équipements eux-mêmes, sans leurs liens. Un parent
    // peut apparaître après son enfant dans le lot.
    for target in &bundle.targets {
        match upsert_target(tx, cipher, target).await {
            Ok(outcome) => {
                outcomes.insert(target_ref(&target.kind, &target.address), outcome);
            }
            Err(error) => report.refused(&format!("Device \"{}\"", target.name), &error),
        }
    }

    // Second passage : parents et agents relais, maintenant que tout existe.
    let refs = target_ids(tx).await?;
    for target in &bundle.targets {
        let key = target_ref(&target.kind, &target.address);
        let Some(id) = refs.get(&key) else { continue };
        let parent = resolve(&mut report, &refs, target.parent.as_deref(), &target.name, "parent");
        let relay =
            resolve(&mut report, &refs, target.via_agent.as_deref(), &target.name, "relay agent");
        // Une cible ne peut pas être son propre parent : la chaîne de
        // suppression des alertes tournerait en rond.
        let parent = parent.filter(|p| p != id);
        let relay = relay.filter(|p| p != id);

        let current = sqlx::query("SELECT parent_id, via_agent FROM targets WHERE id = ?")
            .bind(id)
            .fetch_one(&mut **tx)
            .await
            .context("lecture des rattachements")?;
        if current.try_get::<Option<i64>, _>("parent_id")? == parent
            && current.try_get::<Option<i64>, _>("via_agent")? == relay
        {
            continue;
        }

        sqlx::query("UPDATE targets SET parent_id = ?, via_agent = ? WHERE id = ?")
            .bind(parent)
            .bind(relay)
            .bind(id)
            .execute(&mut **tx)
            .await
            .context("rattachement des équipements")?;
        if outcomes.get(&key) == Some(&RestoreOutcome::Skipped) {
            outcomes.insert(key, RestoreOutcome::Updated);
        }
    }

    for outcome in outcomes.into_values() {
        report.count(outcome);
    }
    Ok(report)
}

fn resolve(
    report: &mut SectionReport,
    refs: &HashMap<String, i64>,
    reference: Option<&str>,
    owner: &str,
    role: &str,
) -> Option<i64> {
    let reference = reference?;
    match refs.get(reference) {
        Some(id) => Some(*id),
        None => {
            report.notes.push(format!(
                "Device \"{owner}\": its {role} ({reference}) is not in this backup and does not \
                 exist here; the link was left empty."
            ));
            None
        }
    }
}

async fn upsert_target(
    tx: &mut Transaction<'_, Sqlite>,
    cipher: &Cipher,
    target: &BundleTarget,
) -> Result<RestoreOutcome> {
    let credential = match &target.credential {
        None => None,
        Some(value) => Some(credential_bytes(value)?),
    };
    let tags = serde_json::to_string(&target.tags)?;

    let existing = sqlx::query(
        "SELECT id, name, profile_id, interval_secs, enabled, tags, credential_enc
         FROM targets WHERE kind = ? AND address = ?",
    )
    .bind(&target.kind)
    .bind(&target.address)
    .fetch_optional(&mut **tx)
    .await
    .context("recherche de l'équipement")?;

    let Some(row) = existing else {
        sqlx::query(
            "INSERT INTO targets (name, address, kind, profile_id, interval_secs, enabled, tags,
                                  credential_enc)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&target.name)
        .bind(&target.address)
        .bind(&target.kind)
        .bind(&target.profile_id)
        .bind(target.interval_secs.max(10))
        .bind(i64::from(target.enabled))
        .bind(&tags)
        .bind(credential.map(|plain| cipher.encrypt(&plain)).transpose()?)
        .execute(&mut **tx)
        .await
        .context("création de l'équipement")?;
        return Ok(RestoreOutcome::Created);
    };

    let id: i64 = row.try_get("id")?;
    let stored_tags: String = row.try_get("tags")?;
    let stored_credential: Option<Vec<u8>> = row.try_get("credential_enc")?;
    let same = row.try_get::<String, _>("name")? == target.name
        && row.try_get::<Option<String>, _>("profile_id")? == target.profile_id
        && row.try_get::<i64, _>("interval_secs")? == target.interval_secs
        && (row.try_get::<i64, _>("enabled")? != 0) == target.enabled
        && serde_json::from_str::<Value>(&stored_tags).ok()
            == serde_json::from_str::<Value>(&tags).ok()
        && same_secret(cipher, stored_credential.as_deref(), target.credential.as_ref());

    if same {
        return Ok(RestoreOutcome::Skipped);
    }

    sqlx::query(
        "UPDATE targets SET name = ?, profile_id = ?, interval_secs = ?, enabled = ?, tags = ?,
             credential_enc = ?, updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
         WHERE id = ?",
    )
    .bind(&target.name)
    .bind(&target.profile_id)
    .bind(target.interval_secs.max(10))
    .bind(i64::from(target.enabled))
    .bind(&tags)
    .bind(credential.map(|plain| cipher.encrypt(&plain)).transpose()?)
    .bind(id)
    .execute(&mut **tx)
    .await
    .context("mise à jour de l'équipement")?;
    Ok(RestoreOutcome::Updated)
}

/// Compare un secret chiffré en base au clair du lot, sans jamais les afficher.
fn same_secret(cipher: &Cipher, stored: Option<&[u8]>, wanted: Option<&Value>) -> bool {
    let decrypted = stored
        .and_then(|bytes| cipher.decrypt(bytes).ok())
        .and_then(|plain| serde_json::from_slice::<Value>(&plain).ok());
    match (decrypted, wanted) {
        (None, None) => true,
        (Some(left), Some(right)) => &left == right,
        _ => false,
    }
}

// --------------------------------------------------------------------------
// Canaux
// --------------------------------------------------------------------------

async fn restore_channels(
    tx: &mut Transaction<'_, Sqlite>,
    cipher: &Cipher,
    bundle: &Bundle,
) -> Result<SectionReport> {
    let mut report = SectionReport::new("channels");
    for channel in &bundle.channels {
        match upsert_channel(tx, cipher, channel).await {
            Ok(outcome) => report.count(outcome),
            Err(error) => report.refused(&format!("Channel \"{}\"", channel.name), &error),
        }
    }
    Ok(report)
}

async fn upsert_channel(
    tx: &mut Transaction<'_, Sqlite>,
    cipher: &Cipher,
    channel: &BundleChannel,
) -> Result<RestoreOutcome> {
    let settings = serde_json::to_string(&channel.settings)?;
    let policy = serde_json::to_string(&channel.policy)?;
    let secrets = match &channel.secrets {
        None => None,
        Some(value) => Some(cipher.encrypt(serde_json::to_vec(value)?.as_slice())?),
    };

    let existing = sqlx::query(
        "SELECT id, kind, enabled, settings, secret_enc, policy
                     FROM notification_channels WHERE name = ?",
    )
    .bind(&channel.name)
    .fetch_optional(&mut **tx)
    .await
    .context("recherche du canal")?;

    let Some(row) = existing else {
        sqlx::query(
            "INSERT INTO notification_channels (name, kind, enabled, settings, secret_enc, policy)
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(&channel.name)
        .bind(&channel.kind)
        .bind(i64::from(channel.enabled))
        .bind(&settings)
        .bind(secrets)
        .bind(&policy)
        .execute(&mut **tx)
        .await
        .context("création du canal")?;
        return Ok(RestoreOutcome::Created);
    };

    let id: i64 = row.try_get("id")?;
    let stored_settings: String = row.try_get("settings")?;
    let stored_policy: String = row.try_get("policy")?;
    let stored_secret: Option<Vec<u8>> = row.try_get("secret_enc")?;
    let same = row.try_get::<String, _>("kind")? == channel.kind
        && (row.try_get::<i64, _>("enabled")? != 0) == channel.enabled
        && serde_json::from_str::<Value>(&stored_settings).ok() == Some(channel.settings.clone())
        && serde_json::from_str::<Value>(&stored_policy).ok() == Some(channel.policy.clone())
        && same_secret(cipher, stored_secret.as_deref(), channel.secrets.as_ref());

    if same {
        return Ok(RestoreOutcome::Skipped);
    }

    sqlx::query(
        "UPDATE notification_channels
         SET kind = ?, enabled = ?, settings = ?, secret_enc = COALESCE(?, secret_enc),
             policy = ?, updated_at = datetime('now')
         WHERE id = ?",
    )
    .bind(&channel.kind)
    .bind(i64::from(channel.enabled))
    .bind(&settings)
    .bind(secrets)
    .bind(&policy)
    .bind(id)
    .execute(&mut **tx)
    .await
    .context("mise à jour du canal")?;
    Ok(RestoreOutcome::Updated)
}

// --------------------------------------------------------------------------
// Règles et surcharges
// --------------------------------------------------------------------------

/// Rend au sélecteur ses identifiants locaux.
fn selector_from_bundle(
    report: &mut SectionReport,
    value: &Value,
    refs: &HashMap<String, i64>,
    rule: &str,
) -> String {
    if value.get("kind").and_then(Value::as_str) == Some("refs") {
        let listed = value.get("refs").and_then(Value::as_array).cloned().unwrap_or_default();
        let mut ids = Vec::new();
        for reference in listed.iter().filter_map(Value::as_str) {
            match refs.get(reference) {
                Some(id) => ids.push(*id),
                None => report.notes.push(format!(
                    "Rule \"{rule}\": it targeted {reference}, which is not in this backup; that \
                     device was dropped from its scope."
                )),
            }
        }
        // Un sélecteur devenu vide viserait plus rien : la règle serait muette
        // sans le dire. On la remet sur tous les équipements, ce que le compte
        // rendu signale.
        if ids.is_empty() {
            report.notes.push(format!(
                "Rule \"{rule}\": none of the devices it targeted exist here; it was restored as \
                 applying to every device. Narrow it again if that is not what you want."
            ));
            return r#"{"kind":"all"}"#.to_string();
        }
        return serde_json::json!({ "kind": "ids", "ids": ids }).to_string();
    }
    value.to_string()
}

async fn restore_rules(
    tx: &mut Transaction<'_, Sqlite>,
    bundle: &Bundle,
    refs: &HashMap<String, i64>,
    channels: &HashMap<String, i64>,
) -> Result<SectionReport> {
    let mut report = SectionReport::new("rules");
    for rule in &bundle.rules {
        let selector = selector_from_bundle(&mut report, &rule.selector, refs, &rule.name);
        let mut ids = Vec::new();
        for name in &rule.channels {
            match channels.get(name) {
                Some(id) => ids.push(*id),
                None => report.notes.push(format!(
                    "Rule \"{}\": the channel \"{name}\" it sends to is not in this backup; it \
                     was dropped from the rule.",
                    rule.name
                )),
            }
        }
        match upsert_rule(tx, rule, &selector, &ids).await {
            Ok(outcome) => report.count(outcome),
            Err(error) => report.refused(&format!("Rule \"{}\"", rule.name), &error),
        }
    }
    Ok(report)
}

async fn upsert_rule(
    tx: &mut Transaction<'_, Sqlite>,
    rule: &BundleRule,
    selector: &str,
    channels: &[i64],
) -> Result<RestoreOutcome> {
    let channel_json = serde_json::to_string(channels)?;
    let params = serde_json::to_string(&rule.params)?;

    let existing = sqlx::query(
        "SELECT id, name, description, kind, query, operator, threshold, clear_threshold,
                for_secs, severity, selector, channels, params, unit, repeat_secs,
                escalate_after_secs, enabled
         FROM alert_rules WHERE uid = ?",
    )
    .bind(&rule.uid)
    .fetch_optional(&mut **tx)
    .await
    .context("recherche de la règle")?;

    if let Some(row) = &existing {
        let same = row.try_get::<String, _>("name")? == rule.name
            && row.try_get::<String, _>("description")? == rule.description
            && row.try_get::<String, _>("kind")? == rule.kind
            && row.try_get::<String, _>("query")? == rule.query
            && row.try_get::<String, _>("operator")? == rule.operator
            && row.try_get::<f64, _>("threshold")? == rule.threshold
            && row.try_get::<Option<f64>, _>("clear_threshold")? == rule.clear_threshold
            && row.try_get::<i64, _>("for_secs")? == rule.for_secs
            && row.try_get::<String, _>("severity")? == rule.severity
            && row.try_get::<String, _>("selector")? == selector
            && row.try_get::<String, _>("channels")? == channel_json
            && row.try_get::<String, _>("unit")? == rule.unit
            && row.try_get::<Option<i64>, _>("repeat_secs")? == rule.repeat_secs
            && row.try_get::<Option<i64>, _>("escalate_after_secs")? == rule.escalate_after_secs
            && (row.try_get::<i64, _>("enabled")? != 0) == rule.enabled;
        if same {
            return Ok(RestoreOutcome::Skipped);
        }
    }

    sqlx::query(
        "INSERT INTO alert_rules
             (uid, name, description, kind, query, operator, threshold, clear_threshold,
              for_secs, severity, selector, channels, params, unit, repeat_secs,
              escalate_after_secs, enabled, builtin, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, datetime('now'))
         ON CONFLICT(uid) DO UPDATE SET
             name = excluded.name, description = excluded.description, kind = excluded.kind,
             query = excluded.query, operator = excluded.operator,
             threshold = excluded.threshold, clear_threshold = excluded.clear_threshold,
             for_secs = excluded.for_secs, severity = excluded.severity,
             selector = excluded.selector, channels = excluded.channels,
             params = excluded.params, unit = excluded.unit,
             repeat_secs = excluded.repeat_secs,
             escalate_after_secs = excluded.escalate_after_secs,
             enabled = excluded.enabled, updated_at = datetime('now')",
    )
    .bind(&rule.uid)
    .bind(&rule.name)
    .bind(&rule.description)
    .bind(&rule.kind)
    .bind(&rule.query)
    .bind(&rule.operator)
    .bind(rule.threshold)
    .bind(rule.clear_threshold)
    .bind(rule.for_secs)
    .bind(&rule.severity)
    .bind(selector)
    .bind(&channel_json)
    .bind(&params)
    .bind(&rule.unit)
    .bind(rule.repeat_secs)
    .bind(rule.escalate_after_secs)
    .bind(i64::from(rule.enabled))
    .bind(i64::from(rule.builtin))
    .execute(&mut **tx)
    .await
    .context("enregistrement de la règle")?;

    Ok(if existing.is_some() { RestoreOutcome::Updated } else { RestoreOutcome::Created })
}

async fn restore_overrides(
    tx: &mut Transaction<'_, Sqlite>,
    bundle: &Bundle,
    refs: &HashMap<String, i64>,
) -> Result<SectionReport> {
    let mut report = SectionReport::new("rule_overrides");
    for over in &bundle.rule_overrides {
        let Some(target_id) = refs.get(&over.target).copied() else {
            report.skipped += 1;
            report.notes.push(format!(
                "Per-device setting of rule \"{}\": its device ({}) is not in this backup.",
                over.rule_uid, over.target
            ));
            continue;
        };
        let existing = sqlx::query(
            "SELECT threshold, clear_threshold, enabled FROM rule_overrides
             WHERE rule_uid = ? AND target_id = ?",
        )
        .bind(&over.rule_uid)
        .bind(target_id)
        .fetch_optional(&mut **tx)
        .await
        .context("recherche de la surcharge")?;

        if let Some(row) = &existing {
            let enabled: Option<i64> = row.try_get("enabled")?;
            if row.try_get::<Option<f64>, _>("threshold")? == over.threshold
                && row.try_get::<Option<f64>, _>("clear_threshold")? == over.clear_threshold
                && enabled.map(|v| v != 0) == over.enabled
            {
                report.skipped += 1;
                continue;
            }
        }

        sqlx::query(
            "INSERT INTO rule_overrides (rule_uid, target_id, threshold, clear_threshold, enabled)
             VALUES (?, ?, ?, ?, ?)
             ON CONFLICT(rule_uid, target_id) DO UPDATE SET
                 threshold = excluded.threshold, clear_threshold = excluded.clear_threshold,
                 enabled = excluded.enabled, updated_at = datetime('now')",
        )
        .bind(&over.rule_uid)
        .bind(target_id)
        .bind(over.threshold)
        .bind(over.clear_threshold)
        .bind(over.enabled.map(i64::from))
        .execute(&mut **tx)
        .await
        .context("enregistrement de la surcharge")?;
        report.count(if existing.is_some() {
            RestoreOutcome::Updated
        } else {
            RestoreOutcome::Created
        });
    }
    Ok(report)
}

async fn restore_notify_policy(
    tx: &mut Transaction<'_, Sqlite>,
    bundle: &Bundle,
) -> Result<SectionReport> {
    let mut report = SectionReport::new("notify_policy");
    let Some(policy) = &bundle.notify_policy else { return Ok(report) };
    let value = serde_json::to_vec(policy)?;

    let existing = sqlx::query("SELECT value FROM settings WHERE key = ?")
        .bind(crate::notify::policy_store::POLICY_KEY)
        .fetch_optional(&mut **tx)
        .await
        .context("lecture de la politique de notification")?;
    if let Some(row) = &existing
        && row.try_get::<Vec<u8>, _>("value")? == value
    {
        report.skipped += 1;
        return Ok(report);
    }

    sqlx::query(
        "INSERT INTO settings (key, value) VALUES (?, ?)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value,
             updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')",
    )
    .bind(crate::notify::policy_store::POLICY_KEY)
    .bind(&value)
    .execute(&mut **tx)
    .await
    .context("écriture de la politique de notification")?;
    report.count(if existing.is_some() {
        RestoreOutcome::Updated
    } else {
        RestoreOutcome::Created
    });
    Ok(report)
}

// --------------------------------------------------------------------------
// Silences, pages de statut, incidents
// --------------------------------------------------------------------------

async fn restore_silences(
    tx: &mut Transaction<'_, Sqlite>,
    bundle: &Bundle,
    refs: &HashMap<String, i64>,
) -> Result<SectionReport> {
    let mut report = SectionReport::new("silences");
    for silence in &bundle.silences {
        let target_id = match &silence.target {
            None => None,
            Some(reference) => match refs.get(reference) {
                Some(id) => Some(*id),
                None => {
                    report.skipped += 1;
                    report.notes.push(format!(
                        "Maintenance window \"{}\": its device ({reference}) is not in this \
                         backup.",
                        silence.name
                    ));
                    continue;
                }
            },
        };
        let matchers = serde_json::to_string(&silence.matchers)?;
        let schedule = serde_json::to_string(&silence.schedule)?;

        let existing = sqlx::query(
            "SELECT id, comment, matchers, schedule, enabled FROM silences
             WHERE name = ? AND target_id IS ?",
        )
        .bind(&silence.name)
        .bind(target_id)
        .fetch_optional(&mut **tx)
        .await
        .context("recherche du silence")?;

        match existing {
            Some(row) => {
                let same = row.try_get::<String, _>("comment")? == silence.comment
                    && row.try_get::<String, _>("matchers")? == matchers
                    && row.try_get::<String, _>("schedule")? == schedule
                    && (row.try_get::<i64, _>("enabled")? != 0) == silence.enabled;
                if same {
                    report.skipped += 1;
                    continue;
                }
                sqlx::query(
                    "UPDATE silences SET comment = ?, matchers = ?, schedule = ?, enabled = ?
                     WHERE id = ?",
                )
                .bind(&silence.comment)
                .bind(&matchers)
                .bind(&schedule)
                .bind(i64::from(silence.enabled))
                .bind(row.try_get::<i64, _>("id")?)
                .execute(&mut **tx)
                .await
                .context("mise à jour du silence")?;
                report.updated += 1;
            }
            None => {
                sqlx::query(
                    "INSERT INTO silences (name, comment, target_id, matchers, schedule, enabled)
                     VALUES (?, ?, ?, ?, ?, ?)",
                )
                .bind(&silence.name)
                .bind(&silence.comment)
                .bind(target_id)
                .bind(&matchers)
                .bind(&schedule)
                .bind(i64::from(silence.enabled))
                .execute(&mut **tx)
                .await
                .context("création du silence")?;
                report.created += 1;
            }
        }
    }
    Ok(report)
}

async fn restore_status_pages(
    tx: &mut Transaction<'_, Sqlite>,
    bundle: &Bundle,
    refs: &HashMap<String, i64>,
) -> Result<SectionReport> {
    let mut report = SectionReport::new("status_pages");
    for page in &bundle.status_pages {
        let existing = sqlx::query(
            "SELECT id, title, description, published, theme, show_uptime_days
             FROM status_pages WHERE slug = ?",
        )
        .bind(&page.slug)
        .fetch_optional(&mut **tx)
        .await
        .context("recherche de la page de statut")?;

        let (id, mut outcome) = match existing {
            Some(row) => {
                let id: i64 = row.try_get("id")?;
                let same = row.try_get::<String, _>("title")? == page.title
                    && row.try_get::<String, _>("description")? == page.description
                    && (row.try_get::<i64, _>("published")? != 0) == page.published
                    && row.try_get::<String, _>("theme")? == page.theme
                    && row.try_get::<i64, _>("show_uptime_days")? == page.show_uptime_days;
                if !same {
                    sqlx::query(
                        "UPDATE status_pages SET title = ?, description = ?, published = ?,
                             theme = ?, show_uptime_days = ?,
                             updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
                         WHERE id = ?",
                    )
                    .bind(&page.title)
                    .bind(&page.description)
                    .bind(i64::from(page.published))
                    .bind(&page.theme)
                    .bind(page.show_uptime_days)
                    .bind(id)
                    .execute(&mut **tx)
                    .await
                    .context("mise à jour de la page de statut")?;
                }
                (id, if same { RestoreOutcome::Skipped } else { RestoreOutcome::Updated })
            }
            None => {
                let row = sqlx::query(
                    "INSERT INTO status_pages
                         (slug, title, description, published, theme, show_uptime_days)
                     VALUES (?, ?, ?, ?, ?, ?) RETURNING id",
                )
                .bind(&page.slug)
                .bind(&page.title)
                .bind(&page.description)
                .bind(i64::from(page.published))
                .bind(&page.theme)
                .bind(page.show_uptime_days)
                .fetch_one(&mut **tx)
                .await
                .context("création de la page de statut")?;
                (row.try_get("id")?, RestoreOutcome::Created)
            }
        };

        for item in &page.items {
            let Some(target_id) = refs.get(&item.target).copied() else {
                report.notes.push(format!(
                    "Status page \"{}\": the device {} it listed is not in this backup.",
                    page.slug, item.target
                ));
                continue;
            };
            let changed = sqlx::query(
                "INSERT INTO status_page_items (page_id, target_id, label, group_name, position)
                 VALUES (?, ?, ?, ?, ?)
                 ON CONFLICT(page_id, target_id) DO UPDATE SET
                     label = excluded.label, group_name = excluded.group_name,
                     position = excluded.position
                 WHERE label IS NOT excluded.label OR group_name IS NOT excluded.group_name
                    OR position IS NOT excluded.position",
            )
            .bind(id)
            .bind(target_id)
            .bind(&item.label)
            .bind(&item.group_name)
            .bind(item.position)
            .execute(&mut **tx)
            .await
            .context("enregistrement d'un service de page de statut")?;
            if changed.rows_affected() > 0 && outcome == RestoreOutcome::Skipped {
                outcome = RestoreOutcome::Updated;
            }
        }
        report.count(outcome);
    }
    Ok(report)
}

async fn restore_incidents(
    tx: &mut Transaction<'_, Sqlite>,
    bundle: &Bundle,
) -> Result<SectionReport> {
    let mut report = SectionReport::new("incidents");
    let pages: HashMap<String, i64> = sqlx::query("SELECT id, slug FROM status_pages")
        .fetch_all(&mut **tx)
        .await
        .context("relecture des pages de statut")?
        .iter()
        .map(|row| Ok((row.try_get("slug")?, row.try_get("id")?)))
        .collect::<Result<_>>()?;

    for incident in &bundle.incidents {
        let page_id = match &incident.page {
            None => None,
            Some(slug) => match pages.get(slug) {
                Some(id) => Some(*id),
                None => {
                    report.skipped += 1;
                    report.notes.push(format!(
                        "Incident \"{}\": its status page ({slug}) is not in this backup.",
                        incident.title
                    ));
                    continue;
                }
            },
        };

        // Un incident est un fait daté, pas un réglage : s'il est déjà là, on
        // le laisse tel quel plutôt que de réécrire un récit.
        let existing = sqlx::query(
            "SELECT id FROM incidents WHERE page_id IS ? AND title = ? AND starts_at = ?",
        )
        .bind(page_id)
        .bind(&incident.title)
        .bind(&incident.starts_at)
        .fetch_optional(&mut **tx)
        .await
        .context("recherche de l'incident")?;
        if existing.is_some() {
            report.skipped += 1;
            continue;
        }

        let row = sqlx::query(
            "INSERT INTO incidents (page_id, title, kind, status, severity, starts_at, ends_at)
             VALUES (?, ?, ?, ?, ?, ?, ?) RETURNING id",
        )
        .bind(page_id)
        .bind(&incident.title)
        .bind(&incident.kind)
        .bind(&incident.status)
        .bind(&incident.severity)
        .bind(&incident.starts_at)
        .bind(&incident.ends_at)
        .fetch_one(&mut **tx)
        .await
        .context("création de l'incident")?;
        let id: i64 = row.try_get("id")?;

        for update in &incident.updates {
            sqlx::query(
                "INSERT INTO incident_updates (incident_id, status, body, created_at)
                 VALUES (?, ?, ?, ?)",
            )
            .bind(id)
            .bind(&update.status)
            .bind(&update.body)
            .bind(&update.created_at)
            .execute(&mut **tx)
            .await
            .context("création d'une mise à jour d'incident")?;
        }
        report.created += 1;
    }
    Ok(report)
}

// --------------------------------------------------------------------------
// Comptes et jetons
// --------------------------------------------------------------------------

async fn restore_users(
    tx: &mut Transaction<'_, Sqlite>,
    cipher: &Cipher,
    bundle: &Bundle,
) -> Result<SectionReport> {
    let mut report = SectionReport::new("users");
    for user in &bundle.users {
        if crate::auth::users::Role::parse(&user.role).is_none() {
            report.refused(
                &format!("Account \"{}\"", user.username),
                &anyhow::anyhow!("unknown role \"{}\"", user.role),
            );
            continue;
        }

        let existing = sqlx::query("SELECT id FROM users WHERE username = ? COLLATE NOCASE")
            .bind(&user.username)
            .fetch_optional(&mut **tx)
            .await
            .context("recherche du compte")?;
        if existing.is_some() {
            // Jamais écrasé : un fichier ne doit pas pouvoir changer le rôle ou
            // le mot de passe d'un compte existant.
            report.skipped += 1;
            report.notes.push(format!(
                "Account \"{}\" already exists here and was left untouched.",
                user.username
            ));
            continue;
        }

        // `oidc_subject` est unique : s'il est déjà pris, le compte est créé
        // sans son rattachement SSO plutôt que refusé en entier.
        let mut subject = user.oidc_subject.clone();
        if let Some(value) = &subject {
            let taken = sqlx::query("SELECT 1 FROM users WHERE oidc_subject = ?")
                .bind(value)
                .fetch_optional(&mut **tx)
                .await
                .context("recherche du rattachement SSO")?;
            if taken.is_some() {
                report.notes.push(format!(
                    "Account \"{}\": its SSO identity already belongs to another account here; \
                     it was restored without the link.",
                    user.username
                ));
                subject = None;
            }
        }

        let totp_secret = user
            .totp_secret
            .as_ref()
            .and_then(|raw| BASE64.decode(raw).ok())
            .map(|plain| cipher.encrypt(&plain))
            .transpose()?;

        let row = sqlx::query(
            "INSERT INTO users (username, display_name, role, password_hash, oidc_subject,
                                oidc_issuer, disabled, totp_secret, totp_enabled)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?) RETURNING id",
        )
        .bind(&user.username)
        .bind(&user.display_name)
        .bind(&user.role)
        .bind(&user.password_hash)
        .bind(&subject)
        .bind(&user.oidc_issuer)
        .bind(i64::from(user.disabled))
        .bind(totp_secret.as_deref())
        .bind(i64::from(user.totp_enabled && totp_secret.is_some()))
        .fetch_one(&mut **tx)
        .await
        .context("création du compte")?;
        let id: i64 = row.try_get("id")?;

        for code in &user.recovery_codes {
            let Ok(hash) = BASE64.decode(code) else { continue };
            sqlx::query("INSERT INTO totp_recovery_codes (user_id, code_hash) VALUES (?, ?)")
                .bind(id)
                .bind(hash)
                .execute(&mut **tx)
                .await
                .context("création d'un code de secours")?;
        }
        report.created += 1;
    }
    Ok(report)
}

async fn restore_agent_tokens(
    tx: &mut Transaction<'_, Sqlite>,
    bundle: &Bundle,
) -> Result<SectionReport> {
    let mut report = SectionReport::new("agent_tokens");
    for token in &bundle.agent_tokens {
        let existing = sqlx::query("SELECT 1 FROM agent_tokens WHERE token_hash = ?")
            .bind(&token.token_hash)
            .fetch_optional(&mut **tx)
            .await
            .context("recherche du jeton d'agent")?;
        if existing.is_some() {
            report.skipped += 1;
            continue;
        }
        sqlx::query(
            "INSERT INTO agent_tokens (name, token_hash, prefix, created_at, revoked_at)
             VALUES (?, ?, ?, COALESCE(?, strftime('%Y-%m-%dT%H:%M:%SZ', 'now')), ?)",
        )
        .bind(&token.name)
        .bind(&token.token_hash)
        .bind(&token.prefix)
        .bind(&token.created_at)
        .bind(&token.revoked_at)
        .execute(&mut **tx)
        .await
        .context("création du jeton d'agent")?;
        report.created += 1;
    }
    Ok(report)
}

async fn restore_api_tokens(
    tx: &mut Transaction<'_, Sqlite>,
    bundle: &Bundle,
) -> Result<SectionReport> {
    let mut report = SectionReport::new("api_tokens");
    for token in &bundle.api_tokens {
        let Ok(hash) = BASE64.decode(&token.token_hash) else {
            report.refused(
                &format!("API token \"{}\"", token.name),
                &anyhow::anyhow!("its fingerprint is not valid base64"),
            );
            continue;
        };
        let existing = sqlx::query("SELECT 1 FROM api_tokens WHERE token_hash = ?")
            .bind(&hash)
            .fetch_optional(&mut **tx)
            .await
            .context("recherche du jeton d'API")?;
        if existing.is_some() {
            report.skipped += 1;
            continue;
        }
        let owner: Option<i64> = match &token.user {
            None => None,
            Some(username) => sqlx::query("SELECT id FROM users WHERE username = ? COLLATE NOCASE")
                .bind(username)
                .fetch_optional(&mut **tx)
                .await
                .context("recherche du propriétaire du jeton")?
                .map(|row| row.try_get("id"))
                .transpose()?,
        };
        match sqlx::query(
            "INSERT INTO api_tokens (name, prefix, token_hash, scope, created_at, revoked_at,
                                     user_id)
             VALUES (?, ?, ?, ?, COALESCE(?, strftime('%Y-%m-%dT%H:%M:%SZ', 'now')), ?, ?)",
        )
        .bind(&token.name)
        .bind(&token.prefix)
        .bind(&hash)
        .bind(&token.scope)
        .bind(&token.created_at)
        .bind(&token.revoked_at)
        .bind(owner)
        .execute(&mut **tx)
        .await
        {
            Ok(_) => report.created += 1,
            Err(error) => report.refused(&format!("API token \"{}\"", token.name), &error.into()),
        }
    }
    Ok(report)
}

async fn restore_push_monitors(
    tx: &mut Transaction<'_, Sqlite>,
    cipher: &Cipher,
    bundle: &Bundle,
    refs: &HashMap<String, i64>,
) -> Result<SectionReport> {
    let mut report = SectionReport::new("push_monitors");
    for monitor in &bundle.push_monitors {
        let Some(target_id) = refs.get(&monitor.target).copied() else {
            report.skipped += 1;
            report.notes.push(format!(
                "Heartbeat token: its device ({}) is not in this backup.",
                monitor.target
            ));
            continue;
        };
        let fingerprint = crate::collectors::push::token::fingerprint(&monitor.token);
        let existing = sqlx::query("SELECT token_hash FROM push_monitors WHERE target_id = ?")
            .bind(target_id)
            .fetch_optional(&mut **tx)
            .await
            .context("recherche du moniteur en poussée")?;

        if let Some(row) = &existing
            && row.try_get::<String, _>("token_hash")? == fingerprint
        {
            report.skipped += 1;
            continue;
        }

        let encrypted = cipher.encrypt(monitor.token.as_bytes())?;
        let outcome = match &existing {
            Some(_) => sqlx::query(
                "UPDATE push_monitors SET token_hash = ?, token_enc = ? WHERE target_id = ?",
            )
            .bind(&fingerprint)
            .bind(&encrypted)
            .bind(target_id)
            .execute(&mut **tx)
            .await
            .map(|_| RestoreOutcome::Updated),
            None => sqlx::query(
                "INSERT INTO push_monitors (target_id, token_hash, token_enc) VALUES (?, ?, ?)",
            )
            .bind(target_id)
            .bind(&fingerprint)
            .bind(&encrypted)
            .execute(&mut **tx)
            .await
            .map(|_| RestoreOutcome::Created),
        };
        match outcome {
            Ok(outcome) => report.count(outcome),
            Err(error) => {
                report.refused(&format!("Heartbeat token of {}", monitor.target), &error.into())
            }
        }
    }
    Ok(report)
}
