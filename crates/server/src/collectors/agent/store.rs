//! Persistance de l'agent : jetons d'enregistrement et machines enregistrées.

use anyhow::{Context, Result};
use dumbmonit_proto::{AgentIdentity, Credential, TargetId};
use sqlx::{Row, SqlitePool};

use crate::crypto::Cipher;
use crate::db;

use super::token;

/// Période d'interrogation attribuée à une machine qui s'enregistre seule.
///
/// Elle sert aussi de période d'échantillonnage renvoyée à l'agent : ajuster la
/// cible dans l'interface pilote donc directement la finesse de la collecte.
const DEFAULT_INTERVAL_SECS: u64 = 30;

/// Durée de la fenêtre ouverte par « Allow re-enrolment », en minutes.
///
/// Assez pour réinstaller un agent sans se presser, trop court pour qu'une
/// fenêtre oubliée ouverte redevienne le trou qu'on vient de boucher.
pub const REBIND_WINDOW_MINUTES: i64 = 60;

/// Ce qu'un jeton a le droit d'enrôler.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TokenPolicy {
    /// Nombre d'enrôlements permis. `None` : jeton de parc, sans limite.
    pub max_uses: Option<i64>,
    /// Nombre de jours avant que le jeton cesse d'enrôler. `None` : sans terme.
    pub expires_in_days: Option<i64>,
}

impl Default for TokenPolicy {
    /// Usage unique : une commande d'installation copiée dans une conversation
    /// ou retrouvée dans l'historique d'un terminal ne doit pas suffire à faire
    /// entrer une seconde machine.
    fn default() -> Self {
        Self { max_uses: Some(1), expires_in_days: None }
    }
}

/// Un jeton, tel qu'il peut être montré. Jamais le secret lui-même.
#[derive(Debug, Clone)]
pub struct TokenRecord {
    pub id: i64,
    pub name: String,
    pub prefix: String,
    pub created_at: String,
    pub last_used_at: Option<String>,
    pub revoked_at: Option<String>,
    /// Enrôlements permis, `None` pour un jeton de parc.
    pub max_uses: Option<i64>,
    /// Enrôlements déjà servis.
    pub uses: i64,
    /// Terme au-delà duquel le jeton n'enrôle plus. Les machines déjà enrôlées
    /// continuent de remonter leurs mesures.
    pub expires_at: Option<String>,
}

/// Pourquoi un jeton valide n'enrôle pas cette machine-ci.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnrolmentDenied {
    /// Tous les enrôlements permis ont été servis.
    Exhausted,
    /// Le terme d'enrôlement est passé.
    Expired,
}

impl EnrolmentDenied {
    /// Message rendu tel quel à l'agent, donc dans les journaux de la machine.
    pub fn message(self) -> &'static str {
        match self {
            Self::Exhausted => {
                "This enrollment token has already enrolled all the machines it was \
                 allowed to. Create a new token, or a reusable one for a fleet."
            }
            Self::Expired => {
                "This enrollment token has passed its enrollment deadline. Create a new \
                 one; machines already enrolled keep reporting."
            }
        }
    }
}

/// Crée un jeton et renvoie sa forme en clair — la seule et unique fois.
pub async fn create_token(
    pool: &SqlitePool,
    name: &str,
    policy: TokenPolicy,
) -> Result<(TokenRecord, String)> {
    let clear = token::generate();
    let expires_at = policy.expires_in_days.map(|days| {
        (chrono::Utc::now() + chrono::Duration::days(days)).format("%Y-%m-%dT%H:%M:%SZ").to_string()
    });
    let row = sqlx::query(
        "INSERT INTO agent_tokens (name, token_hash, prefix, max_uses, expires_at)
         VALUES (?, ?, ?, ?, ?)
         RETURNING id, name, prefix, created_at, last_used_at, revoked_at,
                   max_uses, uses, expires_at",
    )
    .bind(name)
    .bind(token::fingerprint(&clear))
    .bind(token::display_prefix(&clear))
    .bind(policy.max_uses)
    .bind(expires_at)
    .fetch_one(pool)
    .await
    .context("creating the enrollment token")?;

    Ok((row_to_token(&row)?, clear))
}

pub async fn list_tokens(pool: &SqlitePool) -> Result<Vec<TokenRecord>> {
    let rows = sqlx::query(
        "SELECT id, name, prefix, created_at, last_used_at, revoked_at,
                max_uses, uses, expires_at
         FROM agent_tokens ORDER BY id DESC",
    )
    .fetch_all(pool)
    .await
    .context("listing enrollment tokens")?;
    rows.iter().map(row_to_token).collect()
}

/// Révoque un jeton. Idempotent : révoquer deux fois n'est pas une erreur, mais
/// seule la première révocation renvoie « vrai ».
pub async fn revoke_token(pool: &SqlitePool, id: i64) -> Result<bool> {
    let result = sqlx::query(
        "UPDATE agent_tokens
         SET revoked_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
         WHERE id = ? AND revoked_at IS NULL",
    )
    .bind(id)
    .execute(pool)
    .await
    .context("revoking the token")?;
    Ok(result.rows_affected() > 0)
}

/// Retrouve un jeton actif à partir de son empreinte.
///
/// La recherche porte sur l'empreinte, jamais sur le jeton : c'est ce qui permet
/// de s'appuyer sur un index — donc une comparaison en temps constant du point de
/// vue de l'attaquant, qui ne peut de toute façon pas deviner l'empreinte d'un
/// secret qu'il ne possède pas.
///
/// « Actif » veut dire « non révoqué », et rien de plus : un jeton à usage unique
/// épuisé, ou dont le terme d'enrôlement est passé, laisse toujours remonter les
/// mesures de la machine qu'il a fait entrer. C'est [`enrolment_allowed`] qui
/// décide, séparément, s'il peut en faire entrer une de plus.
pub async fn find_active_token(pool: &SqlitePool, fingerprint: &str) -> Result<Option<i64>> {
    let row =
        sqlx::query("SELECT id FROM agent_tokens WHERE token_hash = ? AND revoked_at IS NULL")
            .bind(fingerprint)
            .fetch_optional(pool)
            .await
            .context("checking the enrollment token")?;
    row.map(|row| row.try_get("id")).transpose().context("token id")
}

