//! Traduction des erreurs d'une base de données en raison d'échec, et lecture de
//! la première valeur renvoyée.
//!
//! Module purement fonctionnel : il ne connaît ni socket ni pilote, seulement des
//! codes et des messages. Toute la logique de verdict est donc vérifiable sans
//! serveur de base de données en face.

use crate::uptime::outcome::Failure;

/// Codes `SQLSTATE` de PostgreSQL qui désignent un refus d'identifiants.
///
/// `28P01` est le mot de passe faux, `28000` le compte inexistant ou refusé par
/// `pg_hba.conf`. Les deux se corrigent dans le compte de supervision, pas dans
/// la machine : les confondre avec une panne réveillerait quelqu'un pour rien.
const POSTGRES_AUTH: &[&str] = &["28000", "28P01"];

/// Codes d'erreur MySQL/MariaDB qui désignent un refus d'identifiants.
///
/// `1045` est l'accès refusé, `1044` l'accès refusé à une base précise, `1698`
/// le compte qui n'accepte que l'authentification par socket du système.
const MYSQL_AUTH: &[&str] = &["1045", "1044", "1698", "1251"];

/// Moteur interrogé.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Engine {
    Postgres,
    Mysql,
}

impl Engine {
    pub fn kind(self) -> &'static str {
        match self {
            Self::Postgres => "postgres",
            Self::Mysql => "mysql",
        }
    }

    pub fn default_port(self) -> u16 {
        match self {
            Self::Postgres => 5432,
            Self::Mysql => 3306,
        }
    }

    /// Base ouverte par défaut à la connexion.
    ///
    /// PostgreSQL exige qu'on en nomme une ; MySQL s'en passe très bien, et ne
    /// pas en imposer évite d'échouer sur une instance où `mysql` est fermée au
    /// compte de supervision.
    pub fn default_database(self) -> Option<&'static str> {
        match self {
            Self::Postgres => Some("postgres"),
            Self::Mysql => None,
        }
    }

    fn auth_codes(self) -> &'static [&'static str] {
        match self {
            Self::Postgres => POSTGRES_AUTH,
            Self::Mysql => MYSQL_AUTH,
        }
    }
}

/// Étape du dialogue où l'erreur est survenue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// Ouverture de la connexion et authentification.
    Connect,
    /// Exécution de la requête, connexion déjà établie.
    Query,
}

/// Traduit une erreur de pilote en raison exposée en métrique.
///
/// `code` est le `SQLSTATE` de PostgreSQL ou le numéro d'erreur de MySQL, quand
/// le serveur en a renvoyé un ; `message` est le texte brut. L'ordre compte : un
/// code fait foi, le texte n'est consulté qu'à défaut.
pub fn classify(engine: Engine, stage: Stage, code: Option<&str>, message: &str) -> Failure {
    if let Some(code) = code
        && engine.auth_codes().contains(&code)
    {
        return Failure::Auth;
    }

    let lower = message.to_ascii_lowercase();
    // Un serveur qui refuse le mot de passe n'envoie pas toujours de code : le
    // pilote peut échouer avant, pendant la négociation d'authentification.
    if lower.contains("password authentication failed")
        || lower.contains("access denied for user")
        || lower.contains("authentication plugin")
        || lower.contains("no pg_hba.conf entry")
    {
        return Failure::Auth;
    }
    if lower.contains("certificate") || lower.contains("tls") || lower.contains("ssl") {
        return Failure::Tls;
    }
    if lower.contains("timed out") || lower.contains("timeout") {
        return Failure::Timeout;
    }
    if lower.contains("connection refused")
        || lower.contains("connection reset")
        || lower.contains("no route to host")
        || lower.contains("broken pipe")
        || lower.contains("unexpected end of file")
    {
        return Failure::Connect;
    }

    match stage {
        // Tout ce qui empêche la connexion sans être un refus d'identifiants est
        // une indisponibilité : base en récupération, trop de connexions, port
        // ouvert par autre chose.
        Stage::Connect => Failure::Connect,
        Stage::Query => Failure::Query,
    }
}

/// Valeur scalaire lue dans la première colonne de la première ligne.
///
/// Le type varie d'un moteur et d'une requête à l'autre : `SELECT 1` rend un
/// entier ici, un décimal là, et `SELECT now()` une chaîne. La sonde retient la
/// forme textuelle pour la comparaison et, quand c'est possible, la forme
/// numérique pour la courbe.
#[derive(Debug, Clone, PartialEq)]
pub struct Scalar {
    pub text: String,
    pub number: Option<f64>,
}

impl Scalar {
    pub fn from_text(text: impl Into<String>) -> Self {
        let text = text.into();
        let number = text.trim().parse::<f64>().ok();
        Self { text, number }
    }

