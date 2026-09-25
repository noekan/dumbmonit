//! État des services nommés dans la configuration.
//!
//! Quatre mondes, une seule métrique : `systemctl` sur Linux, `launchctl` sur
//! macOS, `service` sur FreeBSD, le gestionnaire de services sur Windows. Le
//! nom écrit dans la configuration est celui du système hôte — une unité
//! (`sshd.service`), une étiquette launchd (`com.apple.sshd`), un service rc.d
//! (`sshd`) — et l'agent ne le réécrit jamais.
//!
//! L'agent ne remonte jamais d'erreur ici — un service introuvable est un
//! service à l'arrêt du point de vue de celui qui surveille, et faire échouer
//! tout un cycle de collecte pour une unité mal orthographiée serait
//! disproportionné.

/// État d'un service, réduit à ce qui se pilote depuis une alerte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceState {
    Running,
    Stopped,
    /// Le service a échoué : distinct de « arrêté », qui peut être volontaire.
    Failed,
    /// Service introuvable, ou état indéterminable.
    Unknown,
}

impl ServiceState {
    /// Valeur remontée dans `service_up`.
    ///
    /// Tout ce qui n'est pas « en marche » vaut zéro : c'est ce qui permet
    /// d'écrire une règle d'alerte unique, sans énumérer les états d'échec.
    pub fn as_value(self) -> f64 {
        match self {
            Self::Running => 1.0,
            _ => 0.0,
        }
    }
}

/// Interroge l'état des services demandés, dans l'ordre de la configuration.
pub async fn probe(names: &[String]) -> Vec<(String, ServiceState)> {
    if names.is_empty() {
        return Vec::new();
    }
    platform::probe(names).await
}

#[cfg(target_os = "linux")]
mod platform {
    use super::{ServiceState, parse_is_active};

    pub async fn probe(names: &[String]) -> Vec<(String, ServiceState)> {
        // Une seule invocation pour toutes les unités : lancer un processus par
        // service coûterait plus cher que tout le reste de la collecte réunie.
        let output = tokio::process::Command::new("systemctl")
            .arg("is-active")
            .args(names)
            .kill_on_drop(true)
            .output()
            .await;

        match output {
            // `systemctl` renvoie un code non nul dès qu'une unité n'est pas active :
            // c'est une réponse, pas une panne, seule la sortie standard compte.
            Ok(output) => parse_is_active(&String::from_utf8_lossy(&output.stdout), names),
            Err(error) => {
                tracing::debug!(%error, "systemctl unavailable, service states unknown");
                names.iter().map(|name| (name.clone(), ServiceState::Unknown)).collect()
            }
        }
    }
}

/// macOS : `launchctl list` sans argument imprime tout le domaine en trois
/// colonnes — une seule invocation, comme sous Linux.
#[cfg(target_os = "macos")]
mod platform {
    use super::{ServiceState, parse_launchctl_list};

    pub async fn probe(names: &[String]) -> Vec<(String, ServiceState)> {
        let output =
            tokio::process::Command::new("launchctl").arg("list").kill_on_drop(true).output().await;

        match output {
            Ok(output) => parse_launchctl_list(&String::from_utf8_lossy(&output.stdout), names),
            Err(error) => {
                tracing::debug!(%error, "launchctl unavailable, service states unknown");
                names.iter().map(|name| (name.clone(), ServiceState::Unknown)).collect()
            }
        }
    }
}

/// FreeBSD : `service` n'a pas d'équivalent de `systemctl is-active a b c`, il
/// faut donc une invocation par service. C'est un `sh` par service et par
/// cycle ; la liste surveillée en compte une poignée, et ne rien remonter du
/// tout serait pire.
#[cfg(target_os = "freebsd")]
mod platform {
    use super::{ServiceState, parse_service_onestatus};