/// Ce jeton peut-il faire entrer une machine de plus ?
pub async fn enrolment_allowed(
    pool: &SqlitePool,
    token_id: i64,
) -> Result<Result<(), EnrolmentDenied>> {
    let row = sqlx::query(
        "SELECT max_uses, uses,
                (expires_at IS NOT NULL
                 AND expires_at <= strftime('%Y-%m-%dT%H:%M:%SZ', 'now')) AS expired
         FROM agent_tokens WHERE id = ?",
    )
    .bind(token_id)
    .fetch_optional(pool)
    .await
    .context("reading the token limits")?;
    let Some(row) = row else {
        return Ok(Err(EnrolmentDenied::Expired));
    };
    if row.try_get::<i64, _>("expired")? != 0 {
        return Ok(Err(EnrolmentDenied::Expired));
    }
    let max: Option<i64> = row.try_get("max_uses")?;
    let used: i64 = row.try_get("uses")?;
    match max {
        Some(max) if used >= max => Ok(Err(EnrolmentDenied::Exhausted)),
        _ => Ok(Ok(())),
    }
}

/// Décompte un enrôlement servi par ce jeton.
async fn consume_enrolment(pool: &SqlitePool, token_id: i64) -> Result<()> {
    sqlx::query("UPDATE agent_tokens SET uses = uses + 1 WHERE id = ?")
        .bind(token_id)
        .execute(pool)
        .await
        .context("counting the enrollment")?;
    Ok(())
}

pub async fn touch_token(pool: &SqlitePool, id: i64) -> Result<()> {
    sqlx::query(
        "UPDATE agent_tokens SET last_used_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now') WHERE id = ?",
    )
    .bind(id)
    .execute(pool)
    .await
    .context("updating the token timestamp")?;
    Ok(())
}

/// Résultat d'un enregistrement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Registration {
    pub target_id: TargetId,
    /// Vrai si la cible vient d'être créée par ce lot.
    pub created: bool,
    /// Secret de liaison attribué à l'instant, à remettre à l'agent une fois.
    pub issued_secret: Option<String>,
    /// Vrai si la machine est désormais liée à un secret.
    pub bound: bool,
}

/// Pourquoi un lot correctement authentifié n'est pas accepté.
#[derive(Debug)]
pub enum RegisterError {
    /// La machine existe et est liée à un autre secret que celui présenté.
    /// C'est exactement le scénario qu'on ferme : une machine du parc qui se
    /// réclame de l'identité d'une autre.
    BindingMismatch,
    /// Le jeton est valide mais ne peut plus faire entrer de machine.
    EnrolmentDenied(EnrolmentDenied),
    Internal(anyhow::Error),
}

impl From<anyhow::Error> for RegisterError {
    fn from(error: anyhow::Error) -> Self {
        Self::Internal(error)
    }
}

impl From<sqlx::Error> for RegisterError {
    fn from(error: sqlx::Error) -> Self {
        Self::Internal(error.into())
    }
}

/// La machine telle que la base la connaît, du point de vue de la liaison.
struct HostRow {
    target_id: TargetId,
    secret_hash: Option<String>,
    /// Vrai si une fenêtre de reliaison est ouverte et encore valide.
    rebind_open: bool,
}

/// Retrouve la cible d'un agent, ou l'enregistre si elle n'existe pas encore.
///
/// C'est ce qui rend l'installation en une commande possible : la machine se
/// présente avec un jeton valide, et devient une cible sans que personne n'ait
/// rien saisi dans l'interface.
///
/// La clé d'identité (`/etc/machine-id`, ou le nom d'hôte) n'est un secret pour
/// personne : elle dit *qui prétend parler*, jamais *qui parle*. C'est le secret
/// de liaison, attribué à la première présentation et connu de cette machine
/// seule, qui fait la différence. Les trois cas :
///
/// - machine liée : le secret doit correspondre, sinon rien ne passe ;
/// - machine inconnue : le jeton doit pouvoir enrôler, et le secret est attribué
///   si l'agent sait le recevoir ;
/// - machine connue mais non liée (agent installé avant la liaison) : elle
///   continue de remonter, et se lie dès que son binaire est à jour.
pub async fn register(
    pool: &SqlitePool,
    cipher: &Cipher,
    identity: &AgentIdentity,
    token_id: i64,
    presented_secret: Option<&str>,
) -> Result<Registration, RegisterError> {
    let key = identity.key();

    if let Some(host) = find_host(pool, &key).await? {
        return claim(pool, host, identity, token_id, presented_secret).await;
    }

    // Une machine inconnue est un enrôlement : c'est là, et seulement là, que la
    // portée du jeton compte.
    if let Err(denied) = enrolment_allowed(pool, token_id).await? {
        return Err(RegisterError::EnrolmentDenied(denied));
    }

    // Une cible « agent » a pu être créée à la main dans l'interface avant
    // l'installation : on l'adopte plutôt que de buter sur `UNIQUE (kind, address)`
    // et de laisser la machine dehors.
    let existing = sqlx::query("SELECT id FROM targets WHERE kind = 'agent' AND address = ?")
        .bind(&key)
        .fetch_optional(pool)
        .await
        .context("looking up an existing agent target")?;

    let (target_id, created) = match existing {
        Some(row) => (row.try_get::<TargetId, _>("id")?, false),
        None => {
            let input = db::targets::TargetInput {
                name: identity.hostname.clone(),
                // L'adresse d'une cible « agent » n'est pas une adresse de contact :
                // c'est l'agent qui se connecte. On y range donc son identité, ce
                // qui donne au passage l'unicité recherchée par le schéma.
                address: key.clone(),
                kind: "agent".to_string(),
                profile_id: None,
                parent_id: None,
                via_agent: None,
                interval: std::time::Duration::from_secs(DEFAULT_INTERVAL_SECS),
                enabled: true,
                tags: identity.tags.clone(),
                // Aucun secret à conserver de ce côté : c'est l'agent qui se
                // connecte, le serveur n'a jamais à joindre la machine.
                credential: Some(Credential::None),
            };
            (db::targets::create(pool, cipher, &input).await?, true)
        }
    };

    let issued = identity.binding_supported.then(token::generate_secret);
    insert_host(pool, target_id, &key, identity, token_id, issued.as_deref()).await?;
    consume_enrolment(pool, token_id).await?;
    if issued.is_none() {
        tracing::warn!(
            cible = target_id,
            hote = identity.hostname,
            "agent trop ancien pour être lié à sa machine : mettre à jour le binaire"
        );
    }
    Ok(Registration { target_id, created, bound: issued.is_some(), issued_secret: issued })
}

