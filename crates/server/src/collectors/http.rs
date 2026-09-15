//! Clients HTTP partagés entre les collecteurs qui parlent à une API REST
//! (Proxmox VE, Proxmox Backup Server, Synology…).
//!
//! Un client `reqwest` porte son propre pool de connexions, son résolveur DNS
//! et sa configuration TLS : en construire un par collecteur — et *a fortiori*
//! deux, vérifié et non vérifié — multiplie d'autant les threads et les tampons
//! sans rien apporter. Deux clients pour tout le serveur suffisent : la politique
//! TLS est la seule chose qui ne se change pas par requête, tout le reste (délai,
//! en-têtes d'authentification) se règle sur le `RequestBuilder`.

use std::sync::OnceLock;
use std::time::Duration;

use ezymonit_proto::ProbeError;

/// Délai d'établissement de la connexion TCP + TLS.
///
/// Un équipement éteint se manifeste par un `SYN` sans réponse : cinq secondes
/// suffisent à le constater sans faire attendre les autres cibles.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Durée de conservation d'une connexion inactive dans le pool.
///
/// Les cibles sont interrogées toutes les 30 à 60 s : garder la connexion
/// ouverte entre deux interrogations évite une poignée de main TLS complète à
/// chaque fois, ce qui est la part la plus coûteuse d'une collecte Proxmox.
const POOL_IDLE_TIMEOUT: Duration = Duration::from_secs(120);

static VERIFIED: OnceLock<reqwest::Client> = OnceLock::new();
static UNVERIFIED: OnceLock<reqwest::Client> = OnceLock::new();

/// Le client partagé, construit à la première demande.
///
/// `insecure_tls` désactive toute vérification du certificat présenté. C'est
/// indispensable face à une installation Proxmox ou Synology par défaut, qui
/// s'annonce avec un certificat auto-signé, mais cela expose la connexion à une
/// interception : l'option n'est jamais implicite, elle vient d'une étiquette de
/// la cible.
pub fn client(insecure_tls: bool) -> Result<reqwest::Client, ProbeError> {
    let cell = if insecure_tls { &UNVERIFIED } else { &VERIFIED };
    if let Some(existing) = cell.get() {
        return Ok(existing.clone());
    }
    let built = build(insecure_tls)?;
    // Une course entre deux cibles construirait deux clients ; le perdant est
    // simplement jeté, ce qui est sans conséquence.
    let _ = cell.set(built.clone());
    Ok(cell.get().cloned().unwrap_or(built))
}

fn build(accept_invalid_certs: bool) -> Result<reqwest::Client, ProbeError> {
    reqwest::Client::builder()
        .danger_accept_invalid_certs(accept_invalid_certs)
        .connect_timeout(CONNECT_TIMEOUT)
        .pool_idle_timeout(POOL_IDLE_TIMEOUT)
        // Un cluster de trois nœuds passe par un seul proxy : deux connexions par
        // hôte couvrent les appels parallèles sans en garder dix ouvertes.
        .pool_max_idle_per_host(2)
        .user_agent(concat!("DumbMonit/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|error| ProbeError::Config(format!("HTTP client unavailable: {error}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn les_deux_politiques_tls_donnent_un_client_utilisable() {
        assert!(client(false).is_ok());
        assert!(client(true).is_ok());
        // Un second appel ne reconstruit rien : la cellule est déjà remplie.
        assert!(VERIFIED.get().is_some());
        assert!(UNVERIFIED.get().is_some());
    }
}
