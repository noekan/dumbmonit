//! Format du lot de sauvegarde exportable.
//!
//! Un lot est un fichier JSON en deux parties :
//!
//! * une **enveloppe en clair** : le format, sa version, la date, la version du
//!   serveur qui l'a écrit, un décompte par section et les paramètres de
//!   dérivation de clé. Elle permet de savoir ce qu'on tient avant de connaître
//!   la phrase de passe, et de refuser tout de suite un lot venu d'une version
//!   plus récente ;
//! * une **charge utile chiffrée** : tout le reste, adresses comprises. Le lot
//!   contient les identifiants de chaque équipement : il n'y a rien dedans qu'on
//!   accepterait de laisser lisible.
//!
//! La clé vient de la phrase de passe par Argon2id, avec un sel tiré au hasard
//! pour chaque lot et des paramètres inscrits dans l'enveloppe. Le chiffrement
//! lui-même est l'AES-256-GCM de [`crate::crypto`] : une phrase de passe fausse
//! ou un octet modifié font échouer la vérification d'authenticité, jamais un
//! déchiffrement silencieusement faux.
//!
//! L'enveloppe n'est pas couverte par le tag GCM. Pour qu'on ne puisse pas la
//! réécrire (annoncer une autre version, d'autres paramètres), la charge utile
//! répète la version et la date : elles sont comparées après déchiffrement.

use std::collections::BTreeMap;

use anyhow::{Result, bail};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::crypto::Cipher;

/// Marqueur de format, vérifié avant toute autre chose.
pub const FORMAT: &str = "dumbmonit-backup";

/// Version du format écrite par ce serveur. Un lot d'une version supérieure est
/// refusé : il contiendrait des sections que nous ne saurions pas restaurer, et
/// les restaurer à moitié serait pire que de ne rien faire.
pub const VERSION: u32 = 1;

/// Longueur minimale de la phrase de passe.
///
/// Le lot contient tous les identifiants du parc et voyage hors de l'instance :
/// c'est la seule chose qui le protège. Seize caractères, c'est trois mots.
pub const MIN_PASSPHRASE_LEN: usize = 16;

/// Mémoire d'Argon2id, en kibioctets (32 Mio). Plus haut que le défaut de la
/// bibliothèque : cette dérivation est payée une fois par export et par
/// restauration, jamais dans le chemin d'une requête.
const KDF_M_COST: u32 = 32 * 1024;
const KDF_T_COST: u32 = 3;
const KDF_P_COST: u32 = 1;

/// Bornes acceptées à la lecture d'un lot : un fichier hostile pourrait sinon
/// annoncer des paramètres qui feraient réserver plusieurs gibioctets.
const MAX_M_COST: u32 = 1024 * 1024;
const MAX_T_COST: u32 = 16;
const MAX_P_COST: u32 = 8;

/// Paramètres de dérivation, recopiés dans l'enveloppe pour qu'un lot reste
/// lisible même si ce serveur change les siens plus tard.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Kdf {
    pub algorithm: String,
    /// Sel, encodé en base64.
    pub salt: String,
    pub m_cost: u32,
    pub t_cost: u32,
    pub p_cost: u32,
}

impl Kdf {
    fn new(salt: &[u8]) -> Self {
        Self {
            algorithm: "argon2id".to_string(),
            salt: BASE64.encode(salt),
            m_cost: KDF_M_COST,
            t_cost: KDF_T_COST,
            p_cost: KDF_P_COST,
        }
    }
}

/// L'enveloppe en clair : ce que l'on peut lire d'un lot sans sa phrase de passe.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Envelope {
    pub format: String,
    pub version: u32,
    pub created_at: String,
    /// Version de DumbMonit qui a écrit le lot, pour le diagnostic.
    pub source_version: String,
    /// Décompte par section (`targets`, `channels`, …). Des nombres seulement :
    /// de quoi dire « ce fichier contient douze équipements » avant de demander
    /// la phrase de passe, sans rien révéler de ce qu'ils sont.
    #[serde(default)]
    pub summary: BTreeMap<String, usize>,
    pub kdf: Kdf,
    pub cipher: String,
    /// Nonce et texte chiffré, encodés en base64.
    pub payload: String,
}