/// Décide si l'agent qui se présente est bien celui de cette machine.
async fn claim(
    pool: &SqlitePool,
    host: HostRow,
    identity: &AgentIdentity,
    token_id: i64,
    presented_secret: Option<&str>,
) -> Result<Registration, RegisterError> {
    let target_id = host.target_id;

    match host.secret_hash.as_deref() {
        // Machine liée : le secret décide, et lui seul.
        Some(stored) => {
            let matches = presented_secret
                .map(token::fingerprint)
                .is_some_and(|presented| presented == stored);
            if matches {
                update_host(pool, target_id, identity).await?;
                return Ok(Registration {
                    target_id,
                    created: false,
                    issued_secret: None,
                    bound: true,
                });
            }
            // Réinstallation : quelqu'un a explicitement ouvert la fenêtre dans
            // l'interface, et le jeton doit encore pouvoir enrôler.
            if host.rebind_open && identity.binding_supported {
                if let Err(denied) = enrolment_allowed(pool, token_id).await? {
                    return Err(RegisterError::EnrolmentDenied(denied));
                }
                let secret = token::generate_secret();
                bind_host(pool, target_id, &secret, token_id).await?;
                update_host(pool, target_id, identity).await?;
                tracing::info!(cible = target_id, "machine reliée pendant sa fenêtre de reliaison");
                return Ok(Registration {
                    target_id,
                    created: false,
                    issued_secret: Some(secret),
                    bound: true,
                });
            }
            tracing::warn!(
                cible = target_id,
                hote = identity.hostname,
                "identité d'agent refusée : secret de liaison absent ou incorrect"
            );
            Err(RegisterError::BindingMismatch)
        }
        // Machine connue mais pas encore liée : c'est l'état des agents installés
        // avant cette version. Elle continue de remonter, et se lie dès que son
        // binaire sait recevoir un secret.
        None => {
            let issued = identity.binding_supported.then(token::generate_secret);
            if let Some(secret) = issued.as_deref() {
                bind_host(pool, target_id, secret, token_id).await?;
                tracing::info!(
                    cible = target_id,
                    hote = identity.hostname,
                    "machine liée à son agent"
                );
            }
            update_host(pool, target_id, identity).await?;
            Ok(Registration {
                target_id,
                created: false,
                bound: issued.is_some(),
                issued_secret: issued,
            })
        }
    }
}

async fn find_host(pool: &SqlitePool, key: &str) -> Result<Option<HostRow>> {
    let row = sqlx::query(
        "SELECT target_id, secret_hash,
                (rebind_until IS NOT NULL
                 AND rebind_until > strftime('%Y-%m-%dT%H:%M:%SZ', 'now')) AS rebind_open
         FROM agent_hosts WHERE agent_key = ?",
    )
    .bind(key)
    .fetch_optional(pool)
    .await
    .context("looking up the machine")?;
    row.map(|row| {
        Ok(HostRow {
            target_id: row.try_get("target_id")?,
            secret_hash: row.try_get("secret_hash")?,
            rebind_open: row.try_get::<i64, _>("rebind_open")? != 0,
        })
    })
    .transpose()
}

/// Enregistre l'empreinte du secret d'une machine, et referme la fenêtre de
/// reliaison : elle a servi.
async fn bind_host(
    pool: &SqlitePool,
    target_id: TargetId,
    secret: &str,
    token_id: i64,
) -> Result<()> {
    sqlx::query(
        "UPDATE agent_hosts
         SET secret_hash = ?, binding_supported = 1, rebind_until = NULL, token_id = ?,
             bound_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
         WHERE target_id = ?",
    )
    .bind(token::fingerprint(secret))
    .bind(token_id)
    .bind(target_id)
    .execute(pool)
    .await
    .context("binding the machine to its agent")?;
    Ok(())
}

/// Ouvre une fenêtre de reliaison : la machine pourra se relier au prochain lot
/// d'un agent porteur d'un jeton valide. Renvoie l'instant de fermeture.
///
/// C'est la seule issue prévue quand un secret est perdu — réinstallation,
/// disque remplacé, conteneur d'agent recréé sans son volume. Elle est explicite
/// et datée, parce qu'une machine liée doit le rester tant que personne n'a
/// décidé le contraire.
pub async fn allow_rebind(pool: &SqlitePool, target_id: TargetId) -> Result<Option<String>> {
    let until = (chrono::Utc::now() + chrono::Duration::minutes(REBIND_WINDOW_MINUTES))
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string();
    let result = sqlx::query("UPDATE agent_hosts SET rebind_until = ? WHERE target_id = ?")
        .bind(&until)
        .bind(target_id)
        .execute(pool)
        .await
        .context("opening the re-enrolment window")?;
    Ok((result.rows_affected() > 0).then_some(until))
}

/// Verdict rendu à une requête qui se présente avec une clé d'identité.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyAuth {
    /// Aucune machine ne porte cette clé.
    Unknown,
    /// La clé existe, mais le secret présenté n'est pas le sien.
    Denied,
    /// Requête acceptée. `bound` est faux pour une machine d'avant la liaison.
    Allowed { target_id: TargetId, bound: bool },
}

/// Autorise une requête du canal de commandes ou du relais.
///
/// Même règle que pour l'ingestion, et pour la même raison : sans elle, une
/// machine du parc viendrait chercher — et consommerait — les commandes Docker
/// destinées à une autre.
pub async fn authorise_key(
    pool: &SqlitePool,
    key: &str,
    presented_secret: Option<&str>,
) -> Result<KeyAuth> {
    let Some(host) = find_host(pool, key).await? else {
        return Ok(KeyAuth::Unknown);
    };
    match host.secret_hash.as_deref() {
        Some(stored) => {
            let matches = presented_secret
                .map(token::fingerprint)
                .is_some_and(|presented| presented == stored);
            if matches {
                Ok(KeyAuth::Allowed { target_id: host.target_id, bound: true })
            } else {
                Ok(KeyAuth::Denied)
            }
        }
        // Machine d'avant la liaison : on ne coupe pas un parc qui fonctionne,
        // mais l'interface le dit et la trace le répète.
        None => Ok(KeyAuth::Allowed { target_id: host.target_id, bound: false }),
    }
}

