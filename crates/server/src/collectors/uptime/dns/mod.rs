//! Sonde de résolution DNS (`kind = "dns"`).
//!
//! Vérifie qu'un nom se résout, en combien de temps, et — c'est là tout l'intérêt —
//! **vers quoi**. Une zone détournée, un enregistrement effacé par une mauvaise
//! manipulation ou un basculement de bascule oublié se voient ici, alors qu'une
//! sonde HTTP les manquerait : elle suivrait la mauvaise adresse sans broncher.
//!
//! # Adresse et étiquettes
//!
//! Adresse : le nom à résoudre, `www.exemple.fr`.
//!
//! | Étiquette | Défaut | Rôle |
//! |---|---|---|
//! | `record_type` | `A` | `A`, `AAAA`, `CNAME`, `MX`, `TXT`, `NS`, `SOA`, `SRV`, `PTR`, `CAA`. |
//! | `resolver` | système | Adresse IP du résolveur, port facultatif (`1.1.1.1`, `10.0.0.1:5353`). |
//! | `expect` | — | Valeurs devant toutes figurer dans la réponse, séparées par des virgules. |
//! | `timeout_seconds` | `5` | Délai propre à la sonde (1 à 60). |

mod answer;
pub(crate) mod options;

use std::time::Instant;

use async_trait::async_trait;
use ezymonit_proto::{Collector, ProbeError, Sample, Target};
use hickory_resolver::config::{ConnectionConfig, NameServerConfig, ResolveHosts, ResolverConfig};
use hickory_resolver::net::NetError;
use hickory_resolver::net::runtime::TokioRuntimeProvider;
use hickory_resolver::{Resolver, TokioResolver};
use tracing::debug;

use super::outcome::{Failure, Report};
use options::Options;

/// Collecteur de disponibilité par résolution de nom.
#[derive(Default)]
pub struct DnsCollector;

impl DnsCollector {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Collector for DnsCollector {
    fn kind(&self) -> &'static str {
        "dns"
    }

    async fn probe(&self, target: &Target) -> Result<Vec<Sample>, ProbeError> {
        let options = Options::from_target(target)?;
        let resolver = build_resolver(&options)?;

        let mut report = Report::new(self.kind())
            .label("record_type", options.record_type.to_string())
            .label("resolver", options.resolver_label());

        resolve(&mut report, &resolver, &options).await;

        if let Some(detail) = report.detail() {
            debug!(target_id = target.id, name = %options.name, detail, "sonde DNS en échec");
        }
        Ok(report.finish())
    }
}

/// Interroge le résolveur et confronte la réponse aux attentes.
async fn resolve(report: &mut Report, resolver: &TokioResolver, options: &Options) {
    let started = Instant::now();
    let lookup = match resolver.lookup(options.name.as_str(), options.record_type).await {
        Ok(lookup) => lookup,
        Err(error) => {
            let (reason, detail) = classify(&error);
            report.fail(reason, detail);
            return;
        }
    };
    report.gauge("dns_lookup_seconds", started.elapsed().as_secs_f64());

    let records = lookup.answers();
    let matching = answer::count_of_type(records, options.record_type);
    report.gauge("dns_answer_records", matching as f64);

    // Une réponse sans le moindre enregistrement du type demandé est un `NOERROR`
    // vide : le nom existe, mais pas pour ce type. Le résolveur ne le signale pas
    // comme une erreur, et pourtant le service qui en dépend ne marchera pas.
    if answer::is_empty_answer(records, options.record_type) {
        report.fail(
            Failure::Record,
            format!("no {} record for \"{}\"", options.record_type, options.name),
        );
        return;
    }

    if options.expect.is_empty() {
        return;
    }

    let answers = answer::render(records);
    let missing = answer::missing(&answers, &options.expect);
    if !missing.is_empty() {
        report.fail(
            Failure::Record,
            format!(
                "expected values missing from the answer: {} (got: {})",
                missing.join(", "),
                answers.join(", ")
            ),
        );
    }
}

