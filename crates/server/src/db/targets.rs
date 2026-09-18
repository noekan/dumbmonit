//! Accès aux cibles. C'est ici — et nulle part ailleurs — que les identifiants sont
//! chiffrés et déchiffrés, pour qu'aucun autre module n'ait à y penser.

use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

use anyhow::{Context, Result};
use dumbmonit_proto::{Credential, Target, TargetId};
use sqlx::{Row, SqlitePool, sqlite::SqliteRow};

use crate::crypto::Cipher;

/// Champs modifiables d'une cible, à la création comme à la mise à jour.
#[derive(Debug, Clone)]
pub struct TargetInput {
    pub name: String,
    pub address: String,
    pub kind: String,
    pub profile_id: Option<String>,
    pub parent_id: Option<TargetId>,
    /// Agent relais qui interroge cette cible à la place du serveur. `None` :
    /// le serveur s'en charge lui-même, comme pour toute cible ordinaire.
    pub via_agent: Option<TargetId>,
    pub interval: Duration,
    pub enabled: bool,
    pub tags: BTreeMap<String, String>,
    /// `None` signifie « conserver le secret déjà enregistré ».
    ///
    /// C'est ce qui permet de renommer une cible ou d'ajuster sa période sans
    /// ressaisir sa community — une modification bien plus fréquente que le
    /// changement d'identifiants. Pour effacer réellement un secret, on passe
    /// `Some(Credential::None)`.
    pub credential: Option<Credential>,
}

pub async fn list(pool: &SqlitePool, cipher: &Cipher) -> Result<Vec<Target>> {
    let rows = sqlx::query(
        "SELECT id, name, address, kind, profile_id, parent_id, interval_secs, enabled,
                tags, credential_enc
         FROM targets ORDER BY name",
    )
    .fetch_all(pool)
    .await
    .context("liste des cibles")?;
    rows.iter().map(|row| row_to_target(row, cipher)).collect()
}

/// Cibles actives, telles que consommées par le planificateur.
pub async fn list_enabled(pool: &SqlitePool, cipher: &Cipher) -> Result<Vec<Target>> {
    let rows = sqlx::query(
        "SELECT id, name, address, kind, profile_id, parent_id, interval_secs, enabled,
                tags, credential_enc
         FROM targets WHERE enabled = 1",
    )
    .fetch_all(pool)
    .await
    .context("liste des cibles actives")?;
    rows.iter().map(|row| row_to_target(row, cipher)).collect()
}