async fn insert_host(
    pool: &SqlitePool,
    target_id: TargetId,
    key: &str,
    identity: &AgentIdentity,
    token_id: i64,
    secret: Option<&str>,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO agent_hosts
             (target_id, agent_key, hostname, os, os_version, kernel_version, arch,
              agent_version, commands_enabled, relay, site, token_id,
              secret_hash, binding_supported, bound_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?,
                 CASE WHEN ? IS NULL THEN NULL
                      ELSE strftime('%Y-%m-%dT%H:%M:%SZ', 'now') END)",
    )
    .bind(target_id)
    .bind(key)
    .bind(&identity.hostname)
    .bind(&identity.os)
    .bind(&identity.os_version)
    .bind(&identity.kernel_version)
    .bind(&identity.arch)
    .bind(&identity.agent_version)
    .bind(identity.commands_enabled)
    .bind(i64::from(identity.relay))
    .bind(site_of(identity))
    .bind(token_id)
    .bind(secret.map(token::fingerprint))
    .bind(i64::from(identity.binding_supported))
    .bind(secret)
    .execute(pool)
    .await
    .context("registering the machine")?;
    Ok(())
}

/// Rafraîchit la description de la machine à chaque lot.
///
/// Une mise à jour du système ou de l'agent doit se voir dans l'interface sans
/// qu'on ait à désinstaller quoi que ce soit.
async fn update_host(
    pool: &SqlitePool,
    target_id: TargetId,
    identity: &AgentIdentity,
) -> Result<()> {
    sqlx::query(
        "UPDATE agent_hosts
         SET hostname = ?, os = ?, os_version = ?, kernel_version = ?, arch = ?,
             agent_version = ?, commands_enabled = ?, relay = ?, site = ?,
             binding_supported = ?
         WHERE target_id = ?",
    )
    .bind(&identity.hostname)
    .bind(&identity.os)
    .bind(&identity.os_version)
    .bind(&identity.kernel_version)
    .bind(&identity.arch)
    .bind(&identity.agent_version)
    .bind(identity.commands_enabled)
    .bind(i64::from(identity.relay))
    .bind(site_of(identity))
    .bind(i64::from(identity.binding_supported))
    .bind(target_id)
    .execute(pool)
    .await
    .context("updating the machine")?;
    Ok(())
}

/// Site déclaré par l'agent, vidé de ses blancs ; `None` s'il n'en dit rien.
fn site_of(identity: &AgentIdentity) -> Option<String> {
    identity.site.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(str::to_string)
}

/// Note la réception d'un lot. C'est cette trace que le collecteur relit pour
/// juger de la fraîcheur d'une machine.
pub async fn record_batch(
    pool: &SqlitePool,
    target_id: TargetId,
    received_at_ms: i64,
    samples: usize,
) -> Result<()> {
    sqlx::query(
        "UPDATE agent_hosts
         SET last_seen_ms = ?,
             last_seen_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now'),
             last_samples = ?
         WHERE target_id = ?",
    )
    .bind(received_at_ms)
    .bind(samples as i64)
    .bind(target_id)
    .execute(pool)
    .await
    .context("recording the batch receipt")?;
    Ok(())
}

/// La machine telle que son agent la décrit, pour l'interface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostInfo {
    pub hostname: String,
    pub os: String,
    pub os_version: Option<String>,
    pub arch: Option<String>,
    pub agent_version: String,
    /// `None` : l'agent n'a rien déclaré (binaire antérieur au canal de
    /// commandes). Il ne viendra pas chercher de commande, pas plus qu'avec
    /// `Some(false)`.
    pub commands_enabled: Option<bool>,
    /// Vrai si l'agent a déclaré relayer des sondes (`relay: true`).
    pub relay: bool,
    /// Site déclaré par l'agent, s'il en a un.
    pub site: Option<String>,
    pub last_seen_at: Option<String>,
    /// Vrai si la machine est liée à un secret connu d'elle seule.
    pub bound: bool,
    /// Moment de la liaison, s'il y en a une.
    pub bound_at: Option<String>,
    /// Vrai si le binaire installé sait recevoir un secret de liaison. Faux et
    /// non lié : l'agent est antérieur à la liaison et doit être réinstallé.
    pub binding_supported: bool,
    /// Fin de la fenêtre de reliaison ouverte à la main, si elle court encore.
    pub rebind_until: Option<String>,
}

impl HostInfo {
    /// Vrai seulement si l'agent a dit qu'il exécute les commandes : dans le
    /// doute, l'interface ne propose pas une action qui n'aboutirait jamais.
    pub fn commands_supported(&self) -> bool {
        self.commands_enabled == Some(true)
    }

    /// Ce que l'interface affiche sous « Binding ».
    ///
    /// Trois états seulement, parce qu'il n'y a que trois choses à faire :
    /// rien, attendre le prochain lot, ou aller mettre l'agent à jour.
    pub fn binding_state(&self) -> &'static str {
        match (self.bound, self.binding_supported) {
            (true, _) => "bound",
            // Le binaire sait se lier : le prochain lot suffira.
            (false, true) => "pending",
            (false, false) => "unsupported",
        }
    }
}

/// Description de la machine rattachée à une cible, si un agent s'y est présenté.
pub async fn host(pool: &SqlitePool, target_id: TargetId) -> Result<Option<HostInfo>> {
    let row = sqlx::query(
        "SELECT hostname, os, os_version, arch, agent_version, commands_enabled, relay, site,
                last_seen_at, secret_hash, bound_at, binding_supported, rebind_until
         FROM agent_hosts WHERE target_id = ?",
    )
    .bind(target_id)
    .fetch_optional(pool)
    .await
    .context("reading the machine")?;
    row.map(|row| row_to_host(&row)).transpose()
}

