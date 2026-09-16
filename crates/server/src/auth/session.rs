//! Sessions opaques.
//!
//! Le cookie ne porte aucune information : ni identité, ni date, ni signature —
//! seulement `<identifiant>.<jeton>`. Tout le reste vit en base, ce qui permet de
//! révoquer une session immédiatement, chose qu'un jeton auto-porté (JWT) ne sait
//! pas faire sans réintroduire… une table de sessions.
//!
//! La partie secrète n'est jamais stockée : la base ne contient que son empreinte
//! SHA-256. Une copie de `dumbmonit.db` ne donne donc aucune session utilisable.

use anyhow::{Context, Result};
use chrono::{DateTime, Duration, SecondsFormat, Utc};
use sha2::{Digest, Sha256};
use sqlx::{Row, SqlitePool};
use subtle::ConstantTimeEq;

/// Durée de vie d'une session. Trente jours : assez long pour ne pas redemander le
/// mot de passe à chaque visite d'un tableau de bord qu'on garde ouvert, assez
/// court pour qu'un poste oublié finisse par se refermer.
const LIFETIME_DAYS: i64 = 30;

/// Octets d'entropie de la partie secrète du cookie. À 32 octets, la deviner est
/// hors de portée, et la limitation des tentatives ne sert que pour le mot de passe.
const TOKEN_BYTES: usize = 32;
/// Octets de l'identifiant public. Il n'a pas à être secret, seulement unique.
const ID_BYTES: usize = 16;

/// Jeton de session, tel qu'il transite dans le cookie.
///
/// `Debug` est écrit à la main : ce type finit dans des messages de trace, et une
/// dérivation laisserait fuiter des sessions valides dans les journaux.
#[derive(Clone)]
pub struct SessionToken {
    /// Partie publique : sert à retrouver la ligne en base.
    id: String,
    /// Partie secrète : sert à prouver la session, jamais stockée en clair.
    secret: String,
}

impl std::fmt::Debug for SessionToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SessionToken {{ id: {:?}, secret: <redacted> }}", self.id)
    }
}

impl SessionToken {
    fn generate() -> Self {
        Self {
            id: hex::encode(rand::random::<[u8; ID_BYTES]>()),
            secret: hex::encode(rand::random::<[u8; TOKEN_BYTES]>()),
        }
    }

    /// Relit la valeur d'un cookie. Rend `None` sur toute forme inattendue plutôt
    /// que de propager une erreur : un cookie mal formé n'est pas un incident, il
    /// signifie simplement « pas authentifié ».
    pub fn parse(value: &str) -> Option<Self> {
        let (id, secret) = value.split_once('.')?;
        if id.is_empty() || secret.is_empty() {
            return None;
        }
        Some(Self { id: id.to_string(), secret: secret.to_string() })
    }

    /// Valeur à placer dans le cookie.
    pub fn cookie_value(&self) -> String {
        format!("{}.{}", self.id, self.secret)
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    fn secret_hash(&self) -> Vec<u8> {
        Sha256::digest(self.secret.as_bytes()).to_vec()
    }
}

fn to_sql(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Millis, true)
}