    pub async fn probe(names: &[String]) -> Vec<(String, ServiceState)> {
        let mut states = Vec::with_capacity(names.len());
        for name in names {
            // `onestatus` plutôt que `status` : il répond même pour un service
            // que `rc.conf` n'a pas activé, ce qui est justement le cas qu'on
            // veut voir — un service éteint par mégarde.
            let output = tokio::process::Command::new("service")
                .args([name.as_str(), "onestatus"])
                .kill_on_drop(true)
                .output()
                .await;
            let state = match output {
                Ok(output) => parse_service_onestatus(&String::from_utf8_lossy(&output.stdout)),
                Err(error) => {
                    tracing::debug!(%error, "service(8) unavailable, service states unknown");
                    ServiceState::Unknown
                }
            };
            states.push((name.clone(), state));
        }
        states
    }
}

/// Tout autre système d'exploitation de type Unix : rien de fiable à
/// interroger, et l'on préfère le dire.
#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos", target_os = "freebsd"))))]
mod platform {
    use super::ServiceState;

    pub async fn probe(names: &[String]) -> Vec<(String, ServiceState)> {
        names.iter().map(|name| (name.clone(), ServiceState::Unknown)).collect()
    }
}

#[cfg(windows)]
mod platform {
    use super::ServiceState;

    pub async fn probe(names: &[String]) -> Vec<(String, ServiceState)> {
        use windows_service::service::ServiceAccess;
        use windows_service::service::ServiceState as WinState;
        use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};

        let manager =
            match ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT) {
                Ok(manager) => manager,
                Err(error) => {
                    tracing::debug!(%error, "service manager unreachable");
                    return names.iter().map(|n| (n.clone(), ServiceState::Unknown)).collect();
                }
            };

        names
            .iter()
            .map(|name| {
                let state = manager
                    .open_service(name, ServiceAccess::QUERY_STATUS)
                    .and_then(|service| service.query_status())
                    .map(|status| match status.current_state {
                        WinState::Running => ServiceState::Running,
                        WinState::Stopped => ServiceState::Stopped,
                        // Démarrage, arrêt, pause : transitoires, et donc
                        // volontairement classés « ni en marche, ni à l'arrêt ».
                        _ => ServiceState::Unknown,
                    })
                    .unwrap_or(ServiceState::Unknown);
                (name.clone(), state)
            })
            .collect()
    }
}

/// Traduit la sortie de `systemctl is-active`, une ligne par unité interrogée.
///
/// Isolée du lancement de processus pour être vérifiable : c'est le seul endroit
/// où une évolution de `systemctl` peut nous surprendre.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn parse_is_active(stdout: &str, names: &[String]) -> Vec<(String, ServiceState)> {
    let mut lines = stdout.lines().map(str::trim);
    names
        .iter()
        .map(|name| {
            let state = match lines.next() {
                Some("active") => ServiceState::Running,
                Some("failed") => ServiceState::Failed,
                Some("inactive" | "deactivating") => ServiceState::Stopped,
                // « activating », « unknown », ou une ligne manquante parce que
                // `systemctl` a écrit moins de lignes que d'unités demandées.
                _ => ServiceState::Unknown,
            };
            (name.clone(), state)
        })
        .collect()
}

/// Traduit `launchctl list` : trois colonnes séparées par des tabulations,
/// `PID  Status  Label`, une ligne d'en-tête en tête.
///
/// Un PID numérique signifie que le service tourne. À l'arrêt, `launchctl`
/// écrit `-` et garde le code de sortie du dernier lancement : non nul, le
/// service a échoué — c'est ce qui distingue un démon planté d'un démon
/// volontairement arrêté.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn parse_launchctl_list(stdout: &str, names: &[String]) -> Vec<(String, ServiceState)> {
    let mut seen = std::collections::BTreeMap::new();
    for line in stdout.lines().skip(1) {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 3 {
            continue;
        }
        let label = fields[2];
        let state = if fields[0].parse::<u32>().is_ok() {
            ServiceState::Running
        } else if fields[1].parse::<i64>().is_ok_and(|status| status != 0) {
            ServiceState::Failed
        } else {
            ServiceState::Stopped
        };
        seen.insert(label.to_string(), state);
    }
    names
        .iter()
        .map(|name| {
            let state = seen.get(name).copied().unwrap_or(ServiceState::Unknown);
            (name.clone(), state)
        })
        .collect()
}

