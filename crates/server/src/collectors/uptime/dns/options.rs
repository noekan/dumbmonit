//! Réglages de la sonde DNS.

use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use dumbmonit_proto::{ProbeError, Target};
use hickory_resolver::proto::rr::RecordType;

use super::answer;
use crate::collectors::uptime::tags;

/// Port du DNS. Le résolveur peut écouter ailleurs (dnsdist, un `stubby` local),
/// d'où la possibilité d'écrire `10.0.0.1:5353`.
const DEFAULT_RESOLVER_PORT: u16 = 53;

#[derive(Debug, Clone)]
pub struct Options {
    /// Nom à résoudre, tel que saisi.
    pub name: String,
    pub record_type: RecordType,
    /// Résolveur imposé. `None` signifie « celui du système », lu dans
    /// `/etc/resolv.conf` — que Docker monte y compris dans une image `scratch`.
    pub resolver: Option<SocketAddr>,
    /// Valeurs devant toutes figurer dans la réponse. Vide signifie « aucune
    /// attente », la sonde se contente alors de vérifier que la résolution aboutit.
    pub expect: Vec<String>,
    pub timeout: Duration,
}

impl Options {
    pub fn from_target(target: &Target) -> Result<Self, ProbeError> {
        let name = target.address.trim().to_string();
        if name.is_empty() {
            return Err(ProbeError::Config(
                "the address must be the name to resolve (for example \"www.example.com\")"
                    .to_string(),
            ));
        }

        Ok(Self {
            name,
            record_type: answer::parse_record_type(
                tags::tag(target, "record_type").unwrap_or("A"),
            )?,
            resolver: tags::tag(target, "resolver").map(parse_resolver).transpose()?,
            expect: tags::tag(target, "expect").map(tags::split_list).unwrap_or_default(),
            timeout: tags::parse_timeout(target, "timeout_seconds")?,
        })
    }

    /// Libellé du résolveur pour l'étiquetage. Cardinalité : une valeur par cible.
    pub fn resolver_label(&self) -> String {
        self.resolver.map_or_else(|| "system".to_string(), |address| address.ip().to_string())
    }
}

/// Analyse l'adresse d'un résolveur : `1.1.1.1`, `1.1.1.1:5353`, `[2606:4700::1111]:53`.
///
/// Seule une adresse IP est acceptée, jamais un nom : résoudre le résolveur avant
/// de résoudre le nom demanderait un résolveur, et l'erreur serait incompréhensible.
fn parse_resolver(raw: &str) -> Result<SocketAddr, ProbeError> {
    if let Ok(address) = raw.parse::<SocketAddr>() {
        return Ok(address);
    }
    let (host, port) = tags::split_host_port(raw, DEFAULT_RESOLVER_PORT)?;
    let ip: IpAddr = host.parse().map_err(|_| {
        ProbeError::Config(format!(
            "\"resolver\" expects the IP address of a DNS server (for example \"1.1.1.1\" \
             or \"10.0.0.1:5353\"), got \"{raw}\""
        ))
    })?;
    Ok(SocketAddr::new(ip, port))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collectors::uptime::tags::test_support::cible;

    fn options(address: &str, tags: &[(&str, &str)]) -> Result<Options, ProbeError> {
        Options::from_target(&cible("dns", address, tags))
    }

    #[test]
    fn un_nom_seul_interroge_le_resolveur_du_systeme_en_a() {
        let options = options("www.exemple.fr", &[]).unwrap();
        assert_eq!(options.name, "www.exemple.fr");
        assert_eq!(options.record_type, RecordType::A);
        assert!(options.resolver.is_none());
        assert!(options.expect.is_empty());
        assert_eq!(options.resolver_label(), "system");
        assert_eq!(options.timeout, Duration::from_secs(5));
    }

    #[test]
    fn le_resolveur_accepte_les_formes_avec_et_sans_port() {
        let cas = [
            ("1.1.1.1", "1.1.1.1:53"),
            ("10.0.0.1:5353", "10.0.0.1:5353"),
            ("2606:4700:4700::1111", "[2606:4700:4700::1111]:53"),
            ("[2606:4700:4700::1111]:5353", "[2606:4700:4700::1111]:5353"),
        ];
        for (saisie, attendu) in cas {
            let options = options("exemple.fr", &[("resolver", saisie)]).unwrap();
            assert_eq!(options.resolver.unwrap().to_string(), attendu, "pour « {saisie} »");
        }
    }

    #[test]
    fn un_resolveur_designe_par_son_nom_est_refuse_avec_une_explication() {
        let error = options("exemple.fr", &[("resolver", "dns.exemple.fr")]).unwrap_err();
        assert!(matches!(error, ProbeError::Config(_)));
        assert!(error.to_string().contains("IP address"), "{error}");
    }

    #[test]
    fn letiquette_du_resolveur_ne_porte_pas_le_port() {
        // Le port ne distingue pas deux séries utiles et gonflerait l'étiquette
        // sans rien apprendre à personne.
        let options = options("exemple.fr", &[("resolver", "9.9.9.9:5353")]).unwrap();
        assert_eq!(options.resolver_label(), "9.9.9.9");
    }

    #[test]
    fn les_valeurs_attendues_forment_une_liste() {
        let options = options("exemple.fr", &[("expect", "203.0.113.10, 203.0.113.11 ,")]).unwrap();
        assert_eq!(options.expect, vec!["203.0.113.10", "203.0.113.11"]);
    }

    #[test]
    fn un_nom_vide_est_une_erreur_de_configuration() {
        let error = options("  ", &[]).unwrap_err();
        assert!(matches!(error, ProbeError::Config(_)));
        assert!(!error.means_down());
    }
}