/// Ce que contient réellement un lot, une fois déchiffré.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Bundle {
    /// Répétée ici pour être vérifiée contre l'enveloppe après déchiffrement.
    pub version: u32,
    pub created_at: String,
    #[serde(default)]
    pub source_version: String,
    #[serde(default)]
    pub targets: Vec<BundleTarget>,
    #[serde(default)]
    pub rules: Vec<BundleRule>,
    #[serde(default)]
    pub rule_overrides: Vec<BundleOverride>,
    #[serde(default)]
    pub channels: Vec<BundleChannel>,
    /// Politique de notification globale (clé `notify_policy` de `settings`).
    #[serde(default)]
    pub notify_policy: Option<Value>,
    #[serde(default)]
    pub silences: Vec<BundleSilence>,
    #[serde(default)]
    pub status_pages: Vec<BundleStatusPage>,
    #[serde(default)]
    pub incidents: Vec<BundleIncident>,
    #[serde(default)]
    pub users: Vec<BundleUser>,
    #[serde(default)]
    pub agent_tokens: Vec<BundleAgentToken>,
    #[serde(default)]
    pub api_tokens: Vec<BundleApiToken>,
    #[serde(default)]
    pub push_monitors: Vec<BundlePushMonitor>,
}

impl Bundle {
    /// Décompte par section, tel qu'il apparaît en clair dans l'enveloppe.
    pub fn summary(&self) -> BTreeMap<String, usize> {
        BTreeMap::from([
            ("targets".to_string(), self.targets.len()),
            ("rules".to_string(), self.rules.len()),
            ("rule_overrides".to_string(), self.rule_overrides.len()),
            ("channels".to_string(), self.channels.len()),
            ("silences".to_string(), self.silences.len()),
            ("status_pages".to_string(), self.status_pages.len()),
            ("incidents".to_string(), self.incidents.len()),
            ("users".to_string(), self.users.len()),
            ("agent_tokens".to_string(), self.agent_tokens.len()),
            ("api_tokens".to_string(), self.api_tokens.len()),
            ("push_monitors".to_string(), self.push_monitors.len()),
        ])
    }
}

