//! Ce que l'agent déclare de sa machine au serveur.

use dumbmonit_proto::AgentIdentity;
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
        commands_enabled: Some(config.commands),
        relay: config.relay,
        site: config.site.clone(),
        machine_id: machine_id(),
        tags: config.tags.clone(),
        // Ce binaire sait recevoir et représenter un secret de liaison. C'est ce
        // drapeau qui autorise le serveur à lui en attribuer un : un agent plus
        // ancien le jetterait, et se verrait refuser son lot suivant.
        binding_supported: true,
    }
}

/// Identifiant stable de la machine, quand le système en expose un.
///
/// C'est lui qui permet de renommer un serveur sans couper son historique en
/// deux : le serveur reconnaît la machine à cet identifiant, pas à son nom.
/// Chaque système range le sien ailleurs :
///
/// - Linux : `/etc/machine-id`, posé à l'installation.
/// - macOS : l'`IOPlatformUUID` du registre IOKit, gravé dans la machine.
/// - FreeBSD : `kern.hostuuid`, ou `/etc/hostid` s'il est renseigné.
///
/// Ailleurs — Windows, et tout système qui n'expose rien de tel — on s'en remet
/// au nom d'hôte, en connaissance de cause.
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
    #[cfg(target_os = "macos")]
    {
        // `ioreg` est livré avec le système et n'exige aucun privilège. Le
        // passer par une commande plutôt que par une liaison à IOKit évite une
        // dépendance native pour une seule lecture, faite une fois au démarrage.
        let output = std::process::Command::new("/usr/sbin/ioreg")
            .args(["-rd1", "-c", "IOPlatformExpertDevice"])
            .output()
            .ok()?;
        parse_ioplatform_uuid(&String::from_utf8_lossy(&output.stdout))
    }
    #[cfg(target_os = "freebsd")]
    {
        // `/etc/hostid` est écrit au premier démarrage à partir du SMBIOS ;
        // `kern.hostuuid` dit la même chose et existe même sans ce fichier.
        if let Ok(content) = std::fs::read_to_string("/etc/hostid") {
            if let Some(id) = clean_uuid(&content) {
                return Some(id);
            }
        }
        let output = std::process::Command::new("/sbin/sysctl")
            .args(["-n", "kern.hostuuid"])
            .output()
            .ok()?;
        clean_uuid(&String::from_utf8_lossy(&output.stdout))
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "freebsd")))]
    {
        None
    }
}

/// Extrait l'`IOPlatformUUID` d'une sortie de `ioreg`, qui est un arbre de
/// propriétés : `"IOPlatformUUID" = "5A3B…"`.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn parse_ioplatform_uuid(text: &str) -> Option<String> {
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else { continue };
        if !key.contains("IOPlatformUUID") {
            continue;
        }
        if let Some(id) = clean_uuid(value.trim().trim_matches('"')) {
            return Some(id);
        }
    }
    None
}

/// Un identifiant n'est retenu que s'il en est un : `sysctl` rend une ligne
/// vide quand la variable n'existe pas, et FreeBSD écrit un `/etc/hostid` tout à
/// zéro quand le SMBIOS n'a rien donné — deux machines l'auraient alors en
/// commun, ce qui est pire que pas d'identifiant du tout.
#[cfg_attr(not(any(target_os = "macos", target_os = "freebsd")), allow(dead_code))]
fn clean_uuid(raw: &str) -> Option<String> {
    let id = raw.trim().trim_matches('"').trim();
    if id.is_empty() || id.chars().all(|c| c == '0' || c == '-') {
        return None;
    }
    Some(id.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config_with_hostname(hostname: Option<&str>) -> Config {
        Config {
            server_url: "http://serveur:8080".into(),
            token: "dmon_abc".into(),
            interval: std::time::Duration::from_secs(30),
            hostname: hostname.map(str::to_string),
            services: Vec::new(),
            tags: std::collections::BTreeMap::from([("role".to_string(), "nas".to_string())]),
            docker: false,
            docker_socket: std::path::PathBuf::from("/var/run/docker.sock"),
            docker_update_check: false,
            docker_max_containers: 200,
            commands: false,
            relay: false,
            site: None,
            probe: crate::collect::ProbeConfig::default(),
            system_health: crate::collect::system_health::SystemHealthConfig::default(),
            plakar: crate::collect::plakar::PlakarConfig::default(),
            sensors: false,
            smart: crate::collect::smart::SmartConfig::default(),
            zfs: crate::collect::zfs::ZfsConfig::default(),
            max_buffered_samples: 100,
            secret_path: std::path::PathBuf::from("/inexistant/agent-secret"),
            log_level: tracing::Level::INFO,
            deprecated_env: Vec::new(),
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

    #[test]
    fn the_identity_says_this_binary_can_be_bound_to_its_machine() {
        // Le serveur s'en sert pour distinguer « va se lier au prochain lot »
        // d'un agent trop ancien, qu'il faut aller mettre à jour.
        assert!(detect(&config_with_hostname(None)).binding_supported);
    }

    #[test]
    fn the_macos_platform_uuid_is_read_out_of_the_ioreg_tree() {
        let ioreg = r#"+-o J316sAP  <class IOPlatformExpertDevice, id 0x100000267>
    {
      "IOPlatformSerialNumber" = "C02XXXXXXXXX"
      "IOPlatformUUID" = "5A3B7C1E-9D42-4F08-B6A1-7C0E2D4F9A11"
      "model" = <"Mac14,12">
    }
"#;
        assert_eq!(
            parse_ioplatform_uuid(ioreg).as_deref(),
            Some("5A3B7C1E-9D42-4F08-B6A1-7C0E2D4F9A11")
        );
        assert_eq!(parse_ioplatform_uuid("rien de tel ici"), None);
    }

    #[test]
    fn an_identifier_made_only_of_zeroes_identifies_nothing() {
        // FreeBSD écrit ce `hostid` quand le SMBIOS ne lui a rien donné : le
        // retenir ferait passer tout un parc pour une seule machine.
        assert_eq!(clean_uuid("00000000-0000-0000-0000-000000000000"), None);
        assert_eq!(clean_uuid("   \n"), None);
        assert_eq!(
            clean_uuid(" 8f2c1b9e-0d55-11ef-9a3c-0800271b2e44 \n").as_deref(),
            Some("8f2c1b9e-0d55-11ef-9a3c-0800271b2e44")
        );
    }

    #[test]
    fn the_identity_declares_whether_commands_are_accepted() {
        // C'est ce qui permet au serveur de ne pas proposer « Redémarrer » pour
        // une machine dont l'agent ne viendra jamais chercher la commande.
        let mut config = config_with_hostname(None);
        assert_eq!(detect(&config).commands_enabled, Some(false));
        config.commands = true;
        assert_eq!(detect(&config).commands_enabled, Some(true));
    }
}
