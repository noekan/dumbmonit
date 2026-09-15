//! Ce que l'agent déclare de sa machine au serveur.

use ezymonit_proto::AgentIdentity;
use sysinfo::System;

use crate::config::Config;

/// Construit l'identité annoncée dans chaque lot.
///
/// Elle est calculée une fois au démarrage : un nom d'hôte qui changerait en cours
/// de route créerait une seconde cible et couperait les graphes en deux, ce qui est
/// exactement l'inverse du service rendu.
pub fn detect(config: &Config) -> AgentIdentity {
    AgentIdentity {
        hostname: config
            .hostname
            .clone()
            .or_else(System::host_name)
            .unwrap_or_else(|| "unknown-host".to_string()),
        os: std::env::consts::OS.to_string(),
        os_version: System::long_os_version().or_else(System::os_version),
        kernel_version: System::kernel_version(),
        arch: Some(std::env::consts::ARCH.to_string()),
        agent_version: env!("CARGO_PKG_VERSION").to_string(),
        machine_id: machine_id(),
        tags: config.tags.clone(),
    }
}

/// Identifiant stable de la machine, quand le système en expose un.
///
/// Sur Linux, `/etc/machine-id` est posé à l'installation et survit aux
/// renommages ; c'est ce qui permet de renommer un serveur sans perdre son
/// historique. Ailleurs, on s'en remet au nom d'hôte, en connaissance de cause.
fn machine_id() -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        for path in ["/etc/machine-id", "/var/lib/dbus/machine-id"] {
            if let Ok(content) = std::fs::read_to_string(path) {
                let id = content.trim();
                if !id.is_empty() {
                    return Some(id.to_string());
                }
            }
        }
        None
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config_with_hostname(hostname: Option<&str>) -> Config {
        Config {
            server_url: "http://serveur:8080".into(),
            token: "ezym_abc".into(),
            interval: std::time::Duration::from_secs(30),
            hostname: hostname.map(str::to_string),
            services: Vec::new(),
            tags: std::collections::BTreeMap::from([("role".to_string(), "nas".to_string())]),
            docker: false,
            docker_socket: std::path::PathBuf::from("/var/run/docker.sock"),
            docker_update_check: false,
            docker_max_containers: 200,
            commands: false,
            probe: crate::collect::ProbeConfig::default(),
            system_health: crate::collect::system_health::SystemHealthConfig::default(),
            plakar: crate::collect::plakar::PlakarConfig::default(),
            max_buffered_samples: 100,
            log_level: tracing::Level::INFO,
        }
    }

    #[test]
    fn the_configured_hostname_wins_over_the_system_one() {
        // Indispensable derrière un NAT ou dans un conteneur, où le nom d'hôte que
        // la machine se donne n'a aucun sens pour l'exploitant.
        let identity = detect(&config_with_hostname(Some("nas-cave")));
        assert_eq!(identity.hostname, "nas-cave");
    }

    #[test]
    fn the_identity_is_never_empty() {
        let identity = detect(&config_with_hostname(None));
        assert!(!identity.hostname.is_empty());
        assert!(!identity.os.is_empty());
        assert_eq!(identity.agent_version, env!("CARGO_PKG_VERSION"));
        assert_eq!(identity.tags.get("role").map(String::as_str), Some("nas"));
    }
}
