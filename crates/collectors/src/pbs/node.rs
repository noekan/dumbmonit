//! Ce que le nœud dit de lui-même, au-delà de sa charge.
//!
//! Quatre inventaires que la sonde lisait jusqu'ici à travers d'autres : les
//! unités systemd qui doivent tourner, les versions des paquets Proxmox
//! (installée, disponible, en exécution), les certificats servis par
//! l'interface et les règles de limitation de débit avec ce qu'elles
//! transportent à l'instant.
//!
//! Tout est pur ici : les réponses sont converties, jamais demandées.

use dumbmonit_proto::Sample;

use super::metrics::gauge;
use super::model::{CertificateInfo, PackageVersion, ServiceEntry, TrafficRule, parse_rate};
use super::view::{CertificateView, PackageView, ServiceView, TrafficRuleView};

/// Les unités qu'un serveur de sauvegarde doit avoir en marche.
///
/// PBS en liste une vingtaine, dont `postfix` ou `systemd-timesyncd` : toutes
/// produisent leur série, mais seules celles-ci portent `expected = "1"`, et
/// seules celles-là déclenchent la règle intégrée. Un homelab qui arrête
/// `postfix` n'a pas à être réveillé pour ça.
const EXPECTED_SERVICES: &[&str] =
    &["proxmox-backup", "proxmox-backup-proxy", "proxmox-backup-banner"];

/// Paquets dont la version mérite une série. Les autres — une centaine sur une
/// installation ordinaire — feraient autant d'étiquettes sans rien apprendre.
const WATCHED_PACKAGES: &[&str] =
    &["proxmox-backup", "proxmox-backup-server", "proxmox-backup-client", "proxmox-kernel-helper"];

/// `GET /nodes/localhost/services`.
///
/// Une liste vide ne produit aucune série : le serveur n'a rien dit, ce n'est
/// pas « toutes les unités sont éteintes ». Une unité sans nom ne peut pas
/// porter d'étiquette stable : ignorée.
pub fn service_samples(services: &[ServiceEntry], ts_ms: i64) -> Vec<Sample> {
    let mut samples = Vec::new();
    let mut counted = 0usize;

    for service in services.iter().filter(|s| !s.service.is_empty()) {
        counted += 1;
        let expected = EXPECTED_SERVICES.contains(&service.service.as_str());
        let mut push = |sample: Sample| {
            samples.push(
                sample
                    .with_label("service", service.service.clone())
                    .with_label("expected", if expected { "1" } else { "0" }),
            );
        };
        push(gauge("node_service_active", if service.is_running() { 1.0 } else { 0.0 }, ts_ms));
        if let Some(enabled) = service.is_enabled() {
            push(gauge("node_service_enabled", if enabled { 1.0 } else { 0.0 }, ts_ms));
        }
    }

    if counted > 0 {
        samples.push(gauge("node_services_total", counted as f64, ts_ms));
    }
    samples
}

pub fn service_views(services: &[ServiceEntry]) -> Vec<ServiceView> {
    services
        .iter()
        .filter(|s| !s.service.is_empty())
        .map(|service| ServiceView {
            service: service.service.clone(),
            description: service.desc.clone().or_else(|| service.name.clone()),
            state: service.state.clone(),
            unit_state: service.unit_state.clone(),
            running: service.is_running(),
            enabled: service.is_enabled(),
        })
        .collect()
}

/// `GET /nodes/localhost/apt/versions`.
///
/// Trois questions en une : le paquet est-il à jour dans le dépôt, la version
/// installée est-elle celle qui tourne, et laquelle est-ce. La dernière passe
/// par une série de présence `*_info`, dont seules les étiquettes comptent.
pub fn package_samples(packages: &[PackageVersion], ts_ms: i64) -> Vec<Sample> {
    let mut samples = Vec::new();
    let mut stale = None;

    for package in packages.iter().filter(|p| WATCHED_PACKAGES.contains(&p.package.as_str())) {
        let label = package.package.clone();
        samples.push(
            gauge(
                "node_package_upgradable",
                if package.is_upgradable() { 1.0 } else { 0.0 },
                ts_ms,
            )
            .with_label("package", label.clone()),
        );
        // APT écrit littéralement « unknown » pour un méta-paquet dont il ne
        // connaît pas la version : une étiquette vide dit la même chose sans
        // faire passer un aveu d'ignorance pour un numéro de version.
        let known =
            |value: Option<&str>| value.filter(|v| *v != "unknown").unwrap_or_default().to_string();
        samples.push(
            gauge("node_package_info", 1.0, ts_ms)
                .with_label("package", label.clone())
                .with_label("installed", known(package.installed.as_deref()))
                .with_label("available", known(package.available.as_deref()))
                .with_label("running", known(package.running_version())),
        );

        // Seul le démon PBS mérite le verdict « redémarrage en attente » : le
        // noyau, lui, demande un redémarrage complet, que la règle sur les
        // mises à jour couvre déjà.
        if package.package == "proxmox-backup-server"
            && let (Some(installed), Some(running)) =
                (package.installed.as_deref(), package.running_version())
            && let Some(verdict) = super::model::running_version_is_stale(installed, running)
        {
            stale = Some(verdict);
        }
    }

    if let Some(stale) = stale {
        samples.push(gauge("node_running_version_stale", if stale { 1.0 } else { 0.0 }, ts_ms));
    }
    samples
}

