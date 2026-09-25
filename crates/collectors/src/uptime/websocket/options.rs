//! Réglages de la sonde WebSocket.

use std::time::Duration;

use dumbmonit_proto::{Credential, ProbeError, Target};

use crate::uptime::tags;

#[derive(Debug, Clone)]
pub struct Options {
    pub host: String,
    pub port: u16,
    /// Vrai pour `wss://`.
    pub tls: bool,
    /// Chemin de la requête d'ouverture, chaîne de requête comprise.
    pub path: String,
    pub server_name: String,
    pub allow_untrusted: bool,
    pub allow_private: bool,
    /// Valeur de l'en-tête `Origin`, exigée par certains serveurs.
    pub origin: Option<String>,
    /// Sous-protocole demandé (`Sec-WebSocket-Protocol`).
    pub subprotocol: Option<String>,
    /// Trame de texte envoyée juste après l'ouverture.
    pub send: Option<String>,
    /// Texte qu'une trame reçue doit contenir. Vide et sans `send` : la seule
    /// poignée de main suffit.
    pub expect: Option<String>,
    /// En-tête `Authorization`, quand la cible porte des identifiants.
    pub authorization: Option<String>,
    pub timeout: Duration,
}

impl Options {
    pub fn from_target(target: &Target) -> Result<Self, ProbeError> {
        let (tls, rest) = split_scheme(&target.address)?;
        let (authority, path) = match rest.find('/') {
            Some(index) => (&rest[..index], rest[index..].to_string()),
            None => (rest.as_str(), "/".to_string()),
        };
        let default_port = if tls { 443 } else { 80 };
        let (host, port_in_address) = tags::split_host_port(authority, 0)?;
        if host.is_empty() {
            return Err(ProbeError::Config(
                "the address must be the WebSocket endpoint (for example \
                 \"wss://home.example.com/api/websocket\")"
                    .to_string(),
            ));
        }
        let port = match tags::parse_u32(target, "port", u32::from(port_in_address), 0..=65_535)? {
            0 => default_port,
            port => port as u16,
        };

        let authorization = match &target.credential {
            Credential::None => None,
            Credential::ApiToken { token } => Some(format!("Bearer {token}")),
            Credential::UsernamePassword { username, password } => {
                use base64::Engine;
                Some(format!(
                    "Basic {}",
                    base64::engine::general_purpose::STANDARD
                        .encode(format!("{username}:{password}"))
                ))
            }
            _ => {
                return Err(ProbeError::Config(
                    "this check accepts no credential, a user name and password, or a token"
                        .to_string(),
                ));
            }
        };

        Ok(Self {
            server_name: tags::tag(target, "server_name")
                .map(str::to_string)
                .unwrap_or_else(|| host.clone()),
            host,
            port,
            tls,
            path: tags::tag(target, "path").map(str::to_string).unwrap_or(path),
            allow_untrusted: tags::parse_bool(target, "insecure_tls", false)?,
            allow_private: crate::uptime::guard::allowed(target)?,
            origin: tags::tag(target, "origin").map(str::to_string),
            subprotocol: tags::tag(target, "subprotocol").map(str::to_string),
            send: tags::tag(target, "send").map(str::to_string),
            expect: tags::tag(target, "expect").map(str::to_string),
            authorization,
            timeout: tags::parse_timeout(target, "timeout_seconds")?,
        })
    }