/// Traduit `service <nom> onestatus` : une phrase, et rien d'autre.
///
/// « <nom> is running as pid 1234. » ou « <nom> is not running. ». Tout le
/// reste est un inconnu — notamment le service dont le script rc.d n'existe
/// pas, qui fait écrire `service` sur la sortie d'erreur et ne dit rien ici.
/// C'est un nom mal orthographié, pas un service arrêté, et un faux zéro
/// vaudrait une fausse alerte toutes les trente secondes.
#[cfg_attr(not(target_os = "freebsd"), allow(dead_code))]
fn parse_service_onestatus(stdout: &str) -> ServiceState {
    let text = stdout.trim();
    if text.contains("is running") {
        ServiceState::Running
    } else if text.contains("is not running") {
        ServiceState::Stopped
    } else {
        ServiceState::Unknown
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|n| n.to_string()).collect()
    }

    #[test]
    fn each_line_maps_to_the_unit_in_the_same_position() {
        let states =
            parse_is_active("active\ninactive\nfailed\n", &names(&["sshd", "nginx", "postgresql"]));
        assert_eq!(
            states,
            vec![
                ("sshd".to_string(), ServiceState::Running),
                ("nginx".to_string(), ServiceState::Stopped),
                ("postgresql".to_string(), ServiceState::Failed),
            ]
        );
    }

    #[test]
    fn a_missing_line_becomes_an_unknown_state() {
        // Une réponse tronquée ne doit surtout pas décaler les unités suivantes.
        let states = parse_is_active("active\n", &names(&["sshd", "nginx"]));
        assert_eq!(states[0].1, ServiceState::Running);
        assert_eq!(states[1].1, ServiceState::Unknown);
    }

    #[test]
    fn an_empty_output_leaves_every_unit_unknown() {
        let states = parse_is_active("", &names(&["sshd"]));
        assert_eq!(states, vec![("sshd".to_string(), ServiceState::Unknown)]);
    }

    #[test]
    fn only_a_running_service_counts_as_up() {
        assert_eq!(ServiceState::Running.as_value(), 1.0);
        assert_eq!(ServiceState::Stopped.as_value(), 0.0);
        assert_eq!(ServiceState::Failed.as_value(), 0.0);
        assert_eq!(ServiceState::Unknown.as_value(), 0.0);
    }

    #[test]
    fn launchctl_tells_a_running_daemon_from_a_crashed_one() {
        let stdout = "PID\tStatus\tLabel\n\
                      421\t0\tcom.apple.sshd\n\
                      -\t78\tio.tailscale.ipn.macsys\n\
                      -\t0\tcom.example.backup\n";
        let states = parse_launchctl_list(
            stdout,
            &names(&["com.apple.sshd", "io.tailscale.ipn.macsys", "com.example.backup", "absent"]),
        );
        assert_eq!(states[0].1, ServiceState::Running);
        assert_eq!(states[1].1, ServiceState::Failed, "code de sortie non nul");
        assert_eq!(states[2].1, ServiceState::Stopped);
        assert_eq!(states[3].1, ServiceState::Unknown, "étiquette inconnue de launchd");
    }

    #[test]
    fn launchctl_answers_in_the_order_the_configuration_asked() {
        let stdout = "PID\tStatus\tLabel\n-\t0\tb\n12\t0\ta\n";
        let states = parse_launchctl_list(stdout, &names(&["a", "b"]));
        assert_eq!(states[0].0, "a");
        assert_eq!(states[0].1, ServiceState::Running);
        assert_eq!(states[1].1, ServiceState::Stopped);
    }

    #[test]
    fn freebsd_reads_the_one_sentence_service_prints() {
        assert_eq!(
            parse_service_onestatus("sshd is running as pid 1234.\n"),
            ServiceState::Running
        );
        assert_eq!(parse_service_onestatus("nginx is not running.\n"), ServiceState::Stopped);
        // Nom mal orthographié : `service` n'écrit rien ici. Inconnu, surtout
        // pas « à l'arrêt ».
        assert_eq!(parse_service_onestatus(""), ServiceState::Unknown);
    }

    #[tokio::test]
    async fn an_empty_configuration_launches_no_process() {
        assert!(probe(&[]).await.is_empty());
    }
}
