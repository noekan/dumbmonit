//! Comptes utilisateurs : lecture et écriture de la table `users`.
//!
//! Deux rôles, pas un de plus : `admin` fait tout, `viewer` regarde. Il n'y a ni
//! permission fine, ni groupe local — quand une équipe a besoin de plus, c'est le
//! fournisseur d'identité qui décide, par ses groupes (voir `oidc`).

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sqlx::sqlite::SqliteRow;
use sqlx::{Row, SqlitePool};

/// Rôle d'un compte. Sérialisé en minuscules, tel qu'il est stocké et exposé.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Admin,
    Viewer,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Admin => "admin",
            Self::Viewer => "viewer",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "admin" => Some(Self::Admin),
            "viewer" => Some(Self::Viewer),
            _ => None,
        }
    }

    pub fn is_admin(self) -> bool {
        matches!(self, Self::Admin)
    }
}

/// Un compte, tel qu'il est lu en base. L'empreinte du mot de passe n'est jamais
/// exposée au-delà de ce module et de la vérification de connexion.
#[derive(Debug, Clone)]
pub struct User {
    pub id: i64,
    pub username: String,
    pub display_name: String,
    pub role: Role,
    pub password_hash: Option<String>,
    pub oidc_subject: Option<String>,
    pub oidc_issuer: Option<String>,
    pub created_at: String,
    pub last_login_at: Option<String>,
    pub disabled: bool,
    /// Secret TOTP chiffré, `None` sans enrôlement (voir [`crate::auth::totp`]).
    pub totp_secret: Option<Vec<u8>>,
    /// Second facteur exigé à la connexion : l'enrôlement a été confirmé.
    pub totp_enabled: bool,
}

impl User {
    /// Nom à afficher : le nom choisi, sinon l'identifiant.
    pub fn label(&self) -> &str {
        if self.display_name.trim().is_empty() { &self.username } else { &self.display_name }
    }

    /// Méthode de connexion annoncée à l'interface : « password » dès qu'un mot
    /// de passe existe, « oidc » sinon.
    pub fn auth_method(&self) -> &'static str {
        if self.password_hash.is_some() { "password" } else { "oidc" }
    }
}

fn read(row: SqliteRow) -> Result<User> {
    let role: String = row.try_get("role")?;
    Ok(User {
        id: row.try_get("id")?,
        username: row.try_get("username")?,
        display_name: row.try_get("display_name")?,
        role: Role::parse(&role).with_context(|| format!("rôle inconnu en base : {role}"))?,
        password_hash: row.try_get("password_hash")?,
        oidc_subject: row.try_get("oidc_subject")?,
        oidc_issuer: row.try_get("oidc_issuer")?,
        created_at: row.try_get("created_at")?,
        last_login_at: row.try_get("last_login_at")?,
        disabled: row.try_get::<i64, _>("disabled")? != 0,
        totp_secret: row.try_get("totp_secret")?,
        totp_enabled: row.try_get::<i64, _>("totp_enabled")? != 0,
    })
}

pub async fn count(pool: &SqlitePool) -> Result<i64> {
    let row = sqlx::query("SELECT COUNT(*) AS n FROM users")
        .fetch_one(pool)
        .await
        .context("comptage des comptes")?;
    Ok(row.try_get("n")?)
}

pub async fn list(pool: &SqlitePool) -> Result<Vec<User>> {
    let rows = sqlx::query(
        "SELECT id, username, display_name, role, password_hash, oidc_subject, oidc_issuer,
                created_at, last_login_at, disabled, totp_secret, totp_enabled
         FROM users ORDER BY username COLLATE NOCASE",
    )
    .fetch_all(pool)
    .await
    .context("liste des comptes")?;
    rows.into_iter().map(read).collect()
}