    /// Vrai si la valeur correspond à l'attente.
    ///
    /// La comparaison est textuelle et insensible aux espaces de bord : exiger
    /// une égalité de type obligerait l'utilisateur à savoir si son moteur rend
    /// `1` en `int4` ou en `bigint`, ce dont il n'a que faire.
    pub fn matches(&self, expected: &str) -> bool {
        let expected = expected.trim();
        if self.text.trim() == expected {
            return true;
        }
        // Deux nombres égaux écrits différemment (`1` et `1.0`) sont égaux.
        match (self.number, expected.parse::<f64>()) {
            (Some(left), Ok(right)) => (left - right).abs() < f64::EPSILON,
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn un_mot_de_passe_refuse_nest_pas_une_panne_de_serveur() {
        assert_eq!(
            classify(Engine::Postgres, Stage::Connect, Some("28P01"), "whatever"),
            Failure::Auth
        );
        assert_eq!(
            classify(Engine::Mysql, Stage::Connect, Some("1045"), "whatever"),
            Failure::Auth
        );
        // Sans code : le texte du pilote suffit.
        assert_eq!(
            classify(
                Engine::Postgres,
                Stage::Connect,
                None,
                "password authentication failed for user \"monit\""
            ),
            Failure::Auth
        );
        assert_eq!(
            classify(
                Engine::Mysql,
                Stage::Connect,
                None,
                "Access denied for user 'monit'@'10.0.0.2'"
            ),
            Failure::Auth
        );
    }

    /// Le code d'un moteur ne doit pas être lu avec le barème de l'autre : `1045`
    /// n'est pas un SQLSTATE PostgreSQL.
    #[test]
    fn les_codes_dun_moteur_ne_valent_pas_pour_lautre() {
        assert_ne!(
            classify(Engine::Postgres, Stage::Query, Some("1045"), "syntax error"),
            Failure::Auth
        );
        assert_ne!(
            classify(Engine::Mysql, Stage::Query, Some("28P01"), "syntax error"),
            Failure::Auth
        );
    }

    /// Le signal que la sonde existe pour donner : la base répond, s'authentifie,
    /// et la requête échoue quand même.
    #[test]
    fn une_requete_refusee_par_une_base_joignable_est_une_erreur_de_requete() {
        assert_eq!(
            classify(Engine::Postgres, Stage::Query, Some("42P01"), "relation does not exist"),
            Failure::Query
        );
        assert_eq!(
            classify(Engine::Mysql, Stage::Query, Some("1146"), "table doesn't exist"),
            Failure::Query
        );
    }

    #[test]
    fn un_port_ferme_et_un_delai_depasse_ne_se_confondent_pas() {
        assert_eq!(
            classify(Engine::Postgres, Stage::Connect, None, "Connection refused (os error 111)"),
            Failure::Connect
        );
        assert_eq!(
            classify(Engine::Postgres, Stage::Connect, None, "operation timed out"),
            Failure::Timeout
        );
    }

    #[test]
    fn un_refus_de_chiffrement_se_voit_comme_tel() {
        assert_eq!(
            classify(Engine::Postgres, Stage::Connect, None, "invalid peer certificate"),
            Failure::Tls
        );
        assert_eq!(
            classify(Engine::Mysql, Stage::Connect, None, "server does not support TLS"),
            Failure::Tls
        );
    }

    #[test]
    fn une_panne_sans_explication_reste_une_panne_a_letape_de_connexion() {
        assert_eq!(
            classify(
                Engine::Postgres,
                Stage::Connect,
                Some("57P03"),
                "the database is starting up"
            ),
            Failure::Connect
        );
    }

    #[test]
    fn les_moteurs_annoncent_leur_port_et_leur_base_par_defaut() {
        assert_eq!(Engine::Postgres.default_port(), 5432);
        assert_eq!(Engine::Mysql.default_port(), 3306);
        assert_eq!(Engine::Postgres.default_database(), Some("postgres"));
        assert_eq!(Engine::Mysql.default_database(), None);
        assert_eq!(Engine::Postgres.kind(), "postgres");
        assert_eq!(Engine::Mysql.kind(), "mysql");
    }

    #[test]
    fn une_valeur_numerique_devient_une_courbe() {
        let scalar = Scalar::from_text("42");
        assert_eq!(scalar.number, Some(42.0));
        let texte = Scalar::from_text("2026-09-24 10:00:00");
        assert_eq!(texte.number, None);
    }

    #[test]
    fn la_comparaison_ne_depend_pas_de_lecriture_du_nombre() {
        assert!(Scalar::from_text("1").matches("1"));
        assert!(Scalar::from_text("1").matches(" 1 "));
        assert!(Scalar::from_text("1.0").matches("1"), "le moteur choisit son type, pas nous");
        assert!(Scalar::from_text("ok").matches("ok"));
        assert!(!Scalar::from_text("2").matches("1"));
        assert!(!Scalar::from_text("ok").matches("OK"), "le texte reste sensible à la casse");
    }
}
