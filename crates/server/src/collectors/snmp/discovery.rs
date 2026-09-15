//! Balayage d'un réseau à la recherche d'équipements SNMP.
//!
//! Un GET sur `sysDescr` suffit à trancher : un équipement qui répond parle SNMP et
//! accepte l'identifiant fourni. Les autres ne répondent rien du tout — SNMP ne
//! signale pas les communities invalides — et sont écartés par expiration du délai.

use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use ezymonit_proto::{Credential, ProbeError};
use ipnet::IpNet;
use serde::Serialize;
use tokio::sync::Semaphore;
use tracing::{debug, info};

use super::oid::ObjectId;
use super::profile::{self, Catalog};
use super::session::{CommunityVersion, Session};
use super::{SYS_DESCR, SYS_NAME, SYS_OBJECT_ID};

/// Réglages du balayage.
#[derive(Debug, Clone)]
pub struct ScanOptions {
    pub port: u16,
    /// Délai accordé à chaque adresse. Court par construction : sur un réseau local,
    /// un équipement qui n'a pas répondu en une seconde ne répondra pas.
    pub timeout: Duration,
    /// Nombre d'adresses sondées simultanément.
    pub concurrency: usize,
    /// Garde-fou : un `/16` saisi par mégarde représente soixante-cinq mille sondes.
    pub max_hosts: usize,
    /// Version employée avec une community.
    pub version: CommunityVersion,
}

impl Default for ScanOptions {
    fn default() -> Self {
        Self {
            port: super::session::DEFAULT_PORT,
            timeout: Duration::from_secs(1),
            // Assez pour balayer un /24 en quelques secondes, assez peu pour ne pas
            // saturer la table de suivi de connexions d'un routeur domestique.
            concurrency: 64,
            max_hosts: 4096,
            version: CommunityVersion::V2c,
        }
    }
}

/// Un équipement ayant répondu au balayage.
#[derive(Debug, Clone, Serialize)]
pub struct DiscoveredDevice {
    /// Adresse au format attendu par `Target::address`, port inclus s'il n'est pas
    /// celui par défaut.
    pub address: String,
    pub sysdescr: Option<String>,
    pub sysname: Option<String>,
    pub sysobjectid: Option<String>,
    /// Profil que l'auto-détection appliquerait à cet équipement.
    pub suggested_profile: Option<String>,
}

/// Balaie un réseau et renvoie les équipements SNMP qui ont répondu.
///
/// La concurrence est bornée : un `/24` produit deux cent cinquante-quatre sondes,
/// qu'il ne s'agit pas d'émettre toutes dans la même milliseconde.
pub async fn scan_network(
    network: IpNet,
    credential: &Credential,
    options: &ScanOptions,
) -> Result<Vec<DiscoveredDevice>, ProbeError> {
    if !matches!(credential, Credential::SnmpCommunity { .. } | Credential::SnmpV3 { .. }) {
        return Err(ProbeError::Config(format!(
            "SNMP scan is not possible with a credential of type \"{}\"",
            credential.kind_label()
        )));
    }

    // `hosts()` écarte déjà l'adresse de réseau et celle de diffusion, et rend
    // correctement les préfixes /31 et /32. Un élément de plus que la limite suffit à
    // détecter le dépassement sans dérouler un /8 en mémoire.
    let hosts: Vec<IpAddr> = network.hosts().take(options.max_hosts + 1).collect();

    if hosts.len() > options.max_hosts {
        return Err(ProbeError::Config(format!(
            "network {network} exceeds the limit of {} addresses; scan smaller subnets",
            options.max_hosts
        )));
    }

    info!(%network, hosts = hosts.len(), "balayage SNMP démarré");

    let catalog = profile::embedded();
    let credential = Arc::new(credential.clone());
    let options = Arc::new(options.clone());
    let permits = Arc::new(Semaphore::new(options.concurrency.max(1)));

    // Une tâche par adresse plutôt qu'un flux de futures composées : chaque sonde est
    // alors interrogée depuis la boucle de l'ordonnanceur, sans empiler les trames
    // des combinateurs. `AsyncSession` porte ses tampons en ligne, et cette pile-là
    // finit par coûter cher.
    let mut tasks = Vec::with_capacity(hosts.len());
    for host in hosts {
        let credential = credential.clone();
        let options = options.clone();
        let permits = permits.clone();
        tasks.push(tokio::spawn(async move {
            let _permit = permits.acquire().await.ok()?;
            probe_one(host, &credential, &options, catalog).await
        }));
    }

    let mut found = Vec::new();
    for task in tasks {
        // Une tâche qui panique ne doit pas emporter tout le balayage.
        if let Ok(Some(device)) = task.await {
            found.push(device);
        }
    }

    // Les tâches sont attendues dans l'ordre d'émission, mais le tri le garantit
    // explicitement : l'interface affiche cette liste telle quelle.
    found.sort_by(|left, right| left.address.cmp(&right.address));
    info!(%network, found = found.len(), "balayage SNMP terminé");
    Ok(found)
}

