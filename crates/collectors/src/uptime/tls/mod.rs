//! Sonde d'expiration de certificat TLS (`kind = "tls"`).
//!
//! # Pourquoi une sonde distincte de HTTP
//!
//! La sonde HTTP relève déjà le certificat quand l'URL est en `https`, ce qui
//! couvre le cas courant. Une sonde autonome reste nécessaire pour trois raisons :
//!
//! * **tout ce qui présente un certificat ne parle pas HTTP** : SMTPS (465),
//!   IMAPS (993), LDAPS (636), MQTTS (8883), un PostgreSQL derrière TLS… Il n'y a
//!   pas d'URL à interroger, seulement une poignée de main à observer ;
//! * **on ne veut pas toujours envoyer du trafic applicatif** sur un service
//!   qu'on surveille — une API facturée à l'appel, une passerelle de paiement ;
//! * **elle continue de répondre quand l'application est tombée** : le terminateur
//!   TLS répond encore, et savoir que le certificat va expirer reste utile pendant
//!   l'incident.
//!
//! Le coût de cette autonomie est nul : toute la mécanique est déjà écrite pour la
//! sonde HTTP, `kind = "tls"` ne fait que l'exposer seule.
//!
//! # Adresse et étiquettes
//!
//! Adresse : `hôte`, `hôte:port` ou `[ipv6]:port`. Port 443 par défaut.
//!
//! | Étiquette | Défaut | Rôle |
//! |---|---|---|
//! | `server_name` | l'hôte | Nom envoyé en SNI, à préciser derrière un proxy inverse. |
//! | `insecure_tls` | `false` | Une chaîne non vérifiable ne fait plus échouer la sonde. |
//! | `allow_private_targets` | `false` | Autorise la boucle locale et le lien local (voir `guard`). |
//! | `timeout_seconds` | `5` | Délai propre à la sonde (1 à 60). |

pub(crate) mod cert;
pub(crate) mod handshake;
pub(crate) mod options;

use std::time::Duration;

use async_trait::async_trait;
use dumbmonit_proto::{Collector, ProbeError, Sample, Target};
use tracing::debug;

use super::guard;
use super::outcome::{Failure, Report};
use handshake::HandshakeError;
use options::Options;

/// Collecteur de disponibilité par observation de la poignée de main TLS.
#[derive(Default)]
pub struct TlsCollector;

impl TlsCollector {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Collector for TlsCollector {
    fn kind(&self) -> &'static str {
        "tls"
    }

    async fn probe(&self, target: &Target) -> Result<Vec<Sample>, ProbeError> {
        let options = Options::from_target(target)?;
        let allow_private = guard::allowed(target)?;

        let mut report = Report::new(self.kind())
            .label("port", options.port.to_string())
            .label("server_name", options.server_name.clone());

        measure(
            &mut report,
            &options.host,
            options.port,
            &options.server_name,
            options.allow_untrusted,
            allow_private,
            options.timeout,
        )
        .await?;

        if let Some(detail) = report.detail() {
            debug!(target_id = target.id, host = %options.host, port = options.port, detail,
                "sonde TLS en échec");
        }
        Ok(report.finish())
    }
}

/// Relève le certificat présenté et alimente le rapport.
///
/// Partagée avec la sonde HTTP, qui l'appelle sur les URL en `https` : le certificat
/// est ainsi rapporté de la même façon et sous les mêmes noms de métriques, quelle
/// que soit la sonde par laquelle on l'observe.
///
/// `Err` seulement quand l'adresse est refusée par le garde-fou (`guard`) : c'est
/// une erreur de configuration, pas une mesure.
pub(super) async fn measure(
    report: &mut Report,
    host: &str,
    port: u16,
    server_name: &str,
    allow_untrusted: bool,
    allow_private: bool,
    timeout: Duration,
) -> Result<(), ProbeError> {
    let observed = match handshake::inspect(host, port, server_name, allow_private, timeout).await {
        Ok(observed) => observed,
        Err(HandshakeError::Refused(error)) => return Err(error),
        Err(error) => {
            let (reason, detail) = classify(error);
            report.fail(reason, detail);
            return Ok(());
        }
    };

    report.gauge("connect_seconds", observed.connect.as_secs_f64());
    report.gauge("tls_handshake_seconds", observed.tls.as_secs_f64());
    if let Some(version) = &observed.version {
        report.gauge_with("tls_version_info", 1.0, "version", version);
    }

    let info = match cert::parse_der(&observed.leaf_der) {
        Ok(info) => info,
        Err(detail) => {
            report.fail(Failure::Tls, detail);
            return Ok(());
        }
    };

    let now_s = report.timestamp_ms() / 1_000;
    report.gauge("ssl_cert_expiry_days", info.days_until_expiry(now_s));
    report.gauge("ssl_cert_valid", if observed.trusted { 1.0 } else { 0.0 });
    report.gauge_with("ssl_cert_issuer_info", 1.0, "issuer", &info.issuer);

    // Un certificat périmé est un échec quoi qu'il arrive : c'est une panne
    // effective pour tout client normalement configuré, y compris quand
    // l'utilisateur a accepté les autorités privées.
    if info.is_expired_at(now_s) {
        report.fail(
            Failure::CertExpired,
            format!(
                "certificate \"{}\" is outside its validity period ({:.1} days)",
                info.subject,
                info.days_until_expiry(now_s)
            ),
        );
        return Ok(());
    }

    if !observed.trusted && !allow_untrusted {
        let motif = observed.trust_error.as_deref().unwrap_or("chain cannot be verified");
        report.fail(
            Failure::Tls,
            format!(
                "certificate rejected: {motif}. Add the tag \"insecure_tls = true\" if \
                 this target uses a private authority."
            ),
        );
    }
    Ok(())
}

/// Traduit un échec de poignée de main en raison exposée en métrique.
fn classify(error: HandshakeError) -> (Failure, String) {
    match error {
        HandshakeError::Resolve(detail) => (Failure::Dns, detail),
        HandshakeError::Connect(detail) => (Failure::Connect, detail),
        HandshakeError::Timeout => (Failure::Timeout, "probe timed out".to_string()),
        HandshakeError::Tls(detail) => (Failure::Tls, detail),
        // Traitée avant d'arriver ici : voir `measure`.
        HandshakeError::Refused(error) => (Failure::Connect, error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::uptime::tags::test_support::cible;

    #[test]
    fn le_collecteur_annonce_son_type() {
        assert_eq!(TlsCollector::new().kind(), "tls");
    }

    #[test]
    fn une_adresse_illisible_est_une_erreur_de_configuration_et_pas_une_panne() {
        let collector = TlsCollector::new();
        let error = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(collector.probe(&cible("tls", "   ", &[])))
            .unwrap_err();
        assert!(matches!(error, ProbeError::Config(_)));
        assert!(!error.means_down(), "une adresse vide n'est pas un service en panne");
    }

    #[test]
    fn chaque_echec_de_poignee_de_main_a_sa_raison() {
        let cas = [
            (HandshakeError::Resolve("x".into()), Failure::Dns),
            (HandshakeError::Connect("x".into()), Failure::Connect),
            (HandshakeError::Timeout, Failure::Timeout),
            (HandshakeError::Tls("x".into()), Failure::Tls),
        ];
        for (error, attendu) in cas {
            assert_eq!(classify(error).0, attendu);
        }
    }
}
