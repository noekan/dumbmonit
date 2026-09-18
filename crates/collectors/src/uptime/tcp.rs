//! Sonde d'ouverture de connexion TCP (`kind = "tcp"`).
//!
//! La plus simple des cinq, et souvent la seule possible : un serveur SSH, un
//! partage SMB, un broker MQTT ou une base de données ne se laissent pas interroger
//! en HTTP, mais tous acceptent — ou refusent — une connexion.
//!
//! Elle s'arrête à l'établissement de la connexion et la referme aussitôt, sans
//! écrire un octet. C'est délibéré : envoyer des données à un protocole inconnu
//! remplirait les journaux du service surveillé de sessions avortées.
//!
//! # Adresse et étiquettes
//!
//! Adresse : `hôte:port`, `[ipv6]:port`, ou `hôte` avec l'étiquette `port`.
//!
//! | Étiquette | Défaut | Rôle |
//! |---|---|---|
//! | `port` | — | Port à ouvrir, si l'adresse n'en précise pas. |
//! | `allow_private_targets` | `false` | Autorise la boucle locale et le lien local (voir `guard`). |
//! | `timeout_seconds` | `5` | Délai propre à la sonde (1 à 60). |

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use dumbmonit_proto::{Collector, ProbeError, Sample, Target};
use tokio::net::TcpStream;
use tracing::debug;

use super::guard;
use super::outcome::{Failure, Report};
use super::tags;

/// Port sentinelle signifiant « aucun port trouvé dans l'adresse ».
///
/// Contrairement aux autres sondes, il n'existe pas de port par défaut sensé pour
/// TCP : surveiller « le port 80 » de ce que l'utilisateur croyait être un serveur
/// SSH ne rendrait service à personne. L'absence est donc une erreur, pas un défaut.
const NO_PORT: u16 = 0;

#[derive(Debug, Clone)]
struct Options {
    host: String,
    port: u16,
    timeout: Duration,
    /// Lève le garde-fou sur la boucle locale et le lien local (voir `guard`).
    allow_private: bool,
}

impl Options {
    fn from_target(target: &Target) -> Result<Self, ProbeError> {
        let (host, port_in_address) = tags::split_host_port(&target.address, NO_PORT)?;
        let port = match tags::parse_u32(target, "port", u32::from(port_in_address), 0..=65_535)? {
            0 => {
                return Err(ProbeError::Config(
                    "no port: write the address as \"host:port\" (for example \
                     \"nas.lan:445\") or add the \"port\" tag"
                        .to_string(),
                ));
            }
            port => port as u16,
        };

        Ok(Self {
            host,
            port,
            timeout: tags::parse_timeout(target, "timeout_seconds")?,
            allow_private: guard::allowed(target)?,
        })
    }
}

/// Collecteur de disponibilité par ouverture de connexion TCP.
#[derive(Default)]
pub struct TcpCollector;

impl TcpCollector {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Collector for TcpCollector {
    fn kind(&self) -> &'static str {
        "tcp"
    }

    async fn probe(&self, target: &Target) -> Result<Vec<Sample>, ProbeError> {
        let options = Options::from_target(target)?;
        let mut report = Report::new(self.kind()).label("port", options.port.to_string());

        connect(&mut report, &options).await?;

        if let Some(detail) = report.detail() {
            debug!(target_id = target.id, host = %options.host, port = options.port, detail,
                "sonde TCP en échec");
        }
        Ok(report.finish())
    }
}

