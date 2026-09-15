use serde::{Deserialize, Serialize};

/// Secret d'accès à une cible. Toujours chiffré au repos dans la base.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
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
    /// Jeton porteur, pour les intégrations API (Proxmox, Synology).
    ApiToken { token: String },
    /// Identifiants classiques, pour les API qui n'acceptent pas de jeton.
    UsernamePassword { username: String, password: String },
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