/// Désignation d'un équipement à l'intérieur du lot.
///
/// Les identifiants numériques d'une instance ne veulent rien dire dans une
/// autre : tout ce qui pointe vers un équipement — parent, agent relais,
/// silence, page de statut — le désigne par son couple type + adresse, qui est
/// déjà la clé unique de la table.
pub fn target_ref(kind: &str, address: &str) -> String {
    format!("{kind}|{address}")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BundleTarget {
    pub kind: String,
    pub address: String,
    pub name: String,
    #[serde(default)]
    pub profile_id: Option<String>,
    /// Référence de l'équipement parent (voir [`target_ref`]).
    #[serde(default)]
    pub parent: Option<String>,
    /// Référence de l'agent relais qui interroge cet équipement.
    #[serde(default)]
    pub via_agent: Option<String>,
    pub interval_secs: i64,
    pub enabled: bool,
    #[serde(default)]
    pub tags: BTreeMap<String, String>,
    /// `Credential` en clair : c'est le lot entier qui est chiffré.
    #[serde(default)]
    pub credential: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BundleRule {
    pub uid: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub kind: String,
    pub query: String,
    pub operator: String,
    pub threshold: f64,
    #[serde(default)]
    pub clear_threshold: Option<f64>,
    #[serde(default)]
    pub for_secs: i64,
    pub severity: String,
    /// Sélecteur de la règle, avec les équipements désignés par référence.
    #[serde(default)]
    pub selector: Value,
    /// Canaux imposés, par **nom** : les identifiants diffèrent d'une instance
    /// à l'autre. Vide signifie « tous les canaux actifs ».
    #[serde(default)]
    pub channels: Vec<String>,
    #[serde(default)]
    pub params: Value,
    #[serde(default)]
    pub unit: String,
    #[serde(default)]
    pub repeat_secs: Option<i64>,
    #[serde(default)]
    pub escalate_after_secs: Option<i64>,
    pub enabled: bool,
    #[serde(default)]
    pub builtin: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BundleOverride {
    pub rule_uid: String,
    pub target: String,
    #[serde(default)]
    pub threshold: Option<f64>,
    #[serde(default)]
    pub clear_threshold: Option<f64>,
    #[serde(default)]
    pub enabled: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BundleChannel {
    pub name: String,
    pub kind: String,
    pub enabled: bool,
    #[serde(default)]
    pub settings: Value,
    /// Secrets en clair dans la charge utile chiffrée : jeton de webhook, mot de
    /// passe SMTP. C'est ce qui permet au canal de fonctionner tout de suite
    /// après une restauration.
    #[serde(default)]
    pub secrets: Option<Value>,
    #[serde(default)]
    pub policy: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BundleSilence {
    pub name: String,
    #[serde(default)]
    pub comment: String,
    #[serde(default)]
    pub target: Option<String>,
    #[serde(default)]
    pub matchers: Value,
    pub schedule: Value,
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BundleStatusPage {
    pub slug: String,
    pub title: String,
    #[serde(default)]
    pub description: String,
    pub published: bool,
    pub theme: String,
    pub show_uptime_days: i64,
    #[serde(default)]
    pub items: Vec<BundleStatusItem>,
    /// Habillage (depuis la migration 0032) ; le logo, fichier sous `/data`, et
    /// les abonnés, données personnelles, ne voyagent pas dans une sauvegarde.
    #[serde(default = "default_accent")]
    pub accent: String,
    #[serde(default)]
    pub footer_text: String,
    #[serde(default)]
    pub homepage_url: String,
}

fn default_accent() -> String {
    "default".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BundleStatusItem {
    pub target: String,
    pub label: String,
    #[serde(default)]
    pub group_name: String,
    #[serde(default)]
    pub position: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BundleIncident {
    /// Page concernée, par son `slug`. `None` : l'annonce vaut pour toutes.
    #[serde(default)]
    pub page: Option<String>,
    pub title: String,
    pub kind: String,
    pub status: String,
    pub severity: String,
    pub starts_at: String,
    #[serde(default)]
    pub ends_at: Option<String>,
    #[serde(default)]
    pub updates: Vec<BundleIncidentUpdate>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BundleIncidentUpdate {
    pub status: String,
    pub body: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BundleUser {
    pub username: String,
    #[serde(default)]
    pub display_name: String,
    pub role: String,
    #[serde(default)]
    pub disabled: bool,
    #[serde(default)]
    pub oidc_subject: Option<String>,
    #[serde(default)]
    pub oidc_issuer: Option<String>,
    #[serde(default)]
    pub created_at: Option<String>,
    /// Empreinte Argon2 du mot de passe, présente seulement si l'opérateur a
    /// demandé les secrets de comptes à l'export.
    #[serde(default)]
    pub password_hash: Option<String>,
    /// Secret TOTP en clair, base64. Même condition que l'empreinte.
    #[serde(default)]
    pub totp_secret: Option<String>,
    #[serde(default)]
    pub totp_enabled: bool,
    /// Empreintes des codes de secours restants, base64.
    #[serde(default)]
    pub recovery_codes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BundleAgentToken {
    pub name: String,
    /// Empreinte SHA-256 hexadécimale : le jeton lui-même n'est stocké nulle
    /// part. La recopier suffit à ce que les agents déjà installés continuent
    /// d'être acceptés après une restauration.
    pub token_hash: String,
    pub prefix: String,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub revoked_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BundleApiToken {
    pub name: String,
    pub prefix: String,
    /// Empreinte SHA-256 en base64, pour la même raison.
    pub token_hash: String,
    pub scope: String,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub revoked_at: Option<String>,
    /// Compte propriétaire, par nom d'utilisateur.
    #[serde(default)]
    pub user: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BundlePushMonitor {
    pub target: String,
    /// Jeton en clair : c'est l'URL qu'une crontab appelle déjà. Sans lui, la
    /// restauration donnerait une nouvelle URL et tous les travaux surveillés
    /// seraient déclarés en panne le lendemain.
    pub token: String,
}

// --------------------------------------------------------------------------
// Chiffrement
// --------------------------------------------------------------------------

/// Vérifie une phrase de passe avant de s'en servir.
pub fn check_passphrase(passphrase: &str) -> Result<()> {
    if passphrase.chars().count() < MIN_PASSPHRASE_LEN {
        bail!(
            "The backup passphrase must be at least {MIN_PASSPHRASE_LEN} characters long. \
             It is the only thing protecting a file that holds every credential of this \
             instance — three words are enough."
        );
    }
    Ok(())
}

fn derive(passphrase: &str, kdf: &Kdf) -> Result<Cipher> {
    if kdf.algorithm != "argon2id" {
        bail!(
            "This backup uses an unknown key derivation ({}). It was not written by DumbMonit.",
            kdf.algorithm
        );
    }
    if kdf.m_cost > MAX_M_COST || kdf.t_cost > MAX_T_COST || kdf.p_cost > MAX_P_COST {
        bail!(
            "This backup asks for key derivation parameters far beyond anything DumbMonit \
             writes (memory {} KiB). Refusing to read it.",
            kdf.m_cost
        );
    }
    let salt = BASE64.decode(&kdf.salt).map_err(|_| {
        anyhow::anyhow!("The backup header is damaged: its salt is not valid base64.")
    })?;
    if salt.len() < 8 {
        bail!("The backup header is damaged: its salt is too short.");
    }

    let params = argon2::Params::new(kdf.m_cost, kdf.t_cost, kdf.p_cost, Some(32))
        .map_err(|e| anyhow::anyhow!("invalid Argon2id parameters: {e}"))?;
    let argon = argon2::Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params);
    let mut key = [0u8; 32];
    argon
        .hash_password_into(passphrase.as_bytes(), &salt, &mut key)
        .map_err(|e| anyhow::anyhow!("Argon2id key derivation failed: {e}"))?;
    Ok(Cipher::from_key(key))
}

/// Chiffre un lot avec la phrase de passe et rend l'enveloppe complète.
pub fn seal(bundle: &Bundle, passphrase: &str) -> Result<Envelope> {
    check_passphrase(passphrase)?;
    let salt = crate::crypto::generate_salt();
    let kdf = Kdf::new(&salt);
    let cipher = derive(passphrase, &kdf)?;
    let plain = serde_json::to_vec(bundle)?;
    let sealed = cipher.encrypt(&plain)?;

    Ok(Envelope {
        format: FORMAT.to_string(),
        version: bundle.version,
        created_at: bundle.created_at.clone(),
        source_version: bundle.source_version.clone(),
        summary: bundle.summary(),
        kdf,
        cipher: "aes-256-gcm".to_string(),
        payload: BASE64.encode(sealed),
    })
}

/// Vérifie l'enveloppe sans la déchiffrer : format, version, algorithme.
///
/// Séparé de [`open`] pour que l'interface puisse refuser un fichier qui n'est
/// pas un lot, ou qui vient d'une version plus récente, avant même de demander
/// la phrase de passe.
pub fn check_envelope(envelope: &Envelope) -> Result<()> {
    if envelope.format != FORMAT {
        bail!(
            "This file is not a DumbMonit backup (it declares the format \"{}\").",
            envelope.format
        );
    }
    if envelope.version > VERSION {
        bail!(
            "This backup was written in format version {} by a newer DumbMonit; this server \
             reads up to version {VERSION}. Upgrade DumbMonit, then restore it again.",
            envelope.version
        );
    }
    if envelope.version == 0 {
        bail!("This backup declares no format version: it is damaged.");
    }
    if envelope.cipher != "aes-256-gcm" {
        bail!(
            "This backup is encrypted with an algorithm this server does not know ({}).",
            envelope.cipher
        );
    }
    Ok(())
}

/// Déchiffre un lot. Échoue sur une phrase de passe fausse comme sur un fichier
/// modifié : l'AES-GCM ne distingue pas les deux, et le message le dit.
pub fn open(envelope: &Envelope, passphrase: &str) -> Result<Bundle> {
    check_envelope(envelope)?;
    let cipher = derive(passphrase, &envelope.kdf)?;
    let sealed = BASE64
        .decode(&envelope.payload)
        .map_err(|_| anyhow::anyhow!("The backup body is damaged: it is not valid base64."))?;

    let plain = cipher.decrypt(&sealed).map_err(|_| {
        anyhow::anyhow!(
            "Could not open this backup: the passphrase is wrong, or the file has been \
             modified since it was written."
        )
    })?;
    let bundle: Bundle = serde_json::from_slice(&plain).map_err(|error| {
        anyhow::anyhow!("The backup opened but its contents could not be read: {error}")
    })?;

    // L'enveloppe est en clair et n'est pas couverte par le tag GCM : on refuse
    // un fichier dont l'en-tête ne correspond pas à ce qu'il contient vraiment.
    if bundle.version != envelope.version || bundle.created_at != envelope.created_at {
        bail!("The header of this backup does not match its contents: it has been tampered with.");
    }
    Ok(bundle)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lot() -> Bundle {
        Bundle {
            version: VERSION,
            created_at: "2026-09-22 10:00:00".to_string(),
            source_version: "test".to_string(),
            targets: vec![BundleTarget {
                kind: "snmp".to_string(),
                address: "192.168.1.1".to_string(),
                name: "switch".to_string(),
                profile_id: None,
                parent: None,
                via_agent: None,
                interval_secs: 60,
                enabled: true,
                tags: BTreeMap::new(),
                credential: Some(serde_json::json!({"type": "snmp_v2c", "community": "s3cret"})),
            }],
            ..Bundle::default()
        }
    }

    const PHRASE: &str = "trois mots suffisent";

    #[test]
    fn un_aller_retour_rend_le_meme_lot() {
        let envelope = seal(&lot(), PHRASE).unwrap();
        let reopened = open(&envelope, PHRASE).unwrap();
        assert_eq!(reopened.targets.len(), 1);
        assert_eq!(reopened.targets[0].address, "192.168.1.1");
        assert_eq!(reopened.targets[0].credential.as_ref().unwrap()["community"], "s3cret");
    }

    #[test]
    fn le_secret_napparait_pas_en_clair_dans_le_fichier() {
        let envelope = seal(&lot(), PHRASE).unwrap();
        let written = serde_json::to_string(&envelope).unwrap();
        assert!(!written.contains("s3cret"), "le lot laisse fuir un identifiant");
        assert!(!written.contains("192.168.1.1"), "le lot laisse fuir une adresse");
        // L'enveloppe reste exploitable sans la phrase de passe : on sait ce
        // qu'on tient avant de la saisir.
        assert!(written.contains("\"targets\":1"));
    }

    #[test]
    fn une_phrase_de_passe_fausse_est_refusee() {
        let envelope = seal(&lot(), PHRASE).unwrap();
        let error = open(&envelope, "une autre phrase longue").unwrap_err().to_string();
        assert!(error.contains("passphrase is wrong"), "{error}");
    }

    #[test]
    fn une_phrase_de_passe_trop_courte_est_refusee() {
        let error = seal(&lot(), "court").unwrap_err().to_string();
        assert!(error.contains("at least"), "{error}");
    }

    #[test]
    fn un_lot_modifie_est_refuse() {
        let mut envelope = seal(&lot(), PHRASE).unwrap();
        let mut bytes = BASE64.decode(&envelope.payload).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 0x01;
        envelope.payload = BASE64.encode(&bytes);
        assert!(open(&envelope, PHRASE).is_err());
    }

    #[test]
    fn une_enveloppe_reecrite_est_refusee() {
        let mut envelope = seal(&lot(), PHRASE).unwrap();
        envelope.created_at = "1999-01-01 00:00:00".to_string();
        let error = open(&envelope, PHRASE).unwrap_err().to_string();
        assert!(error.contains("tampered"), "{error}");
    }

    #[test]
    fn un_lot_plus_recent_est_refuse_avec_une_phrase_claire() {
        let mut envelope = seal(&lot(), PHRASE).unwrap();
        envelope.version = VERSION + 1;
        let error = open(&envelope, PHRASE).unwrap_err().to_string();
        assert!(error.contains("newer DumbMonit"), "{error}");
        assert!(error.contains("Upgrade DumbMonit"), "{error}");
    }

    #[test]
    fn un_fichier_qui_nest_pas_un_lot_est_refuse() {
        let mut envelope = seal(&lot(), PHRASE).unwrap();
        envelope.format = "something-else".to_string();
        let error = open(&envelope, PHRASE).unwrap_err().to_string();
        assert!(error.contains("not a DumbMonit backup"), "{error}");
    }

    #[test]
    fn des_parametres_de_derivation_absurdes_sont_refuses() {
        let mut envelope = seal(&lot(), PHRASE).unwrap();
        envelope.kdf.m_cost = MAX_M_COST + 1;
        assert!(open(&envelope, PHRASE).is_err());
    }
}