/// Construit un résolveur dédié à cette interrogation.
///
/// Il est reconstruit à chaque passage, et son cache est désactivé : un moniteur
/// doit poser la question au réseau à chaque fois. Un résolveur mutualisé
/// répondrait depuis son cache et mesurerait la latence de sa propre mémoire,
/// masquant aussi bien une panne du serveur DNS qu'un changement d'enregistrement.
fn build_resolver(options: &Options) -> Result<TokioResolver, ProbeError> {
    let mut builder = match options.resolver {
        Some(address) => {
            let mut udp = ConnectionConfig::udp();
            udp.port = address.port();
            let mut tcp = ConnectionConfig::tcp();
            tcp.port = address.port();
            let server = NameServerConfig::new(address.ip(), true, vec![udp, tcp]);
            Resolver::builder_with_config(
                ResolverConfig::from_parts(None, Vec::new(), vec![server]),
                TokioRuntimeProvider::default(),
            )
        }
        None => Resolver::builder_tokio().map_err(|error| {
            ProbeError::Config(format!(
                "no usable system resolver ({error}): set the one to query with the \
                 \"resolver\" tag, for example \"1.1.1.1\""
            ))
        })?,
    };

    {
        let opts = builder.options_mut();
        opts.cache_size = 0;
        // Une seule tentative : les reprises internes masqueraient une perte de
        // paquets qui est justement ce que l'on cherche à mesurer, et feraient
        // dépasser le délai de la sonde.
        opts.attempts = 1;
        opts.timeout = options.timeout;
        // `/etc/hosts` court-circuiterait la requête et la sonde ne testerait plus
        // le serveur DNS mais un fichier local.
        opts.use_hosts_file = ResolveHosts::Never;
        // Les `CNAME` intermédiaires sont conservés : ils font partie de ce que
        // l'utilisateur veut pouvoir attendre avec `expect`.
        opts.preserve_intermediates = true;
        opts.num_concurrent_reqs = 1;
    }

    builder.build().map_err(|error| ProbeError::Config(format!("unusable resolver: {error}")))
}

/// Traduit l'erreur du résolveur en raison exposée en métrique.
///
/// La distinction utile n'est pas technique mais opérationnelle : « le serveur DNS
/// ne répond pas » se corrige sur le serveur DNS, « le nom n'existe pas » se
/// corrige dans la zone.
fn classify(error: &NetError) -> (Failure, String) {
    let detail = error.to_string();
    match error {
        NetError::Timeout => (Failure::Timeout, "the resolver did not answer in time".to_string()),
        NetError::Io(_) | NetError::NoConnections | NetError::Busy => {
            (Failure::Connect, format!("resolver unreachable: {detail}"))
        }
        NetError::Dns(hickory_resolver::net::DnsError::NoRecordsFound(_)) => {
            (Failure::Record, detail)
        }
        _ => (Failure::Dns, detail),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collectors::uptime::tags::test_support::cible;

    #[test]
    fn le_collecteur_annonce_son_type() {
        assert_eq!(DnsCollector::new().kind(), "dns");
    }

    #[test]
    fn un_resolveur_explicite_donne_un_resolveur_construit_sans_reseau() {
        let options =
            Options::from_target(&cible("dns", "exemple.fr", &[("resolver", "9.9.9.9:5353")]))
                .unwrap();
        assert!(build_resolver(&options).is_ok(), "aucune socket n'est ouverte à la construction");
    }

    #[test]
    fn un_type_denregistrement_inconnu_est_refuse_avant_toute_requete() {
        let collector = DnsCollector::new();
        let error = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(collector.probe(&cible("dns", "exemple.fr", &[("record_type", "PIZZA")])))
            .unwrap_err();
        assert!(matches!(error, ProbeError::Config(_)));
        assert!(!error.means_down(), "une faute de frappe n'est pas une panne de service");
    }

    #[test]
    fn un_resolveur_muet_et_un_nom_absent_ne_se_confondent_pas() {
        assert_eq!(classify(&NetError::Timeout).0, Failure::Timeout);
        assert_eq!(classify(&NetError::NoConnections).0, Failure::Connect);
        assert_eq!(classify(&NetError::Msg("réponse tronquée".into())).0, Failure::Dns);
    }
}
