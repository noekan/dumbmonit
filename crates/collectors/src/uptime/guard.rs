//! Garde-fou contre les cibles qui n'en sont pas : boucle locale, lien local,
//! service de métadonnées d'un fournisseur de cloud.
//!
//! Un moniteur HTTP ou TCP se connecte à l'adresse que l'administrateur lui
//! donne, et lui rapporte ce qu'il a vu. C'est le but — mais c'est aussi, mot
//! pour mot, la définition d'une falsification de requête côté serveur : rien
//! ne distingue `http://nas.lan/health` de `http://169.254.169.254/latest/…` ou
//! de `http://127.0.0.1:8428/api/v1/admin/…`, le VictoriaMetrics embarqué, qui
//! n'a pas d'authentification. Un administrateur de la supervision n'est pas
//! forcément administrateur de l'hôte ni du compte cloud.
//!
//! La règle est donc : **ce qui est joignable depuis un autre poste du réseau
//! est autorisé, ce qui n'est joignable que depuis l'hôte lui-même est refusé**,
//! sauf demande explicite (`allow_private_targets = true` sur la cible). Les
//! plages privées (RFC 1918, ULA) restent ouvertes — un homelab surveille des
//! adresses privées, c'est même l'essentiel de son parc.
//!
//! Le refus est une erreur de configuration ([`ProbeError::Config`]) : elle
//! s'affiche, ne déclenche pas d'alerte de panne, et dit quelle option activer.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use dumbmonit_proto::{ProbeError, Target};

use super::tags;

/// Étiquette qui lève le garde-fou sur une cible.
pub const OPTION: &str = "allow_private_targets";

/// Adresse qu'un moniteur ne doit pas atteindre sans autorisation explicite.
///
/// Sont refusées : la boucle locale, les adresses de lien local (dont le service
/// de métadonnées `169.254.169.254`), l'adresse non spécifiée, l'adresse de
/// métadonnées IPv6 d'AWS (`fd00:ec2::254`), et les formes IPv4 projetées en
/// IPv6 (`::ffff:127.0.0.1`) de tout cela.
pub fn is_forbidden(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_forbidden_v4(v4),
        IpAddr::V6(v6) => {
            if let Some(mapped) = v6.to_ipv4_mapped() {
                return is_forbidden_v4(mapped);
            }
            v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_unicast_link_local()
                || v6 == Ipv6Addr::new(0xfd00, 0x0ec2, 0, 0, 0, 0, 0, 0x254)
        }
    }
}

fn is_forbidden_v4(ip: Ipv4Addr) -> bool {
    ip.is_loopback() || ip.is_unspecified() || ip.is_link_local()
}

/// Lit l'option sur la cible : `true` lève le garde-fou.
pub fn allowed(target: &Target) -> Result<bool, ProbeError> {
    tags::parse_bool(target, OPTION, false)
}

/// Refus, rédigé pour l'écran de configuration.
pub fn refusal(host: &str, ip: IpAddr) -> ProbeError {
    ProbeError::Config(format!(
        "\"{host}\" resolves to {ip}, a loopback or link-local address that only the \
         monitoring host itself can reach. Enable the option \"{OPTION}\" on this target if \
         you really mean to monitor a service running on the DumbMonit host."
    ))
}

/// Vérifie une liste d'adresses résolues : la première interdite fait échouer.
///
/// *Toutes* les adresses comptent, pas seulement celle qui sera utilisée : un nom
/// qui alterne entre une adresse publique et la boucle locale est précisément le
/// tour de passe-passe (« DNS rebinding ») que ce garde doit déjouer.
pub fn vet(host: &str, addresses: &[SocketAddr], allow_private: bool) -> Result<(), ProbeError> {
    if allow_private {
        return Ok(());
    }
    match addresses.iter().map(SocketAddr::ip).find(|ip| is_forbidden(*ip)) {
        Some(ip) => Err(refusal(host, ip)),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::uptime::tags::test_support::cible;

    fn ip(text: &str) -> IpAddr {
        text.parse().expect("adresse de test valide")
    }

    #[test]
    fn la_boucle_locale_et_le_lien_local_sont_refuses() {
        for adresse in [
            "127.0.0.1",
            "127.1.2.3",
            "0.0.0.0",
            "169.254.169.254",
            "169.254.0.1",
            "::1",
            "::",
            "fe80::1",
            "fd00:ec2::254",
            "::ffff:127.0.0.1",
            "::ffff:169.254.169.254",
        ] {
            assert!(is_forbidden(ip(adresse)), "{adresse} aurait dû être refusée");
        }
    }

    #[test]
    fn les_adresses_privees_dun_homelab_restent_autorisees() {
        for adresse in [
            "10.0.0.5",
            "172.16.4.2",
            "192.168.1.10",
            "100.64.0.1",
            "93.184.216.34",
            "fd00::1",
            "fc00::5",
            "2001:db8::1",
            "::ffff:192.168.1.10",
        ] {
            assert!(!is_forbidden(ip(adresse)), "{adresse} aurait dû passer");
        }
    }

    #[test]
    fn le_refus_est_une_erreur_de_configuration_qui_nomme_loption() {
        let error = vet("nas.lan", &["127.0.0.1:80".parse().unwrap()], false).unwrap_err();
        assert!(matches!(error, ProbeError::Config(_)));
        assert!(!error.means_down(), "un refus n'est pas une panne du service");
        let message = error.to_string();
        assert!(message.contains(OPTION), "{message}");
        assert!(message.contains("127.0.0.1"), "{message}");
    }

    #[test]
    fn une_seule_adresse_interdite_parmi_dautres_suffit_a_refuser() {
        let adresses: Vec<SocketAddr> =
            vec!["93.184.216.34:80".parse().unwrap(), "127.0.0.1:80".parse().unwrap()];
        assert!(vet("x", &adresses, false).is_err());
        assert!(vet("x", &adresses, true).is_ok(), "l'option lève le garde-fou");
        assert!(vet("x", &adresses[..1], false).is_ok());
    }

    #[test]
    fn loption_se_lit_sur_la_cible() {
        assert!(!allowed(&cible("http", "x", &[])).unwrap());
        assert!(allowed(&cible("http", "x", &[(OPTION, "true")])).unwrap());
        assert!(allowed(&cible("http", "x", &[(OPTION, "peut-être")])).is_err());
    }
}
