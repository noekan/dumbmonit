//! État des services nommés dans la configuration.
//!
//! Deux mondes, une seule métrique : `systemctl` sur Linux, le gestionnaire de
//! services sur Windows. L'agent ne remonte jamais d'erreur ici — un service
//! introuvable est un service à l'arrêt du point de vue de celui qui surveille, et
//! faire échouer tout un cycle de collecte pour une unité mal orthographiée serait
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

#[cfg(unix)]
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
#[cfg_attr(not(unix), allow(dead_code))]
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

    #[tokio::test]
    async fn an_empty_configuration_launches_no_process() {
        assert!(probe(&[]).await.is_empty());
    }
}
