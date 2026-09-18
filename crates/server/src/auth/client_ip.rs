//! Adresse du client, telle que le serveur peut raisonnablement l'établir.
//!
//! Elle sert aux compteurs de tentatives et au journal d'audit — jamais à
//! autoriser quoi que ce soit. La règle est simple : l'adresse de la connexion
//! TCP fait foi, sauf si elle appartient à un mandataire déclaré de confiance
//! (`DUMBMONIT_TRUSTED_PROXIES`), auquel cas c'est le dernier `X-Forwarded-For`
//! qu'il a ajouté qui compte. Sans cette liste, l'en-tête est ignoré : n'importe
//! quel client peut l'écrire, et s'y fier reviendrait à laisser l'attaquant
//! choisir son seau.

use std::net::{IpAddr, SocketAddr};

use axum::extract::ConnectInfo;
use axum::http::request::Parts;
use axum::http::{Extensions, HeaderMap};
use ipnet::IpNet;

/// Adresse du client, ou `None` quand le serveur n'en a aucune idée (tests sans
/// socket, transport inconnu).
#[derive(Debug, Clone, Copy)]
pub struct ClientIp(pub Option<IpAddr>);

impl<S: Send + Sync> axum::extract::FromRequestParts<S> for ClientIp {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let trusted = parts
            .extensions
            .get::<crate::auth::AuthState>()
            .map(|auth| auth.trusted_proxies().to_vec())
            .unwrap_or_default();
        Ok(Self(resolve(&parts.extensions, &parts.headers, &trusted)))
    }
}

/// Établit l'adresse à partir de la connexion et, le cas échéant, des en-têtes.
pub fn resolve(extensions: &Extensions, headers: &HeaderMap, trusted: &[IpNet]) -> Option<IpAddr> {
    let peer = extensions.get::<ConnectInfo<SocketAddr>>().map(|info| info.0.ip());
    match peer {
        Some(peer) if trusted.iter().any(|net| net.contains(&peer)) => {
            forwarded_for(headers, trusted).or(Some(peer))
        }
        other => other,
    }
}

/// Dernier saut de `X-Forwarded-For` qui n'est pas lui-même un mandataire de
/// confiance : c'est celui que le premier mandataire de la chaîne a vu arriver.
fn forwarded_for(headers: &HeaderMap, trusted: &[IpNet]) -> Option<IpAddr> {
    headers
        .get_all("x-forwarded-for")
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .filter_map(|hop| hop.trim().parse::<IpAddr>().ok())
        .rev()
        .find(|hop| !trusted.iter().any(|net| net.contains(hop)))
}

/// Lit `DUMBMONIT_TRUSTED_PROXIES` : adresses ou réseaux CIDR, séparés par des
/// virgules. Une entrée illisible est signalée et ignorée plutôt que de refuser
/// le démarrage : mieux vaut un compteur par adresse de mandataire qu'un serveur
/// arrêté.
pub fn parse_trusted_proxies(raw: &str) -> Vec<IpNet> {
    raw.split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .filter_map(|entry| {
            let parsed =
                entry.parse::<IpNet>().or_else(|_| entry.parse::<IpAddr>().map(IpNet::from));
            match parsed {
                Ok(net) => Some(net),
                Err(_) => {
                    tracing::warn!(entry, "DUMBMONIT_TRUSTED_PROXIES: entrée ignorée");
                    None
                }
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn with_peer(ip: &str) -> Extensions {
        let mut extensions = Extensions::new();
        extensions.insert(ConnectInfo(SocketAddr::new(ip.parse().unwrap(), 4242)));
        extensions
    }

    fn forwarded(value: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", HeaderValue::from_str(value).unwrap());
        headers
    }

    #[test]
    fn the_header_is_ignored_unless_the_peer_is_a_trusted_proxy() {
        let headers = forwarded("203.0.113.9");
        let peer: IpAddr = "10.0.0.2".parse().unwrap();
        assert_eq!(resolve(&with_peer("10.0.0.2"), &headers, &[]), Some(peer));

        let trusted = parse_trusted_proxies("10.0.0.0/8");
        let client: IpAddr = "203.0.113.9".parse().unwrap();
        assert_eq!(resolve(&with_peer("10.0.0.2"), &headers, &trusted), Some(client));
        // Un mandataire de confiance sans en-tête : c'est lui, le client.
        assert_eq!(resolve(&with_peer("10.0.0.2"), &HeaderMap::new(), &trusted), Some(peer));
    }

    #[test]
    fn the_last_untrusted_hop_wins() {
        let trusted = parse_trusted_proxies("10.0.0.2, 10.0.0.3");
        let headers = forwarded("198.51.100.1, 203.0.113.9, 10.0.0.3");
        let client: IpAddr = "203.0.113.9".parse().unwrap();
        assert_eq!(resolve(&with_peer("10.0.0.2"), &headers, &trusted), Some(client));
    }

    #[test]
    fn without_a_socket_there_is_no_address() {
        assert_eq!(resolve(&Extensions::new(), &forwarded("203.0.113.9"), &[]), None);
    }

    #[test]
    fn unreadable_entries_are_skipped() {
        let nets = parse_trusted_proxies("127.0.0.1, nope, fd00::/8,");
        assert_eq!(nets.len(), 2);
    }
}