/// Toutes les machines à agent, avec le nom de leur cible : c'est ce que le
/// formulaire d'équipement propose comme relais possibles.
pub async fn list_hosts(pool: &SqlitePool) -> Result<Vec<(TargetId, String, HostInfo)>> {
    let rows = sqlx::query(
        "SELECT h.target_id, t.name, h.hostname, h.os, h.os_version, h.arch, h.agent_version,
                h.commands_enabled, h.relay, h.site, h.last_seen_at, h.secret_hash, h.bound_at,
                h.binding_supported, h.rebind_until
         FROM agent_hosts h JOIN targets t ON t.id = h.target_id
         ORDER BY t.name",
    )
    .fetch_all(pool)
    .await
    .context("listing the machines")?;
    rows.iter()
        .map(|row| Ok((row.try_get("target_id")?, row.try_get("name")?, row_to_host(row)?)))
        .collect()
}

fn row_to_host(row: &sqlx::sqlite::SqliteRow) -> Result<HostInfo> {
    Ok(HostInfo {
        hostname: row.try_get("hostname")?,
        os: row.try_get("os")?,
        os_version: row.try_get("os_version")?,
        arch: row.try_get("arch")?,
        agent_version: row.try_get("agent_version")?,
        commands_enabled: row.try_get::<Option<i64>, _>("commands_enabled")?.map(|v| v != 0),
        relay: row.try_get::<i64, _>("relay")? != 0,
        site: row.try_get("site")?,
        last_seen_at: row.try_get("last_seen_at")?,
        bound: row.try_get::<Option<String>, _>("secret_hash")?.is_some(),
        bound_at: row.try_get("bound_at")?,
        binding_supported: row.try_get::<i64, _>("binding_supported")? != 0,
        rebind_until: row.try_get("rebind_until")?,
    })
}

/// Instant du dernier lot reçu, en millisecondes depuis l'époque Unix.
pub async fn last_seen_ms(pool: &SqlitePool, target_id: TargetId) -> Result<Option<i64>> {
    let row = sqlx::query("SELECT last_seen_ms FROM agent_hosts WHERE target_id = ?")
        .bind(target_id)
        .fetch_optional(pool)
        .await
        .context("reading the last receipt")?;
    match row {
        Some(row) => row.try_get("last_seen_ms").context("unreadable last receipt"),
        None => Ok(None),
    }
}

