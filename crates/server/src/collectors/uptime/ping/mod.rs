//! Sonde ICMP (`kind = "ping"`).
//!
//! Mesure le temps d'aller-retour et la perte de paquets sur une salve d'échos.
//! C'est la seule sonde qui renseigne sur la qualité du lien, et non seulement sur
//! sa présence : une perte de 20 % sur un lien Wi-Fi ou un tunnel se voit ici bien
//! avant que le service qui l'emprunte ne devienne inutilisable.
//!
//! # Le ping exige un droit particulier
//!
//! ICMP demande une socket brute (`CAP_NET_RAW`) ou, sous Linux, l'appartenance à
//! la plage `net.ipv4.ping_group_range`. L'image DumbMonit est construite depuis
//! `scratch` et ne reçoit aucune capacité : **le ping échoue tant que le
//! `docker-compose.yml` n'a pas été complété**. Ce cas est détecté explicitement et
//! renvoie un `ProbeError::Config` qui indique quoi ajouter — sans quoi
//! l'utilisateur lirait « Permission denied » et croirait son équipement éteint.
//!
//! # Adresse et étiquettes
//!
//! Adresse : le nom ou l'adresse IP de l'hôte.
//!
//! | Étiquette | Défaut | Rôle |
//! |---|---|---|
//! | `count` | `4` | Nombre d'échos par interrogation (1 à 20). |
//! | `packet_timeout_ms` | `1000` | Attente d'une réponse, par paquet. |
//! | `interval_ms` | `100` | Espacement entre deux envois. |
//! | `payload_bytes` | `56` | Taille de la charge utile. |
//! | `ip_version` | `auto` | `auto`, `4` ou `6`. |
//! | `loss_threshold_percent` | `100` | Perte au-delà de laquelle la sonde échoue. |
//! | `timeout_seconds` | `5` | Budget total de la sonde (1 à 60). |

pub(crate) mod options;
mod stats;

use std::net::IpAddr;

use async_trait::async_trait;
use dumbmonit_proto::{Collector, ProbeError, Sample, Target};
use surge_ping::{Client, Config, ICMP, PingIdentifier, PingSequence, SurgeError};
use tracing::debug;

use super::outcome::{Failure, Report};
use options::{IpVersion, Options};
use stats::Burst;

/// Collecteur de disponibilité par échos ICMP.
#[derive(Default)]
pub struct PingCollector;

impl PingCollector {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Collector for PingCollector {
    fn kind(&self) -> &'static str {
        "ping"
    }

    async fn probe(&self, target: &Target) -> Result<Vec<Sample>, ProbeError> {
        let options = Options::from_target(target)?;
        let mut report = Report::new(self.kind());

        let address = match resolve(&options).await {
            Ok(address) => address,
            Err((reason, detail)) => {
                report.fail(reason, detail);
                return Ok(report.finish());
            }
        };

        // La socket est ouverte avant la salve : un refus de droits est une erreur
        // de configuration de l'hôte, pas une indisponibilité de la cible, et il ne
        // doit donc surtout pas s'écrire `probe_success = 0`.
        let client = client_for(address)?;
        let burst = send_burst(&client, address, target.id, &options).await;

        report.gauge("icmp_packets_sent", f64::from(burst.sent()));
        report.gauge("icmp_packets_received", f64::from(burst.received()));
        report.gauge("icmp_packet_loss_ratio", burst.loss_ratio());
        if let Some(mean) = burst.mean_rtt() {
            report.gauge("icmp_rtt_seconds", mean);
        }
        if let Some(min) = burst.min_rtt() {
            report.gauge("icmp_rtt_min_seconds", min);
        }
        if let Some(max) = burst.max_rtt() {
            report.gauge("icmp_rtt_max_seconds", max);
        }

        if burst.loss_ratio() > options.loss_threshold {
            report.fail(
                Failure::PacketLoss,
                format!(
                    "{:.0} % packet loss ({} replies out of {} sent)",
                    burst.loss_ratio() * 100.0,
                    burst.received(),
                    burst.sent()
                ),
            );
        }

        if let Some(detail) = report.detail() {
            debug!(target_id = target.id, host = %options.host, detail, "sonde ICMP en échec");
        }
        Ok(report.finish())
    }
}

/// Résout l'hôte vers une adresse de la famille demandée.
async fn resolve(options: &Options) -> Result<IpAddr, (Failure, String)> {
    // Le port zéro n'a pas de sens pour ICMP : il n'est là que parce que
    // `lookup_host` attend une paire hôte/port.
    let addresses =
        tokio::time::timeout(options.timeout, tokio::net::lookup_host((options.host.as_str(), 0)))
            .await
            .map_err(|_| (Failure::Timeout, "name resolution too slow".to_string()))?
            .map_err(|error| (Failure::Dns, format!("{}: {error}", options.host)))?;

    let mut candidates = addresses.map(|address| address.ip());
    let found = match options.ip_version {
        IpVersion::Auto => candidates.next(),
        IpVersion::V4 => candidates.find(IpAddr::is_ipv4),
        IpVersion::V6 => candidates.find(IpAddr::is_ipv6),
    };

    found.ok_or_else(|| {
        (Failure::Dns, format!("\"{}\" resolves to no usable address", options.host))
    })
}

/// Ouvre la socket ICMP correspondant à la famille de l'adresse.
///
/// Un client par interrogation : il porte une socket et une tâche de réception,
/// et le mutualiser entre cibles ferait dépendre chaque sonde de la santé des
/// autres.
fn client_for(address: IpAddr) -> Result<Client, ProbeError> {
    let config = match address {
        IpAddr::V4(_) => Config::default(),
        IpAddr::V6(_) => Config::builder().kind(ICMP::V6).build(),
    };
    Client::new(&config).map_err(|error| stats::socket_error(&error))
}

/// Envoie la salve, séquentiellement, en s'arrêtant si le budget s'épuise.
async fn send_burst(client: &Client, address: IpAddr, target_id: i64, options: &Options) -> Burst {
    // L'identifiant ICMP distingue nos échos de ceux des autres sondes en vol.
    // Le dériver de la cible le rend stable et unique, sans tirage aléatoire.
    let mut pinger = client.pinger(address, PingIdentifier(target_id as u16)).await;
    pinger.timeout(options.packet_timeout);

    let payload = vec![0u8; options.payload_bytes as usize];
    let mut burst = Burst::default();

    for sequence in 0..options.count {
        burst.record_sent();
        match pinger.ping(PingSequence(sequence as u16), &payload).await {
            Ok((_, rtt)) => burst.record_reply(rtt),
            // Une absence de réponse est un paquet perdu, pas une erreur : c'est
            // exactement ce que la sonde est là pour compter.
            Err(SurgeError::Timeout { .. }) => {}
            Err(error) => {
                debug!(target_id, %address, %error, "écho ICMP en échec");
            }
        }

        if sequence + 1 < options.count && !options.interval.is_zero() {
            tokio::time::sleep(options.interval).await;
        }
    }

    burst
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collectors::uptime::tags::test_support::cible;

    #[test]
    fn le_collecteur_annonce_son_type() {
        assert_eq!(PingCollector::new().kind(), "ping");
    }

    #[test]
    fn des_reglages_intenables_sont_refuses_avant_douvrir_la_moindre_socket() {
        let collector = PingCollector::new();
        let error = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(collector.probe(&cible(
                "ping",
                "10.0.0.1",
                &[("count", "20"), ("packet_timeout_ms", "9000")],
            )))
            .unwrap_err();
        assert!(matches!(error, ProbeError::Config(_)));
        assert!(!error.means_down());
    }
}
