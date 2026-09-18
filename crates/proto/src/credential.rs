use serde::{Deserialize, Serialize};

/// Secret d'accès à une cible. Toujours chiffré au repos dans la base.
///
/// La désérialisation passe par [`CredentialWire`] : la forme stockée et
/// renvoyée est celle-ci, mais l'API accepte en entrée quelques variantes plus
/// commodes à saisir (un jeton Proxmox en deux champs, par exemple).
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", try_from = "CredentialWire")]
pub enum Credential {
    /// Aucune authentification (ICMP, HTTP public).
    None,
    /// SNMP v1 et v2c : une simple community.
    SnmpCommunity { community: String },
    /// SNMP v3 (USM).
    SnmpV3 {
        username: String,
        #[serde(default)]
        auth: Option<SnmpV3Auth>,
        #[serde(default)]
        privacy: Option<SnmpV3Privacy>,
        #[serde(default)]
        context: Option<String>,
    },
    /// Jeton porteur, pour les intégrations API (Proxmox, HTTP).
    ///
    /// Pour Proxmox VE et PBS, `token` est la forme complète `user@realm!nom=secret` :
    /// c'est ce que les collecteurs attendent, et ce que [`CredentialWire`]
    /// assemble quand l'identifiant et le secret arrivent séparément.
    ApiToken { token: String },
    /// Identifiants classiques, pour les API qui n'acceptent pas de jeton.
    UsernamePassword { username: String, password: String },
}

/// Forme acceptée en entrée pour un [`Credential`].
///
/// Identique au type public, à une exception près : un `api_token` peut arriver
/// soit sous la forme combinée `token`, soit en deux morceaux `token_id` +
/// `secret` — les deux champs que l'interface présente pour Proxmox VE et PBS,
/// parce que personne ne devine qu'il faut coller `user@pve!nom=secret` dans une
/// seule case. Les deux morceaux sont assemblés ici, une fois pour toutes, et le
/// reste du serveur ne connaît que la forme combinée.
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum CredentialWire {
    None,
    SnmpCommunity {
        community: String,
    },
    SnmpV3 {
        username: String,
        #[serde(default)]
        auth: Option<SnmpV3Auth>,
        #[serde(default)]
        privacy: Option<SnmpV3Privacy>,
        #[serde(default)]
        context: Option<String>,
    },
    ApiToken {
        #[serde(default)]
        token: Option<String>,
        #[serde(default)]
        token_id: Option<String>,
        #[serde(default)]
        secret: Option<String>,
    },
    UsernamePassword {
        username: String,
        password: String,
    },
}

/// Nettoie une valeur collée : espaces et sauts de ligne autour, guillemets
/// d'encadrement. Le message d'erreur d'un jeton copié « avec ses guillemets »
/// est trop opaque pour laisser la valeur telle quelle.
fn tidy(value: &str) -> &str {
    let value = value.trim();
    for quote in ['"', '\''] {
        if value.len() >= 2 && value.starts_with(quote) && value.ends_with(quote) {
            return value[1..value.len() - 1].trim();
        }
    }
    value
}

impl TryFrom<CredentialWire> for Credential {
    type Error = String;

    fn try_from(wire: CredentialWire) -> Result<Self, Self::Error> {
        Ok(match wire {
            CredentialWire::None => Self::None,
            CredentialWire::SnmpCommunity { community } => Self::SnmpCommunity { community },
            CredentialWire::SnmpV3 { username, auth, privacy, context } => {
                Self::SnmpV3 { username, auth, privacy, context }
            }
            CredentialWire::UsernamePassword { username, password } => {
                Self::UsernamePassword { username, password }
            }
            CredentialWire::ApiToken { token, token_id, secret } => {
                let token_id = token_id.as_deref().map(tidy).filter(|s| !s.is_empty());
                let secret = secret.as_deref().map(tidy).filter(|s| !s.is_empty());
                let token = token.as_deref().map(tidy).filter(|s| !s.is_empty());
                let token = match (token_id, secret, token) {
                    // Un identifiant collé avec son secret (`user@pve!nom=secret`,
                    // la forme historique) est accepté tel quel plutôt que doublé.
                    (Some(id), secret, _) if id.contains('=') => match secret {
                        Some(secret) if !id.ends_with(&format!("={secret}")) => {
                            return Err("api_token: \"token_id\" already ends with a secret, \
                                        which differs from \"secret\""
                                .into());
                        }
                        _ => id.to_string(),
                    },
                    // Les deux morceaux : on assemble la forme que les collecteurs
                    // Proxmox attendent.
                    (Some(id), Some(secret), _) => format!("{id}={secret}"),
                    (Some(_), None, _) => {
                        return Err("api_token: \"secret\" is required with \"token_id\"".into());
                    }
                    (None, Some(_), _) => {
                        return Err("api_token: \"token_id\" is required with \"secret\"".into());
                    }
                    (None, None, Some(token)) => token.to_string(),
                    (None, None, None) => {
                        return Err(
                            "api_token: give \"token\", or \"token_id\" and \"secret\"".into()
                        );
                    }
                };
                Self::ApiToken { token }
            }
        })
    }
}

