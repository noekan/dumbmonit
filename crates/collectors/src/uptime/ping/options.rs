//! Réglages de la sonde ICMP.

use std::time::Duration;

use dumbmonit_proto::{ProbeError, Target};

use crate::uptime::tags;

/// Nombre d'échos par interrogation. Quatre est la convention de `ping`, et le
/// minimum pour que le taux de perte veuille dire quelque chose.
const DEFAULT_COUNT: u32 = 4;

/// Délai d'attente d'une réponse, par paquet.
const DEFAULT_PACKET_TIMEOUT_MS: u32 = 1_000;

/// Espacement entre deux envois. Assez court pour tenir dans le budget, assez
/// long pour ne pas se faire limiter par un pare-feu.
const DEFAULT_INTERVAL_MS: u32 = 100;

/// Taille de la charge utile, celle de `ping` sous Linux.
const DEFAULT_PAYLOAD_BYTES: u32 = 56;

/// Famille d'adresses à interroger.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IpVersion {
    /// Première adresse renvoyée par la résolution, quelle que soit sa famille.
    Auto,
    V4,
    V6,
}

#[derive(Debug, Clone)]
pub struct Options {
    pub host: String,
    pub count: u32,
    pub packet_timeout: Duration,
    pub interval: Duration,
    pub payload_bytes: u32,
    pub ip_version: IpVersion,
    /// Taux de perte au-delà duquel la sonde est déclarée en échec, entre 0 et 1.
    /// Par défaut, seule une perte totale compte comme une indisponibilité — une
    /// perte partielle est visible dans la métrique et alertable séparément.
    pub loss_threshold: f64,
    /// Budget total de la sonde.
    pub timeout: Duration,
}

impl Options {
    pub fn from_target(target: &Target) -> Result<Self, ProbeError> {
        let host = target.address.trim().to_string();
        if host.is_empty() {
            return Err(ProbeError::Config(
                "the address must be the host to ping (name or IP address)".to_string(),
            ));
        }

        let options = Self {
            host,
            count: tags::parse_u32(target, "count", DEFAULT_COUNT, 1..=20)?,
            packet_timeout: Duration::from_millis(u64::from(tags::parse_u32(
                target,
                "packet_timeout_ms",
                DEFAULT_PACKET_TIMEOUT_MS,
                50..=10_000,
            )?)),
            interval: Duration::from_millis(u64::from(tags::parse_u32(
                target,
                "interval_ms",
                DEFAULT_INTERVAL_MS,
                0..=5_000,
            )?)),
            payload_bytes: tags::parse_u32(
                target,
                "payload_bytes",
                DEFAULT_PAYLOAD_BYTES,
                0..=1_400,
            )?,
            ip_version: parse_ip_version(tags::tag(target, "ip_version"))?,
            loss_threshold: f64::from(tags::parse_u32(
                target,
                "loss_threshold_percent",
                100,
                0..=100,
            )?) / 100.0,
            timeout: tags::parse_timeout(target, "timeout_seconds")?,
        };

        options.check_budget()?;
        Ok(options)
    }

    /// Durée maximale d'une salve, tous les paquets restant sans réponse.
    pub fn worst_case(&self) -> Duration {
        let waits = self.packet_timeout * self.count;
        let gaps = self.interval * self.count.saturating_sub(1);
        waits + gaps
    }

    /// Refuse une combinaison qui ne tiendrait pas dans le budget de la sonde.
    ///
    /// Le vérifier ici, et non en cours de route, évite le pire des scénarios : une
    /// salve tronquée par manque de temps, dont le taux de perte serait calculé sur
    /// deux paquets au lieu de dix et paraîtrait aberrant à l'utilisateur.
    fn check_budget(&self) -> Result<(), ProbeError> {
        if self.worst_case() <= self.timeout {
            return Ok(());
        }
        Err(ProbeError::Config(format!(
            "these settings can take up to {:.1} s while the probe timeout is {:.0} s: \
             lower \"count\" or \"packet_timeout_ms\", or raise \"timeout_seconds\"",
            self.worst_case().as_secs_f64(),
            self.timeout.as_secs_f64()
        )))
    }
}

fn parse_ip_version(raw: Option<&str>) -> Result<IpVersion, ProbeError> {
    match raw {
        None => Ok(IpVersion::Auto),
        Some(value) => match value.to_ascii_lowercase().as_str() {
            "auto" => Ok(IpVersion::Auto),
            "4" | "v4" | "ipv4" => Ok(IpVersion::V4),
            "6" | "v6" | "ipv6" => Ok(IpVersion::V6),
            other => Err(ProbeError::Config(format!(
                "\"ip_version\" expects \"auto\", \"4\" or \"6\", got \"{other}\""
            ))),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::uptime::tags::test_support::cible;

    fn options(address: &str, tags: &[(&str, &str)]) -> Result<Options, ProbeError> {
        Options::from_target(&cible("ping", address, tags))
    }

    #[test]
    fn les_valeurs_par_defaut_tiennent_dans_le_budget_de_la_sonde() {
        let options = options("10.0.0.1", &[]).unwrap();
        assert_eq!(options.count, 4);
        assert_eq!(options.ip_version, IpVersion::Auto);
        assert!((options.loss_threshold - 1.0).abs() < 1e-12);
        assert!(
            options.worst_case() <= options.timeout,
            "le pire cas ({:?}) doit rester sous le délai ({:?})",
            options.worst_case(),
            options.timeout
        );
    }

    #[test]
    fn une_salve_qui_ne_tiendrait_pas_dans_le_budget_est_refusee() {
        let error =
            options("10.0.0.1", &[("count", "20"), ("packet_timeout_ms", "5000")]).unwrap_err();
        assert!(matches!(error, ProbeError::Config(_)));
        assert!(error.to_string().contains("timeout_seconds"), "{error}");
        assert!(!error.means_down());

        // La même salve passe si l'on rallonge le budget en conséquence.
        assert!(
            options(
                "10.0.0.1",
                &[("count", "4"), ("packet_timeout_ms", "2000"), ("timeout_seconds", "30")]
            )
            .is_ok()
        );
    }

    #[test]
    fn le_pire_cas_compte_les_attentes_et_les_intervalles() {
        let options = options(
            "10.0.0.1",
            &[("count", "3"), ("packet_timeout_ms", "1000"), ("interval_ms", "500")],
        )
        .unwrap();
        // 3 attentes d'une seconde, 2 intervalles d'une demi-seconde.
        assert_eq!(options.worst_case(), Duration::from_millis(4_000));
    }

    #[test]
    fn la_famille_dadresse_accepte_les_ecritures_courantes() {
        for valeur in ["4", "v4", "IPv4"] {
            assert_eq!(options("x", &[("ip_version", valeur)]).unwrap().ip_version, IpVersion::V4);
        }
        for valeur in ["6", "v6", "IPV6"] {
            assert_eq!(options("x", &[("ip_version", valeur)]).unwrap().ip_version, IpVersion::V6);
        }
        assert!(options("x", &[("ip_version", "5")]).is_err());
    }

    #[test]
    fn le_seuil_de_perte_se_donne_en_pourcentage() {
        let reglages = options("10.0.0.1", &[("loss_threshold_percent", "25")]).unwrap();
        assert!((reglages.loss_threshold - 0.25).abs() < 1e-12);
        assert!(options("10.0.0.1", &[("loss_threshold_percent", "101")]).is_err());
    }

    #[test]
    fn une_adresse_vide_est_une_erreur_de_configuration() {
        assert!(matches!(options("   ", &[]).unwrap_err(), ProbeError::Config(_)));
    }
}