pub async fn get(pool: &SqlitePool, id: i64) -> Result<Option<User>> {
    let row = sqlx::query(
        "SELECT id, username, display_name, role, password_hash, oidc_subject, oidc_issuer,
                created_at, last_login_at, disabled, totp_secret, totp_enabled
         FROM users WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
    .context("lecture du compte")?;
    row.map(read).transpose()
}

pub async fn by_username(pool: &SqlitePool, username: &str) -> Result<Option<User>> {
    let row = sqlx::query(
        "SELECT id, username, display_name, role, password_hash, oidc_subject, oidc_issuer,
                created_at, last_login_at, disabled, totp_secret, totp_enabled
         FROM users WHERE username = ?",
    )
    .bind(username)
    .fetch_optional(pool)
    .await
    .context("recherche du compte par identifiant")?;
    row.map(read).transpose()
}

pub async fn by_oidc(pool: &SqlitePool, issuer: &str, subject: &str) -> Result<Option<User>> {
    let row = sqlx::query(
        "SELECT id, username, display_name, role, password_hash, oidc_subject, oidc_issuer,
                created_at, last_login_at, disabled, totp_secret, totp_enabled
         FROM users WHERE oidc_issuer = ? AND oidc_subject = ?",
    )
    .bind(issuer)
    .bind(subject)
    .fetch_optional(pool)
    .await
    .context("recherche du compte par identité OIDC")?;
    row.map(read).transpose()
}

/// Le seul compte local, s'il n'y en a qu'un : c'est ce qui permet à l'ancien
/// formulaire de connexion (mot de passe seul) de continuer à fonctionner.
pub async fn sole_password_user(pool: &SqlitePool) -> Result<Option<User>> {
    let rows = sqlx::query(
        "SELECT id, username, display_name, role, password_hash, oidc_subject, oidc_issuer,
                created_at, last_login_at, disabled, totp_secret, totp_enabled
         FROM users WHERE password_hash IS NOT NULL LIMIT 2",
    )
    .fetch_all(pool)
    .await
    .context("recherche du compte local unique")?;
    if rows.len() != 1 {
        return Ok(None);
    }
    rows.into_iter().next().map(read).transpose()
}

/// Ce qu'il faut pour créer un compte.
pub struct NewUser<'a> {
    pub username: &'a str,
    pub display_name: &'a str,
    pub role: Role,
    pub password_hash: Option<&'a str>,
    pub oidc: Option<(&'a str, &'a str)>,
}

/// Crée un compte et renvoie son identifiant, ou `None` si l'identifiant est
/// déjà pris. Le conflit est laissé à la contrainte d'unicité : c'est la seule
/// façon de fermer la fenêtre entre « vérifier » et « écrire ».
pub async fn insert(pool: &SqlitePool, user: NewUser<'_>) -> Result<Option<i64>> {
    let (issuer, subject) = match user.oidc {
        Some((issuer, subject)) => (Some(issuer), Some(subject)),
        None => (None, None),
    };
    let result = sqlx::query(
        "INSERT INTO users (username, display_name, role, password_hash, oidc_issuer, oidc_subject)
         VALUES (?, ?, ?, ?, ?, ?)
         ON CONFLICT(username) DO NOTHING",
    )
    .bind(user.username)
    .bind(user.display_name)
    .bind(user.role.as_str())
    .bind(user.password_hash)
    .bind(issuer)
    .bind(subject)
    .execute(pool)
    .await
    .context("création du compte")?;

    Ok((result.rows_affected() == 1).then(|| result.last_insert_rowid()))
}

/// Crée le tout premier compte, seulement si la table est vide.
///
/// La condition est dans la requête même : deux premières configurations
/// simultanées ne peuvent pas créer deux administrateurs.
pub async fn insert_first_admin(pool: &SqlitePool, username: &str, hash: &str) -> Result<bool> {
    let result = sqlx::query(
        "INSERT INTO users (username, display_name, role, password_hash)
         SELECT ?, '', 'admin', ?
         WHERE NOT EXISTS (SELECT 1 FROM users)",
    )
    .bind(username)
    .bind(hash)
    .execute(pool)
    .await
    .context("création du premier administrateur")?;
    Ok(result.rows_affected() == 1)
}