/// Ouvre puis referme la connexion, en distinguant les trois façons d'échouer.
///
/// `Err` seulement pour une adresse refusée par le garde-fou (`guard`) : une
/// erreur de configuration, pas une mesure.
async fn connect(report: &mut Report, options: &Options) -> Result<(), ProbeError> {
    let started = Instant::now();

    let resolved = tokio::time::timeout(
        options.timeout,
        tokio::net::lookup_host((&*options.host, options.port)),
    )
    .await;
    let addresses: Vec<SocketAddr> = match resolved {
        Err(_) => {
            report.fail(Failure::Timeout, "name resolution too slow");
            return Ok(());
        }
        Ok(Err(error)) => {
            report.fail(Failure::Dns, format!("{}: {error}", options.host));
            return Ok(());
        }
        Ok(Ok(addresses)) => addresses.collect(),
    };
    guard::vet(&options.host, &addresses, options.allow_private)?;
    let Some(address) = addresses.first().copied() else {
        report.fail(Failure::Dns, format!("{} resolves to no address", options.host));
        return Ok(());
    };

    // Le délai restant, et non le délai complet : la résolution a déjà consommé
    // une partie du budget, et la sonde doit rendre la main à temps pour écrire
    // son échantillon d'indisponibilité.
    let remaining = options.timeout.saturating_sub(started.elapsed());
    match tokio::time::timeout(remaining, TcpStream::connect(address)).await {
        Err(_) => report.fail(Failure::Timeout, format!("{address}: timed out")),
        Ok(Err(error)) => report.fail(Failure::Connect, format!("{address}: {error}")),
        Ok(Ok(stream)) => {
            report.gauge("connect_seconds", started.elapsed().as_secs_f64());
            // Fermeture immédiate et explicite : sans elle, la socket resterait
            // ouverte jusqu'à la fin de la fonction, ce qui n'est pas très poli
            // pour un service qui compte ses connexions.
            drop(stream);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::uptime::tags::test_support::cible;

    fn options(address: &str, tags: &[(&str, &str)]) -> Result<Options, ProbeError> {
        Options::from_target(&cible("tcp", address, tags))
    }

    /// La boucle locale n'est joignable que depuis l'hôte de supervision : sans
    /// l'option, la sonde refuse avant tout appel réseau, et le refus est une
    /// erreur de configuration qui nomme l'option à activer.
    #[tokio::test]
    async fn la_boucle_locale_est_refusee_sans_loption() {
        let target = cible("tcp", "127.0.0.1:1", &[("timeout_seconds", "2")]);
        let error = TcpCollector::new().probe(&target).await.unwrap_err();
        assert!(matches!(error, ProbeError::Config(_)), "{error}");
        assert!(!error.means_down());
        assert!(error.to_string().contains("allow_private_targets"), "{error}");
    }

    #[test]
    fn le_collecteur_annonce_son_type() {
        assert_eq!(TcpCollector::new().kind(), "tcp");
    }

    #[test]
    fn le_port_se_lit_dans_ladresse() {
        let options = options("nas.lan:445", &[]).unwrap();
        assert_eq!((options.host.as_str(), options.port), ("nas.lan", 445));
        assert_eq!(options.timeout, Duration::from_secs(5));
    }

    #[test]
    fn letiquette_port_prend_le_pas_sur_ladresse() {
        let options = options("nas.lan:445", &[("port", "22")]).unwrap();
        assert_eq!(options.port, 22);
    }

    #[test]
    fn une_ipv6_reste_entiere() {
        assert_eq!(options("[fd00::1]:22", &[]).unwrap().host, "fd00::1");
        let sans_port = options("fd00::1", &[("port", "22")]).unwrap();
        assert_eq!((sans_port.host.as_str(), sans_port.port), ("fd00::1", 22));
    }

    #[test]
    fn labsence_de_port_est_signalee_avec_la_marche_a_suivre() {
        let error = options("nas.lan", &[]).unwrap_err();
        assert!(matches!(error, ProbeError::Config(_)));
        assert!(error.to_string().contains("host:port"), "{error}");
        assert!(!error.means_down());
    }

    #[test]
    fn un_port_hors_bornes_est_refuse_avant_tout_appel_reseau() {
        assert!(options("nas.lan", &[("port", "70000")]).is_err());
        assert!(options("nas.lan:99999", &[]).is_err());
    }

    /// Le contrat central du module, vérifié de bout en bout sur la boucle locale :
    /// un port fermé n'est pas une erreur d'interrogation, c'est une mesure dont le
    /// résultat vaut zéro. Sans cet échantillon, le taux de disponibilité ignorerait
    /// purement et simplement la panne.
    #[tokio::test]
    async fn un_port_ferme_produit_un_echantillon_a_zero_et_non_une_erreur() {
        // Un port réservé puis relâché : la connexion sera refusée immédiatement,
        // sans dépendre d'un service extérieur ni du délai d'attente.
        let ecoute = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = ecoute.local_addr().unwrap().port();
        drop(ecoute);

        let target = cible(
            "tcp",
            &format!("127.0.0.1:{port}"),
            &[("timeout_seconds", "2"), ("allow_private_targets", "true")],
        );
        let samples = TcpCollector::new().probe(&target).await.expect("une mesure, pas une erreur");

        let success = samples.iter().find(|s| s.metric == "probe_success").expect("probe_success");
        assert_eq!(success.value, 0.0);
        assert_eq!(success.labels.get("probe").map(String::as_str), Some("tcp"));
        assert_eq!(success.labels.get("port").map(String::as_str), Some(port.to_string().as_str()));

        assert!(samples.iter().any(|s| s.metric == "probe_duration_seconds"));
        let info = samples.iter().find(|s| s.metric == "probe_failure_info").expect("raison");
        assert_eq!(info.labels.get("reason").map(String::as_str), Some("connect"));
        assert!(
            !samples.iter().any(|s| s.metric == "probe_connect_seconds"),
            "aucune connexion n'a été établie, il n'y a pas de temps à publier"
        );
    }
}