pub async fn get(pool: &SqlitePool, cipher: &Cipher, id: TargetId) -> Result<Option<Target>> {
    let row = sqlx::query(
        "SELECT id, name, address, kind, profile_id, parent_id, interval_secs, enabled,
                tags, credential_enc
         FROM targets WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
    .context("lecture d'une cible")?;
    row.as_ref().map(|row| row_to_target(row, cipher)).transpose()
}

pub async fn create(pool: &SqlitePool, cipher: &Cipher, input: &TargetInput) -> Result<TargetId> {
    let row = sqlx::query(
        "INSERT INTO targets
             (name, address, kind, profile_id, parent_id, via_agent, interval_secs, enabled, tags,
              credential_enc)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
         RETURNING id",
    )
    .bind(&input.name)
    .bind(&input.address)
    .bind(&input.kind)
    .bind(&input.profile_id)
    .bind(input.parent_id)
    .bind(input.via_agent)
    .bind(input.interval.as_secs() as i64)
    .bind(i64::from(input.enabled))
    .bind(serde_json::to_string(&input.tags)?)
    .bind(encrypt_credential(cipher, input.credential.as_ref().unwrap_or(&Credential::None))?)
    .fetch_one(pool)
    .await
    .context("création de la cible")?;

    row.try_get("id").context("identifiant de la cible créée")
}

pub async fn update(
    pool: &SqlitePool,
    cipher: &Cipher,
    id: TargetId,
    input: &TargetInput,
) -> Result<bool> {
    // Deux requêtes distinctes plutôt qu'une construite dynamiquement : sqlx refuse
    // le SQL assemblé à la volée, et la colonne du secret doit rester intacte quand
    // l'appelant n'en fournit pas de nouveau.
    let result = match &input.credential {
        Some(credential) => {
            sqlx::query(
                "UPDATE targets SET
                     name = ?, address = ?, kind = ?, profile_id = ?, parent_id = ?,
                     via_agent = ?, interval_secs = ?, enabled = ?, tags = ?, credential_enc = ?,
                     updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
                 WHERE id = ?",
            )
            .bind(&input.name)
            .bind(&input.address)
            .bind(&input.kind)
            .bind(&input.profile_id)
            .bind(input.parent_id)
            .bind(input.via_agent)
            .bind(input.interval.as_secs() as i64)
            .bind(i64::from(input.enabled))
            .bind(serde_json::to_string(&input.tags)?)
            .bind(encrypt_credential(cipher, credential)?)
            .bind(id)
            .execute(pool)
            .await
        }
        None => {
            sqlx::query(
                "UPDATE targets SET
                     name = ?, address = ?, kind = ?, profile_id = ?, parent_id = ?,
                     via_agent = ?, interval_secs = ?, enabled = ?, tags = ?,
                     updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
                 WHERE id = ?",
            )
            .bind(&input.name)
            .bind(&input.address)
            .bind(&input.kind)
            .bind(&input.profile_id)
            .bind(input.parent_id)
            .bind(input.via_agent)
            .bind(input.interval.as_secs() as i64)
            .bind(i64::from(input.enabled))
            .bind(serde_json::to_string(&input.tags)?)
            .bind(id)
            .execute(pool)
            .await
        }
    }
    .context("mise à jour de la cible")?;

    Ok(result.rows_affected() > 0)
}

pub async fn delete(pool: &SqlitePool, id: TargetId) -> Result<bool> {
    let result = sqlx::query("DELETE FROM targets WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await
        .context("suppression de la cible")?;
    Ok(result.rows_affected() > 0)
}

/// Enregistre l'issue d'une interrogation. `error` à `None` signifie succès.
pub async fn record_probe(pool: &SqlitePool, id: TargetId, error: Option<&str>) -> Result<()> {
    sqlx::query(
        "UPDATE targets SET last_probe_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now'), last_error = ? WHERE id = ?",
    )
    .bind(error)
    .bind(id)
    .execute(pool)
    .await
    .context("enregistrement du résultat d'interrogation")?;
    Ok(())
}

/// Active ou désactive une cible sans toucher au reste de sa définition.
///
/// Le point d'entrée « modifier » exige la cible complète ; ici l'appelant (le
/// serveur MCP) ne connaît que l'identifiant et le drapeau, et ne doit surtout pas
/// réécrire le reste.
pub async fn set_enabled(pool: &SqlitePool, id: TargetId, enabled: bool) -> Result<bool> {
    let result = sqlx::query(
        "UPDATE targets SET enabled = ?, updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
         WHERE id = ?",
    )
    .bind(i64::from(enabled))
    .bind(id)
    .execute(pool)
    .await
    .context("activation de la cible")?;
    Ok(result.rows_affected() > 0)
}

/// Associe un profil détecté à une cible.
pub async fn set_profile(pool: &SqlitePool, id: TargetId, profile_id: &str) -> Result<()> {
    sqlx::query("UPDATE targets SET profile_id = ?, updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now') WHERE id = ?")
        .bind(profile_id)
        .bind(id)
        .execute(pool)
        .await
        .context("association du profil")?;
    Ok(())
}

fn row_to_target(row: &SqliteRow, cipher: &Cipher) -> Result<Target> {
    let id: TargetId = row.try_get("id")?;
    let tags_json: String = row.try_get("tags")?;
    let credential_enc: Option<Vec<u8>> = row.try_get("credential_enc")?;

    Ok(Target {
        id,
        name: row.try_get("name")?,
        address: row.try_get("address")?,
        kind: row.try_get("kind")?,
        profile_id: row.try_get("profile_id")?,
        parent_id: row.try_get("parent_id")?,
        interval: Duration::from_secs(row.try_get::<i64, _>("interval_secs")?.max(1) as u64),
        enabled: row.try_get::<i64, _>("enabled")? != 0,
        tags: serde_json::from_str(&tags_json)
            .with_context(|| format!("étiquettes illisibles pour la cible {id}"))?,
        credential: decrypt_credential(cipher, credential_enc.as_deref())
            .with_context(|| format!("identifiants illisibles pour la cible {id}"))?,
    })
}

fn encrypt_credential(cipher: &Cipher, credential: &Credential) -> Result<Option<Vec<u8>>> {
    if matches!(credential, Credential::None) {
        return Ok(None);
    }
    let json = serde_json::to_vec(credential)?;
    Ok(Some(cipher.encrypt(&json)?))
}

fn decrypt_credential(cipher: &Cipher, data: Option<&[u8]>) -> Result<Credential> {
    match data {
        None => Ok(Credential::None),
        Some(bytes) => Ok(serde_json::from_slice(&cipher.decrypt(bytes)?)?),
    }
}

/// Relais des cibles : identifiant de la cible → identifiant de l'agent qui
/// l'interroge. Les cibles interrogées par le serveur n'y figurent pas.
///
/// Tenu à part de [`Target`] : le relais est une affaire d'acheminement propre au
/// serveur, et les collecteurs — qu'ils tournent ici ou sur l'agent — n'ont pas à
/// le connaître.
pub async fn relay_map(pool: &SqlitePool) -> Result<HashMap<TargetId, TargetId>> {
    let rows = sqlx::query("SELECT id, via_agent FROM targets WHERE via_agent IS NOT NULL")
        .fetch_all(pool)
        .await
        .context("lecture des relais")?;
    rows.into_iter().map(|row| Ok((row.try_get("id")?, row.try_get("via_agent")?))).collect()
}

/// Agent relais d'une cible, s'il y en a un.
pub async fn relay_of(pool: &SqlitePool, id: TargetId) -> Result<Option<TargetId>> {
    let row = sqlx::query("SELECT via_agent FROM targets WHERE id = ?")
        .bind(id)
        .fetch_optional(pool)
        .await
        .context("lecture du relais")?;
    Ok(row.and_then(|row| row.try_get::<Option<TargetId>, _>("via_agent").ok().flatten()))
}

/// Nombre de cibles relayées par chaque agent.
pub async fn relayed_counts(pool: &SqlitePool) -> Result<HashMap<TargetId, usize>> {
    let rows = sqlx::query(
        "SELECT via_agent, COUNT(*) AS n FROM targets WHERE via_agent IS NOT NULL GROUP BY via_agent",
    )
    .fetch_all(pool)
    .await
    .context("décompte des cibles relayées")?;
    rows.into_iter()
        .map(|row| Ok((row.try_get("via_agent")?, row.try_get::<i64, _>("n")?.max(0) as usize)))
        .collect()
}

/// Issue de la dernière interrogation d'une cible, pour l'affichage.
#[derive(Debug, Clone)]
pub struct TargetStatus {
    pub last_probe_at: Option<String>,
    pub last_error: Option<String>,
    /// Agent relais, recopié ici pour que l'API l'expose sans relire la cible.
    pub via_agent: Option<TargetId>,
}

/// Statut de toutes les cibles, indexé par identifiant.
pub async fn statuses(pool: &SqlitePool) -> Result<HashMap<TargetId, TargetStatus>> {
    let rows = sqlx::query("SELECT id, last_probe_at, last_error, via_agent FROM targets")
        .fetch_all(pool)
        .await
        .context("lecture des statuts")?;

    rows.into_iter()
        .map(|row| {
            Ok((
                row.try_get("id")?,
                TargetStatus {
                    last_probe_at: row.try_get("last_probe_at")?,
                    last_error: row.try_get("last_error")?,
                    via_agent: row.try_get("via_agent")?,
                },
            ))
        })
        .collect()
}