pub fn package_views(packages: &[PackageVersion]) -> Vec<PackageView> {
    packages
        .iter()
        .filter(|p| WATCHED_PACKAGES.contains(&p.package.as_str()))
        .map(|package| PackageView {
            package: package.package.clone(),
            title: package.title.clone().filter(|t| t != "unknown"),
            installed: package.installed.clone().filter(|v| v != "unknown"),
            available: package.available.clone().filter(|v| v != "unknown"),
            running: package.running_version().map(str::to_string),
            upgradable: package.is_upgradable(),
            restart_pending: match (package.installed.as_deref(), package.running_version()) {
                (Some(installed), Some(running)) => {
                    super::model::running_version_is_stale(installed, running)
                }
                _ => None,
            },
        })
        .collect()
}

/// `GET /nodes/localhost/certificates/info`.
///
/// Un certificat sans date de fin ne produit rien : mieux vaut pas de série
/// qu'un « expire dans zéro seconde » inventé. Une valeur négative est publiée
/// telle quelle — un certificat déjà expiré est précisément ce qu'il faut voir.
pub fn certificate_samples(
    certificates: &[CertificateInfo],
    now_s: i64,
    ts_ms: i64,
) -> Vec<Sample> {
    certificates
        .iter()
        .filter_map(|certificate| {
            let not_after = certificate.notafter?.0 as i64;
            let filename = certificate.filename.clone().unwrap_or_else(|| "proxy.pem".to_string());
            Some(
                gauge("node_certificate_expires_seconds", (not_after - now_s) as f64, ts_ms)
                    .with_label("certificate", filename)
                    .with_label("issuer", short_dn(certificate.issuer.as_deref())),
            )
        })
        .collect()
}

/// Le nom commun d'un sujet ou d'un émetteur X.509, pour tenir dans une
/// étiquette. PBS le donne déjà découpé en lignes `CN=…`.
fn short_dn(dn: Option<&str>) -> String {
    let Some(dn) = dn else { return String::new() };
    dn.split(['\n', ','])
        .filter_map(|part| part.trim().strip_prefix("CN="))
        .next()
        .unwrap_or_else(|| dn.trim())
        .trim()
        .to_string()
}

pub fn certificate_views(certificates: &[CertificateInfo]) -> Vec<CertificateView> {
    certificates
        .iter()
        .map(|certificate| CertificateView {
            filename: certificate.filename.clone().unwrap_or_else(|| "proxy.pem".to_string()),
            subject: certificate.subject.clone(),
            issuer: certificate.issuer.clone(),
            fingerprint: certificate.fingerprint.clone(),
            not_after: certificate.notafter.map(|n| n.0 as i64),
            san: certificate.san.clone(),
        })
        .collect()
}

/// `GET /admin/traffic-control`.
///
/// Le plafond configuré et le débit courant, dans les deux sens. Sans plafond
/// dans un sens, PBS n'écrit rien : la série manque plutôt que d'annoncer une
/// limite de zéro, qui voudrait dire « tout est bloqué ».
pub fn traffic_samples(rules: &[TrafficRule], ts_ms: i64) -> Vec<Sample> {
    let mut samples = Vec::new();
    let mut counted = 0usize;

    for rule in rules.iter().filter(|r| !r.name.is_empty()) {
        counted += 1;
        let mut push = |sample: Sample| samples.push(sample.with_label("rule", rule.name.clone()));
        if let Some(rate) = rule.cur_rate_in {
            push(gauge("traffic_rate_in_bytes", rate.0, ts_ms));
        }
        if let Some(rate) = rule.cur_rate_out {
            push(gauge("traffic_rate_out_bytes", rate.0, ts_ms));
        }
        if let Some(limit) = rule.rate_in.as_deref().and_then(parse_rate) {
            push(gauge("traffic_limit_in_bytes", limit, ts_ms));
        }
        if let Some(limit) = rule.rate_out.as_deref().and_then(parse_rate) {
            push(gauge("traffic_limit_out_bytes", limit, ts_ms));
        }
    }

    samples.push(gauge("traffic_rules_total", counted as f64, ts_ms));
    samples
}