impl Credential {
    /// Étiquette courte affichée dans l'interface, garantie sans secret.
    pub fn kind_label(&self) -> &'static str {
        match self {
            Self::None => "None",
            Self::SnmpCommunity { .. } => "SNMP community",
            Self::SnmpV3 { .. } => "SNMP v3",
            Self::ApiToken { .. } => "API token",
            Self::UsernamePassword { .. } => "Username / password",
        }
    }
}

/// `Debug` est implémenté à la main — et non dérivé — pour qu'un secret ne puisse
/// jamais atterrir dans un journal, un rapport de panique ou un message d'erreur.
/// Ne dérivez `Debug` sur aucun des types de ce fichier.
impl std::fmt::Debug for Credential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::None => f.write_str("Credential::None"),
            Self::SnmpCommunity { .. } => f.write_str("Credential::SnmpCommunity(<redacted>)"),
            Self::SnmpV3 { username, auth, privacy, .. } => write!(
                f,
                "Credential::SnmpV3 {{ username: {username:?}, auth: {}, privacy: {} }}",
                auth.as_ref().map_or("none", |a| a.protocol.as_str()),
                privacy.as_ref().map_or("none", |p| p.protocol.as_str()),
            ),
            Self::ApiToken { .. } => f.write_str("Credential::ApiToken(<redacted>)"),
            Self::UsernamePassword { username, .. } => {
                write!(
                    f,
                    "Credential::UsernamePassword {{ username: {username:?}, password: <redacted> }}"
                )
            }
        }
    }
}

impl std::fmt::Display for Credential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.kind_label())
    }
}

impl std::fmt::Debug for SnmpV3Auth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SnmpV3Auth {{ protocol: {}, passphrase: <redacted> }}", self.protocol.as_str())
    }
}

