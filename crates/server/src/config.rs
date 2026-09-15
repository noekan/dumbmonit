use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};

use crate::auth::oidc::OidcEnv;

/// Configuration du serveur, entièrement pilotée par variables d'environnement —
/// c'est ce qui rend le déploiement Docker trivial : pas de fichier à monter.
#[derive(Debug, Clone)]
pub struct Config {
    /// Adresse d'écoute de l'API et de l'interface web.
    pub bind: SocketAddr,
    /// Répertoire persistant : base SQLite et clé de chiffrement.
    pub data_dir: PathBuf,
    /// URL de base de VictoriaMetrics.
    pub victoria_url: String,
    /// Secret d'instance, fourni par l'environnement ou lu depuis `data_dir`.
    /// `None` ici signifie « à charger ou générer au démarrage ».
    pub secret: Option<String>,
    /// Nombre maximal d'interrogations simultanées, tous collecteurs confondus.
    pub max_concurrent_probes: usize,
    /// Délai au-delà duquel une interrogation est abandonnée.
    pub probe_timeout: Duration,
    /// Période de vidage du tampon d'écriture vers VictoriaMetrics.
    pub write_flush_interval: Duration,
    /// Taille de lot qui déclenche un envoi immédiat vers VictoriaMetrics, sans
    /// attendre l'échéance.
    pub write_flush_size: usize,
    /// Threads de travail du runtime Tokio.
    ///
    /// Tokio en crée un par cœur par défaut : sur un NAS à seize cœurs, ce sont
    /// seize piles et seize files locales pour un serveur qui passe l'essentiel de
    /// son temps à attendre le réseau. Quatre suffisent largement à un homelab ;
    /// la valeur reste réglable pour une instance qui interroge des centaines de
    /// cibles.
    pub workers: usize,
    /// Nombre maximal de connexions SQLite ouvertes en parallèle.
    ///
    /// Chaque connexion coûte un thread `sqlx` et son cache de pages : quatre
    /// suffisent à l'interface et au planificateur d'un homelab, la base ne
    /// servant qu'à la configuration et aux états d'alerte.
    pub db_pool_size: u32,
    /// Répertoire des binaires de l'agent servis sous `/download/`.
    ///
    /// Rempli à la construction de l'image ; s'il est vide ou absent, la commande
    /// d'installation échoue proprement avec un 404, sans empêcher le serveur de
    /// tourner.
    pub agent_dir: PathBuf,
    /// Réinitialisation des comptes demandée par l'environnement.
    ///
    /// Le seul moyen de reprendre la main sur une instance dont plus personne n'a
    /// le mot de passe : il n'y a pas de courriel de récupération. Le serveur
    /// efface tous les comptes et toutes les sessions au démarrage (les réglages
    /// SSO restent), puis l'interface repropose l'écran de première configuration.
    pub reset_password: bool,
    /// Connexion OpenID Connect décrite par l'environnement (`EZYMONIT_OIDC_*`,
    /// `EZYMONIT_PUBLIC_URL`). Un réglage enregistré depuis l'interface l'emporte.
    pub oidc: OidcEnv,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        Ok(Self {
            bind: env_parsed("EZYMONIT_BIND", "0.0.0.0:8080")?,
            data_dir: PathBuf::from(env_or("EZYMONIT_DATA_DIR", "/data")),
            victoria_url: env_or("EZYMONIT_VM_URL", "http://victoriametrics:8428")
                .trim_end_matches('/')
                .to_string(),
            secret: std::env::var("EZYMONIT_SECRET").ok().filter(|s| !s.is_empty()),
            max_concurrent_probes: env_parsed("EZYMONIT_MAX_CONCURRENT_PROBES", "64")?,
            probe_timeout: Duration::from_secs(env_parsed("EZYMONIT_PROBE_TIMEOUT_SECS", "10")?),
            write_flush_interval: Duration::from_secs(env_parsed(
                "EZYMONIT_FLUSH_INTERVAL_SECS",
                "5",
            )?),
            write_flush_size: env_parsed("EZYMONIT_FLUSH_BATCH", "5000")?,
            workers: env_parsed::<usize>("EZYMONIT_WORKERS", &default_workers().to_string())?
                .clamp(1, 256),
            db_pool_size: env_parsed::<u32>("EZYMONIT_DB_POOL", "4")?.clamp(1, 64),
            agent_dir: PathBuf::from(env_or("EZYMONIT_AGENT_DIR", "/agents")),
            reset_password: env_flag("EZYMONIT_RESET_PASSWORD"),
            oidc: OidcEnv::from_env(),
        })
    }

    pub fn database_path(&self) -> PathBuf {
        self.data_dir.join("ezymonit.db")
    }

    pub fn secret_path(&self) -> PathBuf {
        self.data_dir.join("secret.key")
    }
}

/// Threads de travail par défaut : au plus quatre, et jamais plus que de cœurs.
fn default_workers() -> usize {
    std::thread::available_parallelism().map_or(2, |n| n.get()).min(4)
}

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).ok().filter(|v| !v.is_empty()).unwrap_or_else(|| default.to_string())
}

/// Drapeau booléen : `1`, `true`, `yes` ou `on`, sans distinction de casse.
fn env_flag(key: &str) -> bool {
    std::env::var(key)
        .map(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"))
        .unwrap_or(false)
}

fn env_parsed<T>(key: &str, default: &str) -> Result<T>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    let raw = env_or(key, default);
    raw.parse::<T>()
        .map_err(|e| anyhow::anyhow!("{e}"))
        .with_context(|| format!("{key}: invalid value \"{raw}\""))
}
