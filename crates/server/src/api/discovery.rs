//! Découverte d'équipements SNMP sur le réseau.
//!
//! C'est le raccourci qui évite de saisir les équipements un par un : on donne un
//! réseau, on obtient la liste de ce qui répond, avec le profil que
//! l'auto-détection appliquerait.
//!
//! Un balayage est une action, pas une lecture : il part en `POST`, avec ses
//! paramètres dans le corps. Le garde de session en fait de ce fait une opération
//! d'administrateur, et la community SNMP ne traîne ni dans les journaux du
//! serveur ni dans ceux d'un mandataire, contrairement à une chaîne de requête.

use axum::Json;
use dumbmonit_proto::Credential;
use serde::{Deserialize, Serialize};

use crate::api::{ApiError, ApiResult};
use crate::auth::middleware::AdminIdentity;
use crate::collectors::snmp::{DiscoveredDevice, ScanOptions, scan_network};

#[derive(Deserialize)]
pub struct ScanRequest {
    /// Réseau à balayer, en notation CIDR : `192.168.1.0/24`.
    pub cidr: String,
    /// Community SNMP essayée sur chaque adresse. `public` par défaut, qui reste la
    /// valeur d'usine de la majorité des équipements.
    #[serde(default)]
    pub community: Option<String>,
    #[serde(default)]
    pub port: Option<u16>,
    /// Délai par adresse, en millisecondes.
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

/// `Debug` écrit à la main : la community est un secret, elle ne doit pas finir
/// dans une trace de requête.
impl std::fmt::Debug for ScanRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "ScanRequest {{ cidr: {:?}, community: <redacted>, port: {:?}, timeout_ms: {:?} }}",
            self.cidr, self.port, self.timeout_ms
        )
    }
}

#[derive(Debug, Serialize)]
pub struct ScanResponse {
    pub devices: Vec<DiscoveredDevice>,
    /// Adresses effectivement sondées, pour que l'interface puisse expliquer
    /// « 254 adresses testées, 3 équipements trouvés ».
    pub scanned: usize,
}

/// `POST /api/discovery` — balaie un réseau à la recherche d'équipements SNMP.
/// Réservé aux administrateurs : c'est le serveur qui sonde, vers n'importe quel
/// réseau qu'il peut joindre.
///
/// L'opération est bornée par les garde-fous de [`ScanOptions`] : un `/16` saisi par
/// mégarde est refusé plutôt que de lancer soixante-cinq mille sondes.
pub async fn scan(
    _: AdminIdentity,
    Json(request): Json<ScanRequest>,
) -> ApiResult<Json<ScanResponse>> {
    let network: ipnet::IpNet = request
        .cidr
        .trim()
        .parse()
        .map_err(|_| ApiError::BadRequest(format!("Invalid network \"{}\".", request.cidr)))?;

    let credential = Credential::SnmpCommunity {
        community: request.community.unwrap_or_else(|| "public".to_string()),
    };

    let mut options = ScanOptions::default();
    if let Some(port) = request.port {
        options.port = port;
    }
    if let Some(timeout_ms) = request.timeout_ms {
        options.timeout = std::time::Duration::from_millis(timeout_ms.clamp(100, 10_000));
    }

    // Le nombre d'hôtes est calculé avant le balayage : c'est ce qui permet de
    // refuser un réseau trop large avec un message utile plutôt qu'après coup.
    let scanned = network.hosts().take(options.max_hosts + 1).count();
    if scanned > options.max_hosts {
        return Err(ApiError::BadRequest(format!(
            "Network too large: {} addresses at most. Choose a narrower prefix.",
            options.max_hosts
        )));
    }

    let devices = scan_network(network, &credential, &options)
        .await
        .map_err(|error| ApiError::BadRequest(error.to_string()))?;

    Ok(Json(ScanResponse { devices, scanned }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_never_leaks_the_community() {
        let request = ScanRequest {
            cidr: "192.168.1.0/24".into(),
            community: Some("s3cr3t-community".into()),
            port: None,
            timeout_ms: None,
        };
        let rendered = format!("{request:?}");
        assert!(!rendered.contains("s3cr3t"), "{rendered}");
        assert!(rendered.contains("<redacted>"), "{rendered}");
    }
}