    /// Valeur de l'en-tête `Host`, port compris quand il n'est pas celui du schéma.
    pub fn host_header(&self) -> String {
        let implicit = if self.tls { 443 } else { 80 };
        if self.port == implicit {
            self.host.clone()
        } else if self.host.contains(':') {
            format!("[{}]:{}", self.host, self.port)
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }

    /// Adresse affichée en étiquette. Cardinalité : une valeur par cible.
    pub fn url(&self) -> String {
        let scheme = if self.tls { "wss" } else { "ws" };
        format!("{scheme}://{}{}", self.host_header(), self.path)
    }
}

/// Sépare le schéma de l'adresse. `wss` par défaut, comme HTTPS l'est pour la
/// sonde web : une adresse en clair doit se demander, pas s'obtenir par oubli.
fn split_scheme(address: &str) -> Result<(bool, String), ProbeError> {
    let trimmed = address.trim();
    if trimmed.is_empty() {
        return Err(ProbeError::Config(
            "the address must be the WebSocket endpoint (for example \
             \"wss://home.example.com/api/websocket\")"
                .to_string(),
        ));
    }
    let lower = trimmed.to_ascii_lowercase();
    for (prefix, tls) in
        [("wss://", true), ("ws://", false), ("https://", true), ("http://", false)]
    {
        if lower.starts_with(prefix) {
            return Ok((tls, trimmed[prefix.len()..].to_string()));
        }
    }
    if lower.contains("://") {
        return Err(ProbeError::Config(format!(
            "unsupported scheme in \"{address}\": write \"wss://\" or \"ws://\""
        )));
    }
    Ok((true, trimmed.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::uptime::tags::test_support::cible;

    fn options(address: &str, tags: &[(&str, &str)]) -> Result<Options, ProbeError> {
        Options::from_target(&cible("websocket", address, tags))
    }

    #[test]
    fn une_adresse_complete_se_decompose() {
        let options = options("wss://home.exemple.fr/api/websocket", &[]).unwrap();
        assert_eq!(options.host, "home.exemple.fr");
        assert_eq!(options.port, 443);
        assert!(options.tls);
        assert_eq!(options.path, "/api/websocket");
        assert_eq!(options.url(), "wss://home.exemple.fr/api/websocket");
    }

    #[test]
    fn le_chiffrement_est_le_defaut_comme_pour_la_sonde_web() {
        let options = options("home.exemple.fr/ws", &[]).unwrap();
        assert!(options.tls, "un service en clair doit se demander");
        assert_eq!(options.port, 443);
    }

    #[test]
    fn le_schema_en_clair_change_le_port_par_defaut() {
        let options = options("ws://192.168.1.10/socket", &[]).unwrap();
        assert!(!options.tls);
        assert_eq!(options.port, 80);
        assert_eq!(options.host_header(), "192.168.1.10");
    }

    #[test]
    fn un_port_explicite_apparait_dans_lentete_host() {
        let options = options("ws://192.168.1.10:8123/api/websocket", &[]).unwrap();
        assert_eq!(options.port, 8123);
        assert_eq!(options.host_header(), "192.168.1.10:8123");
        assert_eq!(options.url(), "ws://192.168.1.10:8123/api/websocket");
    }

    #[test]
    fn une_adresse_sans_chemin_interroge_la_racine() {
        assert_eq!(options("wss://exemple.fr", &[]).unwrap().path, "/");
    }

    #[test]
    fn letiquette_path_remplace_le_chemin_de_ladresse() {
        let options = options("wss://exemple.fr/a", &[("path", "/b?x=1")]).unwrap();
        assert_eq!(options.path, "/b?x=1");
    }

    #[test]
    fn un_schema_inapplicable_est_refuse_avec_une_explication() {
        let error = options("mqtt://exemple.fr", &[]).unwrap_err();
        assert!(matches!(error, ProbeError::Config(_)));
        assert!(!error.means_down());
        assert!(error.to_string().contains("wss://"), "{error}");
    }

    #[test]
    fn un_jeton_part_en_bearer_et_un_compte_en_basic() {
        let mut target = cible("websocket", "wss://exemple.fr/ws", &[]);
        target.credential = Credential::ApiToken { token: "abc".into() };
        assert_eq!(
            Options::from_target(&target).unwrap().authorization.as_deref(),
            Some("Bearer abc")
        );

        target.credential =
            Credential::UsernamePassword { username: "u".into(), password: "p".into() };
        assert_eq!(
            Options::from_target(&target).unwrap().authorization.as_deref(),
            Some("Basic dTpw")
        );
    }

    #[test]
    fn une_adresse_vide_est_refusee_avant_tout_appel_reseau() {
        assert!(options("   ", &[]).is_err());
    }
}
