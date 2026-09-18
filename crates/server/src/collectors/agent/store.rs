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

/// Un jeton, tel qu'il peut être montré. Jamais le secret lui-même.
#[derive(Debug, Clone)]
pub struct TokenRecord {
    pub id: i64,
    pub name: String,
    pub prefix: String,
    pub created_at: String,
    pub last_used_at: Option<String>,
    pub revoked_at: Option<String>,
}

/// Crée un jeton et renvoie sa forme en clair — la seule et unique fois.
pub async fn create_token(pool: &SqlitePool, name: &str) -> Result<(TokenRecord, String)> {
    let clear = token::generate();
    let row = sqlx::query(
        "INSERT INTO agent_tokens (name, token_hash, prefix)
         VALUES (?, ?, ?)
         RETURNING id, name, prefix, created_at, last_used_at, revoked_at",
    )
    .bind(name)
    .bind(token::fingerprint(&clear))
    .bind(token::display_prefix(&clear))
    .fetch_one(pool)
    .await
    .context("creating the enrollment token")?;

    Ok((row_to_token(&row)?, clear))
}

pub async fn list_tokens(pool: &SqlitePool) -> Result<Vec<TokenRecord>> {
    let rows = sqlx::query(
        "SELECT id, name, prefix, created_at, last_used_at, revoked_at
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
pub async fn find_active_token(pool: &SqlitePool, fingerprint: &str) -> Result<Option<i64>> {
    let row =
        sqlx::query("SELECT id FROM agent_tokens WHERE token_hash = ? AND revoked_at IS NULL")
            .bind(fingerprint)
            .fetch_optional(pool)
            .await
            .context("checking the enrollment token")?;
    row.map(|row| row.try_get("id")).transpose().context("token id")
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Registration {
    pub target_id: TargetId,
    /// Vrai si la cible vient d'être créée par ce lot.
    pub created: bool,
}

/// Retrouve la cible d'un agent, ou l'enregistre si elle n'existe pas encore.
///
/// C'est ce qui rend l'installation en une commande possible : la machine se
/// présente avec un jeton valide, et devient une cible sans que personne n'ait
/// rien saisi dans l'interface.
pub async fn register(
    pool: &SqlitePool,
    cipher: &Cipher,
    identity: &AgentIdentity,
    token_id: i64,
) -> Result<Registration> {
    let key = identity.key();

    if let Some(target_id) = find_by_key(pool, &key).await? {
        update_host(pool, target_id, identity).await?;
        return Ok(Registration { target_id, created: false });
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
                // Aucun secret à conserver : l'authentification se fait par jeton,
                // à chaque lot, et le serveur n'a jamais à se connecter à l'agent.
                credential: Some(Credential::None),
            };
            (db::targets::create(pool, cipher, &input).await?, true)
        }
    };

    insert_host(pool, target_id, &key, identity, token_id).await?;
    Ok(Registration { target_id, created })
}

async fn find_by_key(pool: &SqlitePool, key: &str) -> Result<Option<TargetId>> {
    let row = sqlx::query("SELECT target_id FROM agent_hosts WHERE agent_key = ?")
        .bind(key)
        .fetch_optional(pool)
        .await
        .context("looking up the machine")?;
    row.map(|row| row.try_get("target_id")).transpose().context("target id")
}

async fn insert_host(
    pool: &SqlitePool,
    target_id: TargetId,
    key: &str,
    identity: &AgentIdentity,
    token_id: i64,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO agent_hosts
             (target_id, agent_key, hostname, os, os_version, kernel_version, arch,
              agent_version, commands_enabled, relay, site, token_id)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
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
             agent_version = ?, commands_enabled = ?, relay = ?, site = ?
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
}

impl HostInfo {
    /// Vrai seulement si l'agent a dit qu'il exécute les commandes : dans le
    /// doute, l'interface ne propose pas une action qui n'aboutirait jamais.
    pub fn commands_supported(&self) -> bool {
        self.commands_enabled == Some(true)
    }
}

/// Description de la machine rattachée à une cible, si un agent s'y est présenté.
pub async fn host(pool: &SqlitePool, target_id: TargetId) -> Result<Option<HostInfo>> {
    let row = sqlx::query(
        "SELECT hostname, os, os_version, arch, agent_version, commands_enabled, relay, site,
                last_seen_at
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
                h.commands_enabled, h.relay, h.site, h.last_seen_at
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
        }
    }

    #[tokio::test]
    async fn a_created_token_can_be_verified_then_revoked() {
        let db = setup().await;
        let (record, clear) = create_token(&db.pool, "parc").await.expect("création");

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
        let (_, clear) = create_token(&db.pool, "parc").await.expect("création");

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
        create_token(&db.pool, "parc").await.expect("création");
        assert_eq!(
            find_active_token(&db.pool, &token::fingerprint("dmon_inconnu")).await.unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn an_unknown_machine_registers_itself() {
        let db = setup().await;
        let (token_record, _) = create_token(&db.pool, "parc").await.expect("création");

        let first =
            register(&db.pool, &db.cipher, &identity("nas", Some("id-nas")), token_record.id)
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
        let (token_record, _) = create_token(&db.pool, "parc").await.expect("création");
        let identity = identity("nas", Some("id-nas"));

        let first = register(&db.pool, &db.cipher, &identity, token_record.id).await.unwrap();
        let second = register(&db.pool, &db.cipher, &identity, token_record.id).await.unwrap();

        assert_eq!(first.target_id, second.target_id);
        assert!(!second.created);
        assert_eq!(db::targets::list(&db.pool, &db.cipher).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn renaming_a_machine_does_not_create_a_second_target() {
        let db = setup().await;
        let (token_record, _) = create_token(&db.pool, "parc").await.expect("création");

        let before =
            register(&db.pool, &db.cipher, &identity("nas", Some("id-stable")), token_record.id)
                .await
                .unwrap();
        // Même identifiant machine, nom d'hôte différent : c'est la même machine.
        let after = register(
            &db.pool,
            &db.cipher,
            &identity("nas-cave", Some("id-stable")),
            token_record.id,
        )
        .await
        .unwrap();

        assert_eq!(before.target_id, after.target_id);
        assert_eq!(db::targets::list(&db.pool, &db.cipher).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn without_a_machine_id_the_hostname_identifies_the_machine() {
        let db = setup().await;
        let (token_record, _) = create_token(&db.pool, "parc").await.expect("création");

        let first = register(&db.pool, &db.cipher, &identity("Pi-Salon", None), token_record.id)
            .await
            .unwrap();
        // La casse ne doit pas suffire à dédoubler une machine.
        let second = register(&db.pool, &db.cipher, &identity("pi-salon", None), token_record.id)
            .await
            .unwrap();

        assert_eq!(first.target_id, second.target_id);
    }

    #[tokio::test]
    async fn the_freshness_of_a_machine_is_recorded_at_each_batch() {
        let db = setup().await;
        let (token_record, _) = create_token(&db.pool, "parc").await.expect("création");
        let registration =
            register(&db.pool, &db.cipher, &identity("nas", Some("id-nas")), token_record.id)
                .await
                .unwrap();

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
        let (token_record, _) = create_token(&db.pool, "parc").await.expect("création");
        // Un agent d'avant le canal de commandes ne dit rien : il n'est pas
        // réputé capable pour autant.
        let mut identity = identity("nas", Some("id-nas"));
        identity.commands_enabled = None;
        let registration =
            register(&db.pool, &db.cipher, &identity, token_record.id).await.unwrap();
        let info = host(&db.pool, registration.target_id).await.unwrap().expect("machine");
        assert_eq!(info.commands_enabled, None);
        assert!(!info.commands_supported());
        assert_eq!(info.agent_version, "0.1.0");

        // Après mise à jour de l'agent, le lot suivant suffit.
        identity.commands_enabled = Some(true);
        identity.agent_version = "0.2.0".into();
        register(&db.pool, &db.cipher, &identity, token_record.id).await.unwrap();
        let info = host(&db.pool, registration.target_id).await.unwrap().expect("machine");
        assert!(info.commands_supported());
        assert_eq!(info.agent_version, "0.2.0");

        // `commands: false` dans la configuration de l'agent.
        identity.commands_enabled = Some(false);
        register(&db.pool, &db.cipher, &identity, token_record.id).await.unwrap();
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
        let (token_record, _) = create_token(&db.pool, "parc").await.expect("création");

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

        let registration =
            register(&db.pool, &db.cipher, &identity("nas", Some("id-nas")), token_record.id)
                .await
                .unwrap();

        assert_eq!(registration.target_id, manual);
        assert!(!registration.created);
        // Le nom choisi par l'utilisateur est préservé : c'est le sien, pas celui
        // que la machine se donne.
        let target = db::targets::get(&db.pool, &db.cipher, manual).await.unwrap().unwrap();
        assert_eq!(target.name, "Serveur de sauvegarde");
    }
}