pub async fn set_password_hash(pool: &SqlitePool, id: i64, hash: &str) -> Result<()> {
    sqlx::query("UPDATE users SET password_hash = ? WHERE id = ?")
        .bind(hash)
        .bind(id)
        .execute(pool)
        .await
        .context("mise à jour du mot de passe")?;
    Ok(())
}

pub async fn set_display_name(pool: &SqlitePool, id: i64, display_name: &str) -> Result<()> {
    sqlx::query("UPDATE users SET display_name = ? WHERE id = ?")
        .bind(display_name)
        .bind(id)
        .execute(pool)
        .await
        .context("mise à jour du nom affiché")?;
    Ok(())
}

pub async fn set_role(pool: &SqlitePool, id: i64, role: Role) -> Result<()> {
    sqlx::query("UPDATE users SET role = ? WHERE id = ?")
        .bind(role.as_str())
        .bind(id)
        .execute(pool)
        .await
        .context("mise à jour du rôle")?;
    Ok(())
}

pub async fn set_disabled(pool: &SqlitePool, id: i64, disabled: bool) -> Result<()> {
    sqlx::query("UPDATE users SET disabled = ? WHERE id = ?")
        .bind(i64::from(disabled))
        .bind(id)
        .execute(pool)
        .await
        .context("activation ou désactivation du compte")?;
    Ok(())
}

/// Rattache un compte existant à son identité chez le fournisseur.
pub async fn link_oidc(pool: &SqlitePool, id: i64, issuer: &str, subject: &str) -> Result<()> {
    sqlx::query("UPDATE users SET oidc_issuer = ?, oidc_subject = ? WHERE id = ?")
        .bind(issuer)
        .bind(subject)
        .bind(id)
        .execute(pool)
        .await
        .context("rattachement de l'identité OIDC")?;
    Ok(())
}

/// Dépose un secret TOTP chiffré, en attente de confirmation : le second
/// facteur n'est pas encore exigé.
pub async fn set_totp_pending(pool: &SqlitePool, id: i64, secret: &[u8]) -> Result<()> {
    sqlx::query("UPDATE users SET totp_secret = ?, totp_enabled = 0 WHERE id = ?")
        .bind(secret)
        .bind(id)
        .execute(pool)
        .await
        .context("enrôlement du second facteur")?;
    Ok(())
}

/// Confirme l'enrôlement : le second facteur est désormais exigé.
pub async fn set_totp_enabled(pool: &SqlitePool, id: i64) -> Result<()> {
    sqlx::query("UPDATE users SET totp_enabled = 1 WHERE id = ? AND totp_secret IS NOT NULL")
        .bind(id)
        .execute(pool)
        .await
        .context("activation du second facteur")?;
    Ok(())
}

/// Retire le second facteur, secret et codes de secours compris.
pub async fn clear_totp(pool: &SqlitePool, id: i64) -> Result<()> {
    let mut tx = pool.begin().await.context("ouverture de la transaction")?;
    sqlx::query("UPDATE users SET totp_secret = NULL, totp_enabled = 0 WHERE id = ?")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM totp_recovery_codes WHERE user_id = ?")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await.context("désactivation du second facteur")?;
    Ok(())
}