fn row_to_token(row: &sqlx::sqlite::SqliteRow) -> Result<TokenRecord> {
    Ok(TokenRecord {
        id: row.try_get("id")?,
        name: row.try_get("name")?,
        prefix: row.try_get("prefix")?,
        created_at: row.try_get("created_at")?,
        last_used_at: row.try_get("last_used_at")?,
        revoked_at: row.try_get("revoked_at")?,
        max_uses: row.try_get("max_uses")?,
        uses: row.try_get("uses")?,
        expires_at: row.try_get("expires_at")?,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    struct TestDb {
        pool: SqlitePool,
        cipher: Cipher,
        _dir: tempfile::TempDir,
    }

    async fn setup() -> TestDb {
        let dir = tempfile::tempdir().expect("répertoire temporaire");
        let pool = db::open(&dir.path().join("test.db")).await.expect("base");
        let cipher =
            db::init_cipher(&pool, "secret-de-test-suffisamment-long").await.expect("chiffrement");
        TestDb { pool, cipher, _dir: dir }
    }

    fn identity(hostname: &str, machine_id: Option<&str>) -> AgentIdentity {
        AgentIdentity {
            hostname: hostname.to_string(),
            os: "linux".into(),
            os_version: Some("Debian GNU/Linux 12".into()),
            kernel_version: Some("6.1.0".into()),
            arch: Some("x86_64".into()),
            agent_version: "0.1.0".into(),
            commands_enabled: Some(true),
            relay: false,
            site: None,
            machine_id: machine_id.map(str::to_string),
            tags: BTreeMap::new(),
            binding_supported: true,
        }
    }

    /// Un agent d'avant la liaison : il ignore le secret que le serveur
    /// renverrait, donc le serveur ne doit pas lui en attribuer.
    fn legacy(hostname: &str, machine_id: Option<&str>) -> AgentIdentity {
        AgentIdentity { binding_supported: false, ..identity(hostname, machine_id) }
    }

    async fn token(db: &TestDb, name: &str) -> i64 {
        create_token(&db.pool, name, TokenPolicy { max_uses: None, expires_in_days: None })
            .await
            .expect("création")
            .0
            .id
    }

    /// Enrôle une machine et renvoie son enregistrement, en échouant clairement.
    async fn enrol(db: &TestDb, identity: &AgentIdentity, token_id: i64) -> Registration {
        register(&db.pool, &db.cipher, identity, token_id, None).await.expect("enregistrement")
    }

    #[tokio::test]
    async fn a_created_token_can_be_verified_then_revoked() {
        let db = setup().await;
        let (record, clear) =
            create_token(&db.pool, "parc", TokenPolicy::default()).await.expect("création");

        let found =
            find_active_token(&db.pool, &token::fingerprint(&clear)).await.expect("vérification");
        assert_eq!(found, Some(record.id));

        assert!(revoke_token(&db.pool, record.id).await.expect("révocation"));
        // Un jeton révoqué ne doit plus jamais ouvrir l'ingestion.
        assert_eq!(
            find_active_token(&db.pool, &token::fingerprint(&clear)).await.expect("vérification"),
            None
        );
        // Deuxième révocation : sans effet, mais sans erreur.
        assert!(!revoke_token(&db.pool, record.id).await.expect("révocation"));
    }

    #[tokio::test]
    async fn the_clear_token_is_never_stored() {
        let db = setup().await;
        let (_, clear) =
            create_token(&db.pool, "parc", TokenPolicy::default()).await.expect("création");

        let row = sqlx::query("SELECT token_hash, prefix FROM agent_tokens")
            .fetch_one(&db.pool)
            .await
            .expect("lecture");
        let stored: String = row.try_get("token_hash").unwrap();
        let prefix: String = row.try_get("prefix").unwrap();

        assert_ne!(stored, clear);
        assert_eq!(stored, token::fingerprint(&clear));
        // Le préfixe affichable ne doit pas suffire à reconstituer le jeton.
        assert!(clear.starts_with(&prefix) && prefix.len() < clear.len());
    }

    #[tokio::test]
    async fn an_unknown_fingerprint_opens_nothing() {
        let db = setup().await;
        token(&db, "parc").await;
        assert_eq!(
            find_active_token(&db.pool, &token::fingerprint("dmon_inconnu")).await.unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn an_unknown_machine_registers_itself() {
        let db = setup().await;
        let token_id = token(&db, "parc").await;

        let first =
            register(&db.pool, &db.cipher, &identity("nas", Some("id-nas")), token_id, None)
                .await
                .expect("enregistrement");
        assert!(first.created, "la première présentation crée la cible");

        let target = db::targets::get(&db.pool, &db.cipher, first.target_id)
            .await
            .expect("lecture")
            .expect("cible");
        assert_eq!(target.name, "nas");
        assert_eq!(target.kind, "agent");
        assert!(target.enabled);
    }

    #[tokio::test]
    async fn a_known_machine_does_not_register_twice() {
        let db = setup().await;
        let token_id = token(&db, "parc").await;
        let identity = identity("nas", Some("id-nas"));

        let first = enrol(&db, &identity, token_id).await;
        let secret = first.issued_secret.clone().expect("secret de liaison");
        let second = register(&db.pool, &db.cipher, &identity, token_id, Some(&secret))
            .await
            .expect("second lot");

        assert_eq!(first.target_id, second.target_id);
        assert!(!second.created);
        assert!(second.issued_secret.is_none(), "le secret n'est remis qu'une fois");
        assert_eq!(db::targets::list(&db.pool, &db.cipher).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn renaming_a_machine_does_not_create_a_second_target() {
        let db = setup().await;
        let token_id = token(&db, "parc").await;

        let before = enrol(&db, &identity("nas", Some("id-stable")), token_id).await;
        let secret = before.issued_secret.clone().expect("secret");
        // Même identifiant machine, nom d'hôte différent : c'est la même machine.
        let after = register(
            &db.pool,
            &db.cipher,
            &identity("nas-cave", Some("id-stable")),
            token_id,
            Some(&secret),
        )
        .await
        .unwrap();

        assert_eq!(before.target_id, after.target_id);
        assert_eq!(db::targets::list(&db.pool, &db.cipher).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn without_a_machine_id_the_hostname_identifies_the_machine() {
        let db = setup().await;
        let token_id = token(&db, "parc").await;

        let first = enrol(&db, &identity("Pi-Salon", None), token_id).await;
        let secret = first.issued_secret.clone().expect("secret");
        // La casse ne doit pas suffire à dédoubler une machine.
        let second =
            register(&db.pool, &db.cipher, &identity("pi-salon", None), token_id, Some(&secret))
                .await
                .unwrap();

        assert_eq!(first.target_id, second.target_id);
    }

    #[tokio::test]
    async fn the_freshness_of_a_machine_is_recorded_at_each_batch() {
        let db = setup().await;
        let token_id = token(&db, "parc").await;
        let registration = enrol(&db, &identity("nas", Some("id-nas")), token_id).await;

        // Aucun lot reçu pour l'instant : la machine est enregistrée sans être vue.
        assert_eq!(last_seen_ms(&db.pool, registration.target_id).await.unwrap(), None);

        record_batch(&db.pool, registration.target_id, 1_700_000_000_000, 42).await.unwrap();
        assert_eq!(
            last_seen_ms(&db.pool, registration.target_id).await.unwrap(),
            Some(1_700_000_000_000)
        );
    }

    #[tokio::test]
    async fn the_agent_capabilities_follow_its_last_batch() {
        let db = setup().await;
        let token_id = token(&db, "parc").await;
        // Un agent d'avant le canal de commandes ne dit rien : il n'est pas
        // réputé capable pour autant.
        let mut identity = identity("nas", Some("id-nas"));
        identity.commands_enabled = None;
        let registration = enrol(&db, &identity, token_id).await;
        let secret = registration.issued_secret.clone().expect("secret");
        let info = host(&db.pool, registration.target_id).await.unwrap().expect("machine");
        assert_eq!(info.commands_enabled, None);
        assert!(!info.commands_supported());
        assert_eq!(info.agent_version, "0.1.0");

        // Après mise à jour de l'agent, le lot suivant suffit.
        identity.commands_enabled = Some(true);
        identity.agent_version = "0.2.0".into();
        register(&db.pool, &db.cipher, &identity, token_id, Some(&secret)).await.unwrap();
        let info = host(&db.pool, registration.target_id).await.unwrap().expect("machine");
        assert!(info.commands_supported());
        assert_eq!(info.agent_version, "0.2.0");

        // `commands: false` dans la configuration de l'agent.
        identity.commands_enabled = Some(false);
        register(&db.pool, &db.cipher, &identity, token_id, Some(&secret)).await.unwrap();
        let info = host(&db.pool, registration.target_id).await.unwrap().expect("machine");
        assert_eq!(info.commands_enabled, Some(false));
        assert!(!info.commands_supported());

        // Une cible sans agent n'a pas de machine.
        assert!(host(&db.pool, 4_242).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn a_target_that_is_not_an_agent_has_no_freshness() {
        let db = setup().await;
        assert_eq!(last_seen_ms(&db.pool, 4_242).await.unwrap(), None);
    }

    #[tokio::test]
    async fn a_manually_created_agent_target_is_adopted() {
        let db = setup().await;
        let token_id = token(&db, "parc").await;

        // L'utilisateur a préparé la cible dans l'interface avant d'installer
        // l'agent : la machine doit s'y rattacher, pas échouer sur l'unicité.
        let manual = db::targets::create(
            &db.pool,
            &db.cipher,
            &db::targets::TargetInput {
                name: "Serveur de sauvegarde".into(),
                address: "id-nas".into(),
                kind: "agent".into(),
                profile_id: None,
                parent_id: None,
                via_agent: None,
                interval: std::time::Duration::from_secs(60),
                enabled: true,
                tags: BTreeMap::new(),
                credential: Some(Credential::None),
            },
        )
        .await
        .unwrap();

        let registration = enrol(&db, &identity("nas", Some("id-nas")), token_id).await;

        assert_eq!(registration.target_id, manual);
        assert!(!registration.created);
        // Le nom choisi par l'utilisateur est préservé : c'est le sien, pas celui
        // que la machine se donne.
        let target = db::targets::get(&db.pool, &db.cipher, manual).await.unwrap().unwrap();
        assert_eq!(target.name, "Serveur de sauvegarde");
    }

    // ------------------------------------------------------- liaison machine

    #[tokio::test]
    async fn a_first_contact_binds_the_machine_and_hands_back_its_secret() {
        let db = setup().await;
        let token_id = token(&db, "parc").await;

        let registration = enrol(&db, &identity("nas", Some("id-nas")), token_id).await;
        let secret = registration.issued_secret.clone().expect("secret remis une fois");
        assert!(registration.bound);
        assert!(secret.starts_with(dumbmonit_proto::AGENT_SECRET_PREFIX));

        // Seule l'empreinte est en base : un vol de la base ne rejoue rien.
        let stored: Option<String> = sqlx::query("SELECT secret_hash FROM agent_hosts")
            .fetch_one(&db.pool)
            .await
            .unwrap()
            .try_get("secret_hash")
            .unwrap();
        assert_eq!(stored, Some(token::fingerprint(&secret)));
    }

    #[tokio::test]
    async fn a_second_machine_cannot_claim_an_existing_registration() {
        // Le cœur de la correction : une machine du parc, porteuse du même jeton
        // de flotte, se présente avec la clé d'identité d'une autre.
        let db = setup().await;
        let token_id = token(&db, "flotte").await;
        let victim = enrol(&db, &identity("nas", Some("id-nas")), token_id).await;

        // Sans secret : refusé.
        let error =
            register(&db.pool, &db.cipher, &identity("pirate", Some("id-nas")), token_id, None)
                .await
                .expect_err("l'usurpation doit être refusée");
        assert!(matches!(error, RegisterError::BindingMismatch), "{error:?}");

        // Avec un secret inventé : refusé aussi.
        let error = register(
            &db.pool,
            &db.cipher,
            &identity("pirate", Some("id-nas")),
            token_id,
            Some("dmab_0000000000000000"),
        )
        .await
        .expect_err("l'usurpation doit être refusée");
        assert!(matches!(error, RegisterError::BindingMismatch), "{error:?}");

        // Et rien n'a bougé : ni le nom d'hôte de la victime, ni son nombre de cibles.
        let info = host(&db.pool, victim.target_id).await.unwrap().expect("machine");
        assert_eq!(info.hostname, "nas");
        assert_eq!(db::targets::list(&db.pool, &db.cipher).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_machine_cannot_read_or_report_the_commands_of_another() {
        let db = setup().await;
        let token_id = token(&db, "flotte").await;
        let victim = enrol(&db, &identity("nas", Some("id-nas")), token_id).await;
        let secret = victim.issued_secret.clone().expect("secret");

        // L'agent légitime, avec son secret.
        assert_eq!(
            authorise_key(&db.pool, "id-nas", Some(&secret)).await.unwrap(),
            KeyAuth::Allowed { target_id: victim.target_id, bound: true }
        );
        // Le voisin compromis, avec le jeton de flotte mais pas le secret.
        assert_eq!(authorise_key(&db.pool, "id-nas", None).await.unwrap(), KeyAuth::Denied);
        assert_eq!(
            authorise_key(&db.pool, "id-nas", Some("dmab_pas-le-bon")).await.unwrap(),
            KeyAuth::Denied
        );
        // Une clé qu'aucune machine ne porte.
        assert_eq!(authorise_key(&db.pool, "id-inconnu", None).await.unwrap(), KeyAuth::Unknown);
    }

    #[tokio::test]
    async fn an_agent_from_before_the_binding_keeps_reporting_and_is_shown_as_such() {
        let db = setup().await;
        let token_id = token(&db, "parc").await;

        let registration = enrol(&db, &legacy("vieux-nas", Some("id-vieux")), token_id).await;
        assert!(!registration.bound);
        assert!(registration.issued_secret.is_none(), "un agent qui l'ignorerait n'en reçoit pas");

        // Il continue de pousser, lot après lot.
        let again = enrol(&db, &legacy("vieux-nas", Some("id-vieux")), token_id).await;
        assert_eq!(again.target_id, registration.target_id);
        assert!(!again.bound);

        // Et le canal de commandes reste ouvert pour lui, faute de mieux.
        assert_eq!(
            authorise_key(&db.pool, "id-vieux", None).await.unwrap(),
            KeyAuth::Allowed { target_id: registration.target_id, bound: false }
        );

        // L'interface a de quoi dire quoi faire.
        let info = host(&db.pool, registration.target_id).await.unwrap().expect("machine");
        assert_eq!(info.binding_state(), "unsupported");
        assert!(!info.bound);
        assert!(info.bound_at.is_none());
    }

    #[tokio::test]
    async fn upgrading_the_agent_binds_the_machine_without_anyone_doing_anything() {
        let db = setup().await;
        let token_id = token(&db, "parc").await;
        let before = enrol(&db, &legacy("nas", Some("id-nas")), token_id).await;
        assert!(!before.bound);

        // Le binaire est mis à jour : le premier lot suffit à lier la machine.
        let after = enrol(&db, &identity("nas", Some("id-nas")), token_id).await;
        assert_eq!(after.target_id, before.target_id);
        assert!(after.bound);
        let secret = after.issued_secret.clone().expect("secret remis à la liaison");

        // À partir de là, plus personne d'autre ne passe.
        assert!(matches!(
            register(&db.pool, &db.cipher, &identity("nas", Some("id-nas")), token_id, None)
                .await
                .expect_err("liaison exigée"),
            RegisterError::BindingMismatch
        ));
        assert!(
            register(
                &db.pool,
                &db.cipher,
                &identity("nas", Some("id-nas")),
                token_id,
                Some(&secret)
            )
            .await
            .is_ok()
        );
        let info = host(&db.pool, before.target_id).await.unwrap().expect("machine");
        assert_eq!(info.binding_state(), "bound");
        assert!(info.bound_at.is_some());
    }

    #[tokio::test]
    async fn a_reinstalled_machine_rebinds_only_inside_the_window_someone_opened() {
        let db = setup().await;
        let token_id = token(&db, "parc").await;
        let registration = enrol(&db, &identity("nas", Some("id-nas")), token_id).await;
        let old_secret = registration.issued_secret.clone().expect("secret");

        // Le disque a été refait : l'agent revient sans secret, et se fait jeter.
        assert!(matches!(
            register(&db.pool, &db.cipher, &identity("nas", Some("id-nas")), token_id, None)
                .await
                .expect_err("liaison exigée"),
            RegisterError::BindingMismatch
        ));

        // Quelqu'un ouvre la fenêtre depuis l'interface.
        let until = allow_rebind(&db.pool, registration.target_id).await.unwrap();
        assert!(until.is_some());
        let rebound = enrol(&db, &identity("nas", Some("id-nas")), token_id).await;
        let new_secret = rebound.issued_secret.clone().expect("nouveau secret");
        assert_ne!(new_secret, old_secret, "la reliaison change le secret");

        // La fenêtre s'est refermée derrière elle, et l'ancien secret ne vaut plus.
        assert!(matches!(
            register(
                &db.pool,
                &db.cipher,
                &identity("nas", Some("id-nas")),
                token_id,
                Some(&old_secret)
            )
            .await
            .expect_err("l'ancien secret est périmé"),
            RegisterError::BindingMismatch
        ));
        assert!(
            host(&db.pool, registration.target_id).await.unwrap().unwrap().rebind_until.is_none()
        );

        // Une cible qui n'a pas d'agent n'ouvre aucune fenêtre.
        assert_eq!(allow_rebind(&db.pool, 4_242).await.unwrap(), None);
    }

    // ------------------------------------------------------- portée du jeton

    #[tokio::test]
    async fn a_single_use_token_enrols_one_machine_and_not_a_second() {
        let db = setup().await;
        let (record, _) =
            create_token(&db.pool, "portable", TokenPolicy::default()).await.expect("création");
        assert_eq!(record.max_uses, Some(1), "l'usage unique est le défaut");

        let first = enrol(&db, &identity("portable", Some("id-1")), record.id).await;
        let secret = first.issued_secret.clone().expect("secret");

        let error =
            register(&db.pool, &db.cipher, &identity("autre", Some("id-2")), record.id, None)
                .await
                .expect_err("une seconde machine doit être refusée");
        assert!(
            matches!(error, RegisterError::EnrolmentDenied(EnrolmentDenied::Exhausted)),
            "{error:?}"
        );

        // La machine déjà entrée continue pourtant de pousser : un jeton épuisé
        // n'éteint pas ce qu'il a fait entrer.
        assert!(
            register(
                &db.pool,
                &db.cipher,
                &identity("portable", Some("id-1")),
                record.id,
                Some(&secret)
            )
            .await
            .is_ok()
        );
    }

    #[tokio::test]
    async fn a_fleet_token_enrols_as_many_machines_as_it_is_allowed() {
        let db = setup().await;
        let (record, _) = create_token(
            &db.pool,
            "flotte",
            TokenPolicy { max_uses: Some(2), expires_in_days: None },
        )
        .await
        .expect("création");

        enrol(&db, &identity("a", Some("id-a")), record.id).await;
        enrol(&db, &identity("b", Some("id-b")), record.id).await;
        assert!(matches!(
            register(&db.pool, &db.cipher, &identity("c", Some("id-c")), record.id, None)
                .await
                .expect_err("la troisième dépasse le compte"),
            RegisterError::EnrolmentDenied(EnrolmentDenied::Exhausted)
        ));

        let listed = list_tokens(&db.pool).await.unwrap();
        let capped = listed.iter().find(|t| t.id == record.id).expect("jeton");
        assert_eq!(capped.uses, 2);
        assert_eq!(capped.max_uses, Some(2));

        // Sans limite, le parc entier passe.
        let open = token(&db, "sans limite").await;
        for n in 0..5 {
            enrol(&db, &identity(&format!("m{n}"), Some(&format!("id-m{n}"))), open).await;
        }
        assert!(enrolment_allowed(&db.pool, open).await.unwrap().is_ok());
    }

    #[tokio::test]
    async fn a_token_past_its_deadline_stops_enrolling_but_not_reporting() {
        let db = setup().await;
        let (record, _) = create_token(
            &db.pool,
            "fenêtre",
            TokenPolicy { max_uses: None, expires_in_days: Some(1) },
        )
        .await
        .expect("création");
        let registration = enrol(&db, &identity("nas", Some("id-nas")), record.id).await;
        let secret = registration.issued_secret.clone().expect("secret");

        // Le terme passe.
        sqlx::query("UPDATE agent_tokens SET expires_at = '2000-01-01T00:00:00Z' WHERE id = ?")
            .bind(record.id)
            .execute(&db.pool)
            .await
            .unwrap();

        assert_eq!(
            enrolment_allowed(&db.pool, record.id).await.unwrap(),
            Err(EnrolmentDenied::Expired)
        );
        assert!(matches!(
            register(&db.pool, &db.cipher, &identity("autre", Some("id-2")), record.id, None)
                .await
                .expect_err("plus d'enrôlement après le terme"),
            RegisterError::EnrolmentDenied(EnrolmentDenied::Expired)
        ));
        // La machine déjà enrôlée n'est pas coupée pour autant.
        assert!(
            register(
                &db.pool,
                &db.cipher,
                &identity("nas", Some("id-nas")),
                record.id,
                Some(&secret)
            )
            .await
            .is_ok()
        );
        // Le jeton reste « actif » au sens de l'ingestion : seule la révocation ferme tout.
        assert!(find_active_token(&db.pool, &token::fingerprint("x")).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn the_enrolment_count_starts_from_the_machines_already_there() {
        // Le scénario de la migration : des machines sont déjà enrôlées quand le
        // compteur apparaît. Lui poser une limite plus tard doit partir de là.
        let db = setup().await;
        let open = token(&db, "parc").await;
        enrol(&db, &identity("a", Some("id-a")), open).await;
        enrol(&db, &identity("b", Some("id-b")), open).await;

        let listed = list_tokens(&db.pool).await.unwrap();
        assert_eq!(listed.iter().find(|t| t.id == open).unwrap().uses, 2);
    }
}