impl std::fmt::Debug for SnmpV3Privacy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "SnmpV3Privacy {{ protocol: {}, passphrase: <redacted> }}",
            self.protocol.as_str()
        )
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnmpV3Auth {
    pub protocol: SnmpV3AuthProtocol,
    pub passphrase: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SnmpV3AuthProtocol {
    Md5,
    Sha1,
    Sha224,
    Sha256,
    Sha384,
    Sha512,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnmpV3Privacy {
    pub protocol: SnmpV3PrivacyProtocol,
    pub passphrase: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SnmpV3PrivacyProtocol {
    Des,
    Aes128,
    Aes192,
    Aes256,
}

impl SnmpV3AuthProtocol {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Md5 => "MD5",
            Self::Sha1 => "SHA-1",
            Self::Sha224 => "SHA-224",
            Self::Sha256 => "SHA-256",
            Self::Sha384 => "SHA-384",
            Self::Sha512 => "SHA-512",
        }
    }
}

impl SnmpV3PrivacyProtocol {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Des => "DES",
            Self::Aes128 => "AES-128",
            Self::Aes192 => "AES-192",
            Self::Aes256 => "AES-256",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Garde-fou : une régression ici exposerait des mots de passe dans les journaux.
    #[test]
    fn debug_never_leaks_a_secret() {
        let secrets = ["s3cr3t-community", "s3cr3t-auth", "s3cr3t-priv", "s3cr3t-token"];

        let credentials = vec![
            Credential::SnmpCommunity { community: secrets[0].into() },
            Credential::SnmpV3 {
                username: "monitor".into(),
                auth: Some(SnmpV3Auth {
                    protocol: SnmpV3AuthProtocol::Sha256,
                    passphrase: secrets[1].into(),
                }),
                privacy: Some(SnmpV3Privacy {
                    protocol: SnmpV3PrivacyProtocol::Aes128,
                    passphrase: secrets[2].into(),
                }),
                context: None,
            },
            Credential::ApiToken { token: secrets[3].into() },
            Credential::UsernamePassword {
                username: "admin".into(),
                password: "s3cr3t-password".into(),
            },
        ];

        for credential in &credentials {
            let rendered = format!("{credential:?} {credential}");
            for secret in secrets {
                assert!(!rendered.contains(secret), "le secret {secret} a fuité dans : {rendered}");
            }
            assert!(!rendered.contains("s3cr3t-password"), "mot de passe fuité : {rendered}");
        }
    }

    fn parse(json: &str) -> Result<Credential, String> {
        serde_json::from_str::<Credential>(json).map_err(|e| e.to_string())
    }

    /// L'interface envoie l'identifiant et le secret d'un jeton Proxmox dans deux
    /// champs : le serveur les assemble sous la forme que les collecteurs lisent.
    #[test]
    fn un_jeton_en_deux_morceaux_est_assemble() {
        let credential =
            parse(r#"{"type":"api_token","token_id":" dumbmonit@pve!monitor ","secret":"\"8f3a-1c9e\"\n"}"#)
                .unwrap();
        assert_eq!(
            credential,
            Credential::ApiToken { token: "dumbmonit@pve!monitor=8f3a-1c9e".into() }
        );
    }

    #[test]
    fn la_forme_combinee_reste_acceptee() {
        let combined = Credential::ApiToken { token: "dumbmonit@pve!monitor=8f3a-1c9e".into() };
        assert_eq!(
            parse(r#"{"type":"api_token","token":"dumbmonit@pve!monitor=8f3a-1c9e"}"#).unwrap(),
            combined
        );
        // Jeton complet collé dans le champ « Token ID », avec ou sans le secret répété.
        assert_eq!(
            parse(r#"{"type":"api_token","token_id":"dumbmonit@pve!monitor=8f3a-1c9e"}"#).unwrap(),
            combined
        );
        assert_eq!(
            parse(r#"{"type":"api_token","token_id":"dumbmonit@pve!monitor=8f3a-1c9e","secret":"8f3a-1c9e"}"#).unwrap(),
            combined
        );
        // Ce que le serveur renvoie et stocke ne change pas de forme.
        assert_eq!(
            serde_json::to_string(&combined).unwrap(),
            r#"{"type":"api_token","token":"dumbmonit@pve!monitor=8f3a-1c9e"}"#
        );
    }

    #[test]
    fn un_jeton_incomplet_est_refuse_avec_le_champ_manquant() {
        let err = parse(r#"{"type":"api_token","token_id":"dumbmonit@pve!monitor"}"#).unwrap_err();
        assert!(err.contains("secret"), "{err}");
        let err = parse(r#"{"type":"api_token","secret":"8f3a"}"#).unwrap_err();
        assert!(err.contains("token_id"), "{err}");
        let err = parse(r#"{"type":"api_token"}"#).unwrap_err();
        assert!(err.contains("token"), "{err}");
        let err =
            parse(r#"{"type":"api_token","token_id":"a@pve!b=one","secret":"two"}"#).unwrap_err();
        assert!(err.contains("differs"), "{err}");
    }

    #[test]
    fn les_autres_formes_traversent_sans_changement() {
        assert_eq!(parse(r#"{"type":"none"}"#).unwrap(), Credential::None);
        assert_eq!(
            parse(r#"{"type":"username_password","username":"dumbmonit","password":" p "}"#)
                .unwrap(),
            Credential::UsernamePassword { username: "dumbmonit".into(), password: " p ".into() }
        );
        assert_eq!(
            parse(r#"{"type":"snmp_v3","username":"monitor"}"#).unwrap(),
            Credential::SnmpV3 {
                username: "monitor".into(),
                auth: None,
                privacy: None,
                context: None
            }
        );
    }

    #[test]
    fn debug_keeps_the_useful_context() {
        let credential = Credential::SnmpV3 {
            username: "monitor".into(),
            auth: Some(SnmpV3Auth { protocol: SnmpV3AuthProtocol::Sha256, passphrase: "x".into() }),
            privacy: None,
            context: None,
        };
        let rendered = format!("{credential:?}");
        assert!(rendered.contains("monitor"), "{rendered}");
        assert!(rendered.contains("SHA-256"), "{rendered}");
        assert!(rendered.contains("none"), "{rendered}");
    }
}
