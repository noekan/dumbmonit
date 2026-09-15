//! Agent système EzyMonit.
//!
//! Un petit binaire installé sur la machine à surveiller, qui mesure et pousse ses
//! relevés vers le serveur. C'est l'agent qui se connecte, jamais le serveur :
//! rien à ouvrir sur la machine surveillée, et une machine derrière un NAT
//! domestique se surveille aussi bien qu'une autre.

mod backoff;
mod buffer;
mod client;
mod collect;
mod commands;
mod config;
mod identity;
mod run;
mod shutdown;
#[cfg(windows)]
mod winsvc;

use std::path::PathBuf;

use anyhow::{Context, Result, bail};

use crate::config::Config;

fn main() -> Result<()> {
    let args = Args::parse(std::env::args().skip(1))?;

    if args.help {
        print_help();
        return Ok(());
    }
    if args.version {
        println!("ezymonit-agent {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }

    // Lancé par le gestionnaire de services Windows : il faut lui répondre avant
    // toute autre chose, sinon il considère le démarrage comme échoué.
    #[cfg(windows)]
    if args.service {
        return winsvc::start(args.config_path());
    }

    let config = Config::load(&args.config_path())?;
    init_tracing(config.log_level);

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("starting the async runtime")?;

    runtime.block_on(async move {
        if args.dry_run {
            return print_samples(config).await;
        }
        let mut agent = run::Agent::new(config)?;
        if args.once {
            return agent.run_once().await;
        }
        let (trigger, shutdown) = shutdown::channel();
        shutdown::listen_for_signals(trigger);
        agent.run(shutdown).await
    })
}

/// Collecte un cycle et l'affiche, sans rien envoyer.
///
/// C'est l'outil de diagnostic : il répond à « qu'est-ce que cet agent remonte
/// exactement ? » sans dépendre du serveur ni polluer les séries.
async fn print_samples(config: Config) -> Result<()> {
    let mut agent = run::Agent::new(config)?;
    let samples = agent.collect().await;
    println!("{}", serde_json::to_string_pretty(&samples)?);
    eprintln!("{} samples, nothing sent (--dry-run)", samples.len());
    Ok(())
}

fn init_tracing(level: tracing::Level) {
    // Sans couleurs : la sortie part dans le journal du système, où les codes
    // d'échappement ne font que gêner la lecture.
    tracing_subscriber::fmt().with_max_level(level).with_target(false).with_ansi(false).init();
}

#[derive(Debug, Default, PartialEq, Eq)]
struct Args {
    config: Option<PathBuf>,
    once: bool,
    dry_run: bool,
    /// N'a de sens que sur Windows, mais l'option reste analysée partout pour que
    /// le même fichier de configuration de service puisse être copié tel quel.
    #[cfg_attr(not(windows), allow(dead_code))]
    service: bool,
    help: bool,
    version: bool,
}

impl Args {
    fn parse(mut args: impl Iterator<Item = String>) -> Result<Self> {
        let mut parsed = Self::default();

        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--once" => parsed.once = true,
                "--dry-run" => parsed.dry_run = true,
                "--service" => parsed.service = true,
                "-h" | "--help" => parsed.help = true,
                "-V" | "--version" => parsed.version = true,
                "-c" | "--config" => {
                    let path = args.next().context("--config expects a file path")?;
                    parsed.config = Some(PathBuf::from(path));
                }
                other if other.starts_with("--config=") => {
                    parsed.config = Some(PathBuf::from(&other["--config=".len()..]));
                }
                other => bail!("unknown option '{other}' (see --help)"),
            }
        }
        Ok(parsed)
    }

    fn config_path(&self) -> PathBuf {
        self.config
            .clone()
            .or_else(|| std::env::var_os("EZYMONIT_AGENT_CONFIG").map(PathBuf::from))
            .unwrap_or_else(Config::default_path)
    }
}

fn print_help() {
    println!(
        "ezymonit-agent {version}

Collects this machine's metrics and pushes them to a DumbMonit server.

USAGE:
    ezymonit-agent [OPTIONS]

OPTIONS:
    -c, --config <FILE>     Configuration file
                            (default: {default})
        --once              Send a single batch, then exit
        --dry-run           Print the measurements without sending anything
        --service           Run as a Windows service
    -h, --help              Show this help
    -V, --version           Show the version

ENVIRONMENT VARIABLES (override the file):
    EZYMONIT_AGENT_CONFIG           Configuration file path
    EZYMONIT_AGENT_URL              Server URL, for example http://server:8080
    EZYMONIT_AGENT_TOKEN            Enrollment token
    EZYMONIT_AGENT_INTERVAL_SECS    Sampling period
    EZYMONIT_AGENT_HOSTNAME         Hostname announced to the server
    EZYMONIT_AGENT_SERVICES         Services to watch, comma-separated
    EZYMONIT_AGENT_TAGS             Tags as key=value, comma-separated
    EZYMONIT_AGENT_DOCKER           Container inventory (true/false)
    EZYMONIT_AGENT_DOCKER_UPDATE_CHECK  Compare images with their registry (true/false)
    EZYMONIT_AGENT_COMMANDS         Accept container actions from the server (true/false)
    EZYMONIT_AGENT_PLAKAR_KLOSETS   Plakar klosets to watch, comma-separated
    EZYMONIT_AGENT_PLAKAR_BIN       Path to the plakar binary (default: plakar on PATH)
    EZYMONIT_AGENT_PLAKAR_HOME      HOME used when running plakar
    EZYMONIT_AGENT_PLAKAR_INTERVAL_SECS  Seconds between two kloset readings
    EZYMONIT_AGENT_LOG              Log level (info, debug, ...)",
        version = env!("CARGO_PKG_VERSION"),
        default = Config::default_path().display(),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Args> {
        Args::parse(args.iter().map(|a| a.to_string()))
    }

    #[test]
    fn no_argument_means_the_default_configuration() {
        assert_eq!(parse(&[]).unwrap(), Args::default());
    }

    #[test]
    fn the_configuration_path_is_accepted_in_both_forms() {
        // La forme accolée est celle qu'écrivent les fichiers d'unité systemd, la
        // forme séparée celle qu'on tape à la main.
        assert_eq!(
            parse(&["--config=/etc/ezymonit/agent.yaml"]).unwrap().config,
            Some(PathBuf::from("/etc/ezymonit/agent.yaml"))
        );
        assert_eq!(
            parse(&["-c", "/autre/agent.yaml"]).unwrap().config,
            Some(PathBuf::from("/autre/agent.yaml"))
        );
    }

    #[test]
    fn a_config_option_without_a_path_is_refused() {
        assert!(parse(&["--config"]).is_err());
    }

    #[test]
    fn an_unknown_option_is_refused_rather_than_ignored() {
        // Ignorer silencieusement une faute de frappe dans un fichier d'unité
        // ferait tourner l'agent avec une configuration qui n'est pas la bonne.
        let error = parse(&["--interval=5"]).unwrap_err().to_string();
        assert!(error.contains("--interval=5"), "unexpected message: {error}");
    }

    #[test]
    fn flags_can_be_combined() {
        let args = parse(&["--once", "--dry-run"]).unwrap();
        assert!(args.once && args.dry_run);
    }
}