/// Sonde une adresse. `None` dès qu'elle ne répond pas : sur un balayage, une adresse
/// muette est le cas normal et ne mérite ni erreur ni journal.
async fn probe_one(
    host: IpAddr,
    credential: &Credential,
    options: &ScanOptions,
    catalog: &Catalog,
) -> Option<DiscoveredDevice> {
    let endpoint = std::net::SocketAddr::new(host, options.port).to_string();
    let mut session =
        Session::open(&endpoint, credential, options.timeout, options.version).await.ok()?;

    let identity = read_identity(&mut session).await.ok()?;
    // Aucun sysDescr : soit ce n'est pas un agent SNMP, soit l'identifiant est refusé.
    identity.sysdescr.as_ref()?;

    let sysobjectid = identity.sysobjectid.as_ref().and_then(|raw| raw.parse::<ObjectId>().ok());
    let suggested = catalog
        .select(sysobjectid.as_ref(), identity.sysdescr.as_deref())
        .map(|profile| profile.id.clone());

    debug!(%host, profile = ?suggested, "équipement SNMP détecté");

    Some(DiscoveredDevice {
        // Le port par défaut est omis pour que l'adresse reste celle que
        // l'utilisateur aurait saisie lui-même.
        address: if options.port == super::session::DEFAULT_PORT {
            host.to_string()
        } else {
            endpoint
        },
        sysdescr: identity.sysdescr,
        sysname: identity.sysname,
        sysobjectid: identity.sysobjectid,
        suggested_profile: suggested,
    })
}

/// Ce qu'un équipement dit de lui-même.
#[derive(Debug, Default, Clone)]
pub struct Identity {
    pub sysdescr: Option<String>,
    pub sysname: Option<String>,
    pub sysobjectid: Option<String>,
}

/// Lit les trois scalaires d'identification en une seule requête.
pub async fn read_identity(session: &mut Session) -> Result<Identity, ProbeError> {
    let wanted = [SYS_DESCR.clone(), SYS_OBJECT_ID.clone(), SYS_NAME.clone()];
    let mut identity = Identity::default();

    for (oid, value) in session.get_many(&wanted).await? {
        let Some(text) = value.as_label() else { continue };
        if oid == *SYS_DESCR {
            identity.sysdescr = Some(text);
        } else if oid == *SYS_OBJECT_ID {
            identity.sysobjectid = Some(text);
        } else if oid == *SYS_NAME {
            identity.sysname = Some(text);
        }
    }
    Ok(identity)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn community() -> Credential {
        Credential::SnmpCommunity { community: "public".to_string() }
    }

    #[test]
    fn un_identifiant_non_snmp_est_refuse_sans_toucher_au_reseau() {
        let error = super::super::testutil::block_on_large_stack(async {
            scan_network(
                "10.0.0.0/30".parse().unwrap(),
                &Credential::ApiToken { token: "s3cr3t".to_string() },
                &ScanOptions::default(),
            )
            .await
            .unwrap_err()
        });
        assert!(matches!(error, ProbeError::Config(_)));
        assert!(!error.to_string().contains("s3cr3t"));
    }

    #[test]
    fn un_reseau_trop_vaste_est_refuse() {
        let error = super::super::testutil::block_on_large_stack(async {
            let options = ScanOptions { max_hosts: 8, ..ScanOptions::default() };
            scan_network("10.0.0.0/16".parse().unwrap(), &community(), &options).await.unwrap_err()
        });
        assert!(matches!(error, ProbeError::Config(_)), "{error}");
        assert!(!error.means_down());
    }

    #[test]
    fn un_reseau_sans_equipement_renvoie_une_liste_vide() {
        // 192.0.2.0/24 est réservé à la documentation (RFC 5737) : personne ne répond.
        let found = super::super::testutil::block_on_large_stack(async {
            let options = ScanOptions {
                timeout: Duration::from_millis(120),
                concurrency: 16,
                ..ScanOptions::default()
            };
            scan_network("192.0.2.0/29".parse().unwrap(), &community(), &options).await.unwrap()
        });
        assert!(found.is_empty(), "{found:?}");
    }
}
