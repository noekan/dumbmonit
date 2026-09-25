//! Réglages des sondes PostgreSQL et MySQL/MariaDB.

use std::time::Duration;

use dumbmonit_proto::{Credential, ProbeError, Target};

use super::verdict::Engine;
use crate::uptime::tags;

/// Requête exécutée par défaut.
///
/// Elle ne lit aucune table : un compte de supervision n'a donc besoin d'aucun
/// droit au-delà de la connexion, et la mesure reste valable sur une instance
/// dont le schéma change.
pub const DEFAULT_QUERY: &str = "SELECT 1";

/// Exigence de chiffrement, telle que l'utilisateur la choisit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SslMode {
    /// Jamais de TLS.
    Disable,
    /// TLS si le serveur le propose, sans vérifier la chaîne.
    Prefer,
    /// TLS obligatoire, chaîne non vérifiée.
    Require,
    /// TLS obligatoire, chaîne et nom vérifiés.
    VerifyFull,
}

impl SslMode {
    pub fn parse(raw: &str) -> Result<Self, ProbeError> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "disable" | "disabled" | "none" => Ok(Self::Disable),
            "prefer" | "preferred" => Ok(Self::Prefer),
            "require" | "required" => Ok(Self::Require),
            "verify-full" | "verify_full" | "verify-identity" => Ok(Self::VerifyFull),
            other => Err(ProbeError::Config(format!(
                "\"sslmode\" expects \"disable\", \"prefer\", \"require\" or \"verify-full\", \
                 got \"{other}\""
            ))),
        }
    }

    /// Vrai si le nom d'hôte doit être vérifié dans le certificat.
    ///
    /// C'est le seul cas où la connexion doit viser le nom et non l'adresse déjà
    /// filtrée par le garde-fou : vérifier un nom sur une adresse IP échouerait
    /// systématiquement.
    pub fn verifies_hostname(self) -> bool {
        self == Self::VerifyFull
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Disable => "disable",
            Self::Prefer => "prefer",
            Self::Require => "require",
            Self::VerifyFull => "verify-full",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Options {
    pub engine: Engine,
    pub host: String,
    pub port: u16,
    /// Base ouverte à la connexion. Vide : celle du moteur par défaut.
    pub database: Option<String>,
    pub username: String,
    pub password: String,
    pub ssl_mode: SslMode,
    pub query: String,
    /// Valeur attendue en première colonne de la première ligne.
    pub expect: Option<String>,
    pub allow_private: bool,
    pub timeout: Duration,
}

impl Options {
    pub fn from_target(engine: Engine, target: &Target) -> Result<Self, ProbeError> {
        let (host, port_in_address) = tags::split_host_port(&target.address, 0)?;
        if host.is_empty() {
            return Err(ProbeError::Config(format!(
                "the address must be the database server (for example \"db.home.lan\" or \
                 \"db.home.lan:{}\")",
                engine.default_port()
            )));
        }
        let port = match tags::parse_u32(target, "port", u32::from(port_in_address), 0..=65_535)? {
            0 => engine.default_port(),
            port => port as u16,
        };

        let (username, password) = match &target.credential {
            Credential::UsernamePassword { username, password } => {
                (username.clone(), password.clone())
            }
            Credential::None => {
                return Err(ProbeError::Config(
                    "a database check needs a user name and password: create a read-only \
                     account for monitoring rather than reusing an application one"
                        .to_string(),
                ));
            }
            _ => {
                return Err(ProbeError::Config(
                    "this check expects a user name and password".to_string(),
                ));
            }
        };

        let database = tags::tag(target, "database")
            .map(str::to_string)
            .or_else(|| engine.default_database().map(str::to_string));

        let query = tags::tag(target, "query").unwrap_or(DEFAULT_QUERY).to_string();

        Ok(Self {
            engine,
            host,
            port,
            database,
            username,
            password,
            ssl_mode: SslMode::parse(tags::tag(target, "sslmode").unwrap_or("prefer"))?,
            query,
            expect: tags::tag(target, "expect").map(str::to_string),
            allow_private: crate::uptime::guard::allowed(target)?,
            timeout: tags::parse_timeout(target, "timeout_seconds")?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::uptime::tags::test_support::cible;

    fn cible_avec_compte(kind: &str, address: &str, tags: &[(&str, &str)]) -> Target {
        let mut target = cible(kind, address, tags);
        target.credential =
            Credential::UsernamePassword { username: "monit".into(), password: "secret".into() };
        target
    }

    fn options(
        engine: Engine,
        address: &str,
        tags: &[(&str, &str)],
    ) -> Result<Options, ProbeError> {
        Options::from_target(engine, &cible_avec_compte(engine.kind(), address, tags))
    }

    #[test]
    fn chaque_moteur_a_son_port_et_sa_base_par_defaut() {
        let pg = options(Engine::Postgres, "db.lan", &[]).unwrap();
        assert_eq!(pg.port, 5432);
        assert_eq!(pg.database.as_deref(), Some("postgres"));

        let my = options(Engine::Mysql, "db.lan", &[]).unwrap();
        assert_eq!(my.port, 3306);
        assert_eq!(my.database, None, "MySQL n'exige pas qu'on nomme une base");
    }

    #[test]
    fn la_requete_par_defaut_ne_lit_aucune_table() {
        assert_eq!(options(Engine::Postgres, "db.lan", &[]).unwrap().query, "SELECT 1");
        let choisie =
            options(Engine::Mysql, "db.lan", &[("query", "SELECT count(*) FROM jobs")]).unwrap();
        assert_eq!(choisie.query, "SELECT count(*) FROM jobs");
    }

    #[test]
    fn le_port_de_ladresse_prime_sur_le_defaut() {
        assert_eq!(options(Engine::Postgres, "db.lan:6432", &[]).unwrap().port, 6432);
        assert_eq!(
            options(Engine::Postgres, "db.lan:6432", &[("port", "5433")]).unwrap().port,
            5433
        );
    }

    /// Une base de données ne se surveille pas anonymement : le dire au réglage
    /// vaut mieux qu'un échec toutes les minutes.
    #[test]
    fn labsence_didentifiants_est_une_erreur_de_configuration_explicite() {
        let error =
            Options::from_target(Engine::Postgres, &cible("postgres", "db.lan", &[])).unwrap_err();
        assert!(matches!(error, ProbeError::Config(_)));
        assert!(!error.means_down());
        assert!(error.to_string().contains("read-only"), "{error}");
    }

    #[test]
    fn les_modes_de_chiffrement_sont_ceux_des_moteurs() {
        for (saisie, attendu) in [
            ("disable", SslMode::Disable),
            ("prefer", SslMode::Prefer),
            ("require", SslMode::Require),
            ("verify-full", SslMode::VerifyFull),
            ("VERIFY_FULL", SslMode::VerifyFull),
        ] {
            assert_eq!(
                options(Engine::Postgres, "db.lan", &[("sslmode", saisie)]).unwrap().ssl_mode,
                attendu,
                "pour « {saisie} »"
            );
        }
        assert!(options(Engine::Postgres, "db.lan", &[("sslmode", "peut-être")]).is_err());
    }

    #[test]
    fn seul_le_mode_le_plus_strict_verifie_le_nom() {
        assert!(SslMode::VerifyFull.verifies_hostname());
        for mode in [SslMode::Disable, SslMode::Prefer, SslMode::Require] {
            assert!(!mode.verifies_hostname(), "{:?}", mode);
        }
    }

    #[test]
    fn une_adresse_vide_est_refusee_avant_tout_appel_reseau() {
        assert!(options(Engine::Mysql, "   ", &[]).is_err());
    }
}