/// Remplace les codes de secours d'un compte par un nouveau jeu d'empreintes.
pub async fn replace_recovery_codes(pool: &SqlitePool, id: i64, hashes: &[Vec<u8>]) -> Result<()> {
    let mut tx = pool.begin().await.context("ouverture de la transaction")?;
    sqlx::query("DELETE FROM totp_recovery_codes WHERE user_id = ?")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    for hash in hashes {
        sqlx::query("INSERT INTO totp_recovery_codes (user_id, code_hash) VALUES (?, ?)")
            .bind(id)
            .bind(hash)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await.context("enregistrement des codes de secours")?;
    Ok(())
}

/// Consomme un code de secours s'il est inutilisé : `true` quand il a servi.
///
/// La condition est dans la requête : deux connexions simultanées avec le même
/// code ne peuvent pas passer toutes les deux.
pub async fn consume_recovery_code(pool: &SqlitePool, id: i64, hash: &[u8]) -> Result<bool> {
    let result = sqlx::query(
        "UPDATE totp_recovery_codes
         SET used_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
         WHERE id = (
             SELECT id FROM totp_recovery_codes
             WHERE user_id = ? AND code_hash = ? AND used_at IS NULL
             LIMIT 1
         )",
    )
    .bind(id)
    .bind(hash)
    .execute(pool)
    .await
    .context("consommation d'un code de secours")?;
    Ok(result.rows_affected() == 1)
}

/// Nombre de codes de secours encore utilisables.
pub async fn recovery_codes_left(pool: &SqlitePool, id: i64) -> Result<i64> {
    let row = sqlx::query(
        "SELECT COUNT(*) AS n FROM totp_recovery_codes WHERE user_id = ? AND used_at IS NULL",
    )
    .bind(id)
    .fetch_one(pool)
    .await
    .context("comptage des codes de secours")?;
    Ok(row.try_get("n")?)
}

pub async fn touch_login(pool: &SqlitePool, id: i64) -> Result<()> {
    sqlx::query(
        "UPDATE users SET last_login_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now') WHERE id = ?",
    )
    .bind(id)
    .execute(pool)
    .await
    .context("horodatage de la connexion")?;
    Ok(())
}

pub async fn delete(pool: &SqlitePool, id: i64) -> Result<()> {
    // Les sessions suivent par `ON DELETE CASCADE`.
    sqlx::query("DELETE FROM users WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await
        .context("suppression du compte")?;
    Ok(())
}

/// Nombre d'administrateurs actifs : c'est lui que protège la règle du « dernier
/// administrateur » — une instance sans admin ne s'administre plus.
pub async fn active_admin_count(pool: &SqlitePool) -> Result<i64> {
    let row = sqlx::query("SELECT COUNT(*) AS n FROM users WHERE role = 'admin' AND disabled = 0")
        .fetch_one(pool)
        .await
        .context("comptage des administrateurs")?;
    Ok(row.try_get("n")?)
}

/// Efface tous les comptes et toutes les sessions.
///
/// Réservé au démarrage, sur demande explicite de l'environnement : c'est l'issue
/// de secours d'une instance dont plus personne n'a le mot de passe, et elle exige
/// d'avoir la main sur le conteneur — ce qui vaut bien un mot de passe. Les
/// réglages OIDC, eux, survivent.
pub async fn reset_all(pool: &SqlitePool) -> Result<()> {
    let mut tx = pool.begin().await.context("ouverture de la transaction")?;
    sqlx::query("DELETE FROM auth_sessions").execute(&mut *tx).await?;
    sqlx::query("DELETE FROM users").execute(&mut *tx).await?;
    tx.commit().await.context("réinitialisation des comptes")?;
    Ok(())
}

/// Vérifie la forme d'un identifiant : court, sans espace, en ASCII imprimable.
///
/// La règle est volontairement souple — un courriel passe — parce qu'un
/// fournisseur OIDC peut envoyer l'un ou l'autre et qu'on voudra les faire
/// correspondre.
pub fn validate_username(username: &str) -> Result<(), String> {
    let trimmed = username.trim();
    if trimmed.is_empty() {
        return Err("Enter a username.".into());
    }
    if trimmed.chars().count() > 64 {
        return Err("The username must be 64 characters or fewer.".into());
    }
    if trimmed.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err("The username cannot contain spaces.".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roles_round_trip_through_their_text_form() {
        for role in [Role::Admin, Role::Viewer] {
            assert_eq!(Role::parse(role.as_str()), Some(role));
        }
        assert_eq!(Role::parse("root"), None);
        assert!(Role::Admin.is_admin());
        assert!(!Role::Viewer.is_admin());
    }

    #[test]
    fn usernames_are_short_and_without_spaces() {
        assert!(validate_username("admin").is_ok());
        assert!(validate_username("jane.doe@example.org").is_ok());
        assert!(validate_username("").is_err());
        assert!(validate_username("jane doe").is_err());
        assert!(validate_username(&"a".repeat(65)).is_err());
    }
}
