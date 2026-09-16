//! Intégration au gestionnaire de services Windows.
//!
//! Windows n'envoie pas de signal : il appelle un gestionnaire d'événements et
//! attend que le service annonce lui-même son changement d'état. Un binaire qui
//! ignore ce dialogue est déclaré en échec au démarrage, puis tué à l'arrêt — d'où
//! ce module, qui traduit l'ordre d'arrêt du système en le même signal interne que
//! `SIGTERM` sur Linux.
//!
//! Ce fichier n'est compilé que sur Windows ; il n'est donc pas couvert par les
//! tests exécutés dans notre chaîne d'intégration, qui est sous Linux.

use std::ffi::OsString;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use windows_service::service::{
    ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus, ServiceType,
};
use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
use windows_service::{define_windows_service, service_dispatcher};

/// Nom sous lequel le service est enregistré. Doit correspondre à celui employé
/// par `install.ps1`, sinon le système refuse de rattacher le processus.
pub const SERVICE_NAME: &str = "DumbMonitAgent";

/// Chemin de configuration transmis du processus principal au fil du service.
///
/// Le gestionnaire de services rappelle le binaire sans nos arguments d'origine :
/// une variable de processus est le moyen le plus simple de ne pas les perdre.
static CONFIG_PATH: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

define_windows_service!(ffi_service_main, service_main);

/// Point d'entrée quand l'agent est lancé avec `--service`.
pub fn start(config_path: PathBuf) -> Result<()> {
    let _ = CONFIG_PATH.set(config_path);
    service_dispatcher::start(SERVICE_NAME, ffi_service_main)
        .context("connecting to the Windows service manager")
}

fn service_main(_arguments: Vec<OsString>) {
    if let Err(error) = run_service() {
        // Rien de mieux à faire ici : la journalisation n'est pas forcément encore
        // en place, et le système ne lit que le code d'état du service.
        eprintln!("the DumbMonit service stopped on an error: {error:#}");
    }
}

fn run_service() -> Result<()> {
    let config_path =
        CONFIG_PATH.get().cloned().unwrap_or_else(crate::config::Config::default_path);
    let config = crate::config::Config::load(&config_path)?;
    crate::init_tracing(config.log_level);
    config.warn_deprecated_env();

    let (trigger, shutdown) = crate::shutdown::channel();

    // Le gestionnaire est appelé depuis un fil du système : il se contente donc de
    // déclencher le signal interne, et rend la main immédiatement comme Windows
    // l'exige — tout traitement long ici ferait passer le service pour bloqué.
    let stop_trigger = trigger.clone();
    let event_handler = move |control| -> ServiceControlHandlerResult {
        match control {
            ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
            ServiceControl::Stop | ServiceControl::Shutdown => {
                stop_trigger.fire();
                ServiceControlHandlerResult::NoError
            }
            _ => ServiceControlHandlerResult::NotImplemented,
        }
    };

    let status_handle = service_control_handler::register(SERVICE_NAME, event_handler)
        .context("registering the service event handler")?;

    status_handle
        .set_service_status(status(
            ServiceState::Running,
            ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN,
        ))
        .context("reporting startup to the service manager")?;

    let outcome = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("starting the async runtime")?
        .block_on(async move {
            crate::shutdown::listen_for_signals(trigger);
            crate::run::Agent::new(config)?.run(shutdown).await
        });

    // L'état « arrêté » est annoncé quoi qu'il arrive : un service qui meurt sans
    // le dire reste affiché « en cours d'arrêt » jusqu'au redémarrage de la machine.
    status_handle
        .set_service_status(status(ServiceState::Stopped, ServiceControlAccept::empty()))
        .context("reporting shutdown to the service manager")?;

    outcome
}

fn status(state: ServiceState, controls: ServiceControlAccept) -> ServiceStatus {
    ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state: state,
        controls_accepted: controls,
        exit_code: ServiceExitCode::Win32(0),
        checkpoint: 0,
        wait_hint: Duration::default(),
        process_id: None,
    }
}