pub fn traffic_views(rules: &[TrafficRule]) -> Vec<TrafficRuleView> {
    rules
        .iter()
        .filter(|r| !r.name.is_empty())
        .map(|rule| TrafficRuleView {
            name: rule.name.clone(),
            comment: rule.comment.clone(),
            networks: rule.network.clone(),
            timeframe: rule.timeframe.clone(),
            limit_in_bytes: rule.rate_in.as_deref().and_then(parse_rate),
            limit_out_bytes: rule.rate_out.as_deref().and_then(parse_rate),
            rate_in_bytes: rule.cur_rate_in.map(|n| n.0),
            rate_out_bytes: rule.cur_rate_out.map(|n| n.0),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pbs::model::Envelope;

    /// Copie de `GET /nodes/localhost/services` d'un PBS 4 (extrait).
    const SERVICES: &str = r#"{"data":[
      {"desc":"Proxmox Backup API Server","name":"proxmox-backup.service","service":"proxmox-backup","state":"running","unit-state":"enabled"},
      {"desc":"Proxmox Backup API Proxy Server","name":"proxmox-backup-proxy.service","service":"proxmox-backup-proxy","state":"dead","unit-state":"enabled"},
      {"desc":"Postfix Mail Transport Agent","name":"postfix.service","service":"postfix","state":"dead","unit-state":"disabled"},
      {"desc":"Set Console Banner","name":"proxmox-backup-banner.service","service":"proxmox-backup-banner","state":"dead","unit-state":"static"}
    ]}"#;

    /// Copie de `GET /nodes/localhost/apt/versions` d'un PBS 4.2.6, avec une
    /// mise à niveau disponible sur le démon.
    const VERSIONS: &str = r#"{"data":[
      {"Arch":"unknown","ExtraInfo":"running kernel: 6.8.12-4-pve","Origin":"unknown","Package":"proxmox-backup","Title":"unknown","Version":"unknown"},
      {"Arch":"amd64","ExtraInfo":"running version: 4.2.6","OldVersion":"4.2.6-1","Origin":"Proxmox","Package":"proxmox-backup-server","Title":"Proxmox Backup Server daemon with tools and GUI","Version":"4.3.1-1"},
      {"Arch":"amd64","OldVersion":"4.2.6-1","Origin":"Proxmox","Package":"proxmox-backup-client","Title":"Proxmox Backup Client tools","Version":"4.2.6-1"},
      {"Arch":"amd64","OldVersion":"7.0.0-7","Origin":"Proxmox","Package":"libjs-extjs","Title":"cross-browser JavaScript library","Version":"7.0.0-7"}
    ]}"#;

    /// Copie de `GET /admin/traffic-control`.
    const TRAFFIC: &str = r#"{"data":[
      {"comment":"WAN cap","cur-rate-in":1048576,"cur-rate-out":0,"name":"tc-wan","network":["0.0.0.0/0"],"rate-in":"100 MB","rate-out":"50 MB"}
    ]}"#;

    fn parse<T: serde::de::DeserializeOwned>(raw: &str) -> T {
        serde_json::from_str::<Envelope<T>>(raw).unwrap().data
    }

    fn value(samples: &[Sample], metric: &str, label: (&str, &str)) -> Option<f64> {
        samples
            .iter()
            .find(|s| s.metric == metric && s.labels.get(label.0).is_some_and(|v| v == label.1))
            .map(|s| s.value)
    }

    #[test]
    fn une_unite_arretee_donne_zero_et_les_unites_attendues_sont_marquees() {
        let services: Vec<ServiceEntry> = parse(SERVICES);
        let samples = service_samples(&services, 1_000);

        assert_eq!(
            value(&samples, "pbs_node_service_active", ("service", "proxmox-backup")),
            Some(1.0)
        );
        assert_eq!(
            value(&samples, "pbs_node_service_active", ("service", "proxmox-backup-proxy")),
            Some(0.0),
            "le mandataire arrêté est la panne que la règle doit voir"
        );
        assert_eq!(
            value(&samples, "pbs_node_service_active", ("service", "postfix")),
            Some(0.0),
            "postfix produit sa série comme les autres"
        );

        let expected = |service: &str| {
            samples
                .iter()
                .find(|s| {
                    s.metric == "pbs_node_service_active"
                        && s.labels.get("service").is_some_and(|v| v == service)
                })
                .and_then(|s| s.labels.get("expected").cloned())
        };
        assert_eq!(expected("proxmox-backup-proxy").as_deref(), Some("1"));
        assert_eq!(expected("postfix").as_deref(), Some("0"), "postfix ne doit réveiller personne");

        // Une unité `static` n'est ni activée ni désactivée : pas de série.
        assert!(
            value(&samples, "pbs_node_service_enabled", ("service", "proxmox-backup-banner"))
                .is_none()
        );
        assert_eq!(value(&samples, "pbs_node_service_enabled", ("service", "postfix")), Some(0.0));
        assert_eq!(
            samples.iter().find(|s| s.metric == "pbs_node_services_total").map(|s| s.value),
            Some(4.0)
        );
    }

    #[test]
    fn une_liste_dunites_vide_ne_produit_rien() {
        // Un serveur qui ne répond rien n'est pas un serveur dont tout est
        // éteint : aucune série, donc aucune alerte.
        assert!(service_samples(&[], 1_000).is_empty());
        assert!(service_views(&[]).is_empty());
    }

    #[test]
    fn la_vue_des_unites_reprend_letat_et_lactivation() {
        let services: Vec<ServiceEntry> = parse(SERVICES);
        let views = service_views(&services);
        let proxy = views.iter().find(|v| v.service == "proxmox-backup-proxy").unwrap();
        assert!(!proxy.running);
        assert_eq!(proxy.enabled, Some(true));
        assert_eq!(proxy.description.as_deref(), Some("Proxmox Backup API Proxy Server"));
        let banner = views.iter().find(|v| v.service == "proxmox-backup-banner").unwrap();
        assert_eq!(banner.enabled, None, "une unité statique n'a pas d'avis");
    }

    #[test]
    fn un_paquet_a_mettre_a_niveau_se_distingue_dun_paquet_a_jour() {
        let packages: Vec<PackageVersion> = parse(VERSIONS);
        let samples = package_samples(&packages, 1_000);

        assert_eq!(
            value(&samples, "pbs_node_package_upgradable", ("package", "proxmox-backup-server")),
            Some(1.0)
        );
        assert_eq!(
            value(&samples, "pbs_node_package_upgradable", ("package", "proxmox-backup-client")),
            Some(0.0)
        );
        assert!(
            value(&samples, "pbs_node_package_upgradable", ("package", "libjs-extjs")).is_none(),
            "seuls les paquets suivis produisent une série"
        );
    }

    #[test]
    fn la_version_en_execution_se_compare_a_la_version_installee() {
        let packages: Vec<PackageVersion> = parse(VERSIONS);
        let samples = package_samples(&packages, 1_000);
        // Installée 4.2.6-1, en exécution 4.2.6 : la révision Debian ne compte
        // pas, le démon est bien celui qui est installé.
        assert_eq!(
            samples.iter().find(|s| s.metric == "pbs_node_running_version_stale").map(|s| s.value),
            Some(0.0)
        );

        let mut stale = packages;
        for package in &mut stale {
            if package.package == "proxmox-backup-server" {
                package.installed = Some("4.3.1-1".into());
            }
        }
        let samples = package_samples(&stale, 1_000);
        assert_eq!(
            samples.iter().find(|s| s.metric == "pbs_node_running_version_stale").map(|s| s.value),
            Some(1.0),
            "paquet mis à niveau, démon non redémarré"
        );
    }

    #[test]
    fn sans_version_en_execution_aucun_verdict_nest_invente() {
        let packages = vec![PackageVersion {
            package: "proxmox-backup-server".into(),
            installed: Some("4.2.6-1".into()),
            available: Some("4.2.6-1".into()),
            ..Default::default()
        }];
        let samples = package_samples(&packages, 1_000);
        assert!(
            !samples.iter().any(|s| s.metric == "pbs_node_running_version_stale"),
            "sans ExtraInfo, pas de comparaison"
        );
    }

    #[test]
    fn un_aveu_dignorance_dapt_ne_devient_pas_un_numero_de_version() {
        let packages: Vec<PackageVersion> = parse(VERSIONS);
        let samples = package_samples(&packages, 1_000);
        let meta = samples
            .iter()
            .find(|s| {
                s.metric == "pbs_node_package_info"
                    && s.labels.get("package").is_some_and(|v| v == "proxmox-backup")
            })
            .unwrap();
        assert_eq!(meta.labels.get("available").map(String::as_str), Some(""));
        assert_eq!(meta.labels.get("running").map(String::as_str), Some("6.8.12-4-pve"));
    }

    #[test]
    fn la_vue_des_paquets_reprend_les_trois_versions() {
        let packages: Vec<PackageVersion> = parse(VERSIONS);
        let views = package_views(&packages);
        let server = views.iter().find(|v| v.package == "proxmox-backup-server").unwrap();
        assert_eq!(server.installed.as_deref(), Some("4.2.6-1"));
        assert_eq!(server.available.as_deref(), Some("4.3.1-1"));
        assert_eq!(server.running.as_deref(), Some("4.2.6"));
        assert!(server.upgradable);
        assert_eq!(server.restart_pending, Some(false));

        // Le méta-paquet `proxmox-backup` ne porte que la version du noyau :
        // ses « unknown » ne doivent pas s'afficher comme des versions.
        let meta = views.iter().find(|v| v.package == "proxmox-backup").unwrap();
        assert_eq!(meta.installed, None);
        assert_eq!(meta.running.as_deref(), Some("6.8.12-4-pve"));
    }

    #[test]
    fn un_certificat_donne_le_temps_qui_lui_reste() {
        let certificates = vec![CertificateInfo {
            filename: Some("proxy.pem".into()),
            issuer: Some("CN=pbs.lan\nO=Proxmox".into()),
            notafter: Some(crate::pbs::model::Num(1_800_000_000.0)),
            ..Default::default()
        }];
        let samples = certificate_samples(&certificates, 1_799_000_000, 1_000);
        assert_eq!(samples.len(), 1);
        assert_eq!(samples[0].metric, "pbs_node_certificate_expires_seconds");
        assert_eq!(samples[0].value, 1_000_000.0);
        assert_eq!(samples[0].labels.get("issuer").map(String::as_str), Some("pbs.lan"));

        // Déjà expiré : la valeur négative est publiée telle quelle.
        let samples = certificate_samples(&certificates, 1_900_000_000, 1_000);
        assert!(samples[0].value < 0.0);

        // Sans date de fin, pas de série plutôt qu'un zéro trompeur.
        assert!(certificate_samples(&[CertificateInfo::default()], 0, 0).is_empty());
    }

    #[test]
    fn une_regle_de_debit_donne_son_plafond_et_son_debit_courant() {
        let rules: Vec<TrafficRule> = parse(TRAFFIC);
        let samples = traffic_samples(&rules, 1_000);
        assert_eq!(
            value(&samples, "pbs_traffic_rate_in_bytes", ("rule", "tc-wan")),
            Some(1_048_576.0)
        );
        assert_eq!(
            value(&samples, "pbs_traffic_limit_in_bytes", ("rule", "tc-wan")),
            Some(100_000_000.0),
            "PBS écrit des préfixes décimaux"
        );
        assert_eq!(
            value(&samples, "pbs_traffic_limit_out_bytes", ("rule", "tc-wan")),
            Some(50_000_000.0)
        );
        assert_eq!(
            samples.iter().find(|s| s.metric == "pbs_traffic_rules_total").map(|s| s.value),
            Some(1.0)
        );

        let views = traffic_views(&rules);
        assert_eq!(views[0].networks, vec!["0.0.0.0/0"]);
        assert_eq!(views[0].limit_out_bytes, Some(50_000_000.0));
    }

    #[test]
    fn aucune_regle_de_debit_donne_un_total_a_zero_et_rien_dautre() {
        let samples = traffic_samples(&[], 1_000);
        assert_eq!(samples.len(), 1);
        assert_eq!(samples[0].value, 0.0);
    }

    #[test]
    fn un_debit_se_lit_dans_les_formes_ecrites_par_pbs() {
        assert_eq!(parse_rate("100 MB"), Some(100_000_000.0));
        assert_eq!(parse_rate("1.5GB"), Some(1_500_000_000.0));
        assert_eq!(parse_rate("500 KB"), Some(500_000.0));
        assert_eq!(parse_rate("2048"), Some(2_048.0));
        assert_eq!(parse_rate("beaucoup"), None);
        assert_eq!(parse_rate(""), None);
    }
}