/// Ouvre une session pour un compte et renvoie le jeton à confier au navigateur.
///
/// La purge des sessions expirées est faite ici : les connexions sont rares, et
/// c'est le seul moment où la table grossit. Cela évite une tâche de fond dédiée.
pub async fn create(pool: &SqlitePool, user_id: i64) -> Result<SessionToken> {
    let token = SessionToken::generate();
    let now = Utc::now();
    let expires_at = now + Duration::days(LIFETIME_DAYS);

    sqlx::query(
        "INSERT INTO auth_sessions (id, token_hash, created_at, expires_at, user_id)
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(&token.id)
    .bind(token.secret_hash())
    .bind(to_sql(now))
    .bind(to_sql(expires_at))
    .bind(user_id)
    .execute(pool)
    .await
    .context("ouverture de session")?;

    purge_expired(pool).await?;
    Ok(token)
}

/// Vérifie un jeton et renvoie le compte auquel la session appartient.
///
/// La recherche se fait sur la partie publique ; le secret, lui, est comparé en
/// temps constant, pour qu'aucune mesure de durée ne permette de reconstituer une
/// empreinte octet par octet. Une session sans compte — reliquat théorique de la
/// migration — vaut « pas authentifié ».
pub async fn authenticate(pool: &SqlitePool, token: &SessionToken) -> Result<Option<i64>> {
    let row = sqlx::query("SELECT token_hash, expires_at, user_id FROM auth_sessions WHERE id = ?")
        .bind(&token.id)
        .fetch_optional(pool)
        .await
        .context("lecture de la session")?;

    let Some(row) = row else { return Ok(None) };

    let stored: Vec<u8> = row.try_get("token_hash")?;
    let expires_at: String = row.try_get("expires_at")?;
    let user_id: Option<i64> = row.try_get("user_id")?;

    let matches: bool = stored.ct_eq(&token.secret_hash()).into();
    if !matches {
        return Ok(None);
    }

    // Une session expirée est refusée puis effacée : sans cela, un jeton volé
    // resterait indéfiniment en base à attendre la prochaine purge.
    let expired = DateTime::parse_from_rfc3339(&expires_at)
        .map(|at| at.with_timezone(&Utc) <= Utc::now())
        .unwrap_or(true);
    if expired {
        delete(pool, &token.id).await?;
        return Ok(None);
    }

    Ok(user_id)
}

pub async fn delete(pool: &SqlitePool, id: &str) -> Result<()> {
    sqlx::query("DELETE FROM auth_sessions WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await
        .context("fermeture de session")?;
    Ok(())
}

/// Ferme toutes les sessions d'un compte, sauf celle passée en argument.
///
/// C'est le geste du changement de mot de passe : on déconnecte partout ailleurs
/// sans se déconnecter soi-même, sinon l'utilisateur qui vient de sécuriser son
/// compte se retrouverait dehors. Sans `keep`, c'est la révocation complète —
/// compte désactivé, mot de passe remis par un administrateur.
pub async fn delete_for_user(pool: &SqlitePool, user_id: i64, keep: Option<&str>) -> Result<u64> {
    let result = match keep {
        Some(id) => {
            sqlx::query("DELETE FROM auth_sessions WHERE user_id = ? AND id <> ?")
                .bind(user_id)
                .bind(id)
                .execute(pool)
                .await
        }
        None => {
            sqlx::query("DELETE FROM auth_sessions WHERE user_id = ?")
                .bind(user_id)
                .execute(pool)
                .await
        }
    }
    .context("fermeture des autres sessions")?;
    Ok(result.rows_affected())
}

pub async fn purge_expired(pool: &SqlitePool) -> Result<u64> {
    let result = sqlx::query("DELETE FROM auth_sessions WHERE expires_at <= ?")
        .bind(to_sql(Utc::now()))
        .execute(pool)
        .await
        .context("purge des sessions expirées")?;
    Ok(result.rows_affected())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_token_survives_a_round_trip_through_the_cookie() {
        let token = SessionToken::generate();
        let parsed = SessionToken::parse(&token.cookie_value()).expect("jeton relu");
        assert_eq!(parsed.id, token.id);
        assert_eq!(parsed.secret, token.secret);
    }

    #[test]
    fn a_malformed_cookie_is_not_an_error() {
        assert!(SessionToken::parse("").is_none());
        assert!(SessionToken::parse("sans-point").is_none());
        assert!(SessionToken::parse(".secret").is_none());
        assert!(SessionToken::parse("identifiant.").is_none());
    }

    #[test]
    fn two_tokens_never_collide() {
        let first = SessionToken::generate();
        let second = SessionToken::generate();
        assert_ne!(first.id, second.id);
        assert_ne!(first.secret, second.secret);
        // 32 octets d'entropie, rendus en hexadécimal.
        assert_eq!(first.secret.len(), TOKEN_BYTES * 2);
    }

    /// Garde-fou : une régression ici publierait des sessions valides dans les
    /// journaux, où elles seraient réutilisables telles quelles.
    #[test]
    fn debug_never_leaks_a_secret() {
        let token = SessionToken::generate();
        let rendered = format!("{token:?}");
        assert!(!rendered.contains(&token.secret), "le jeton a fuité dans : {rendered}");
        assert!(rendered.contains("<redacted>"), "{rendered}");
    }
}
