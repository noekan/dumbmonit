use std::sync::Arc;

use anyhow::{Context, Result};
use dumbmonit_server::config::Config;
use dumbmonit_server::state::{AppState, Inner};
use dumbmonit_server::{alerting, api, auth, collectors, crypto, db, scheduler, tsdb};
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

fn main() -> Result<()> {
    init_tracing();

    let config = Config::from_env().context("invalid configuration")?;
    info!(version = env!("CARGO_PKG_VERSION"), workers = config.workers, "starting DumbMonit");

    // Le runtime est construit à la main plutôt que par `#[tokio::main]` : c'est
    // le seul moyen de fixer le nombre de threads d'après la configuration.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(config.workers)
        // Le pool bloquant ne sert qu'aux lectures de fichiers (`tokio::fs`) et à
        // quelques résolutions DNS : 512 threads par défaut, seize suffisent.
        .max_blocking_threads(16)
        .thread_name("dumbmonit-worker")
        .enable_all()
        .build()
        .context("building the async runtime")?;
    runtime.block_on(run(config))
}

async fn run(config: Config) -> Result<()> {
    tokio::fs::create_dir_all(&config.data_dir)
        .await
        .with_context(|| format!("creating directory {}", config.data_dir.display()))?;
    ensure_data_dir_writable(&config.data_dir).await?;

    let secret = resolve_secret(&config).await?;
    db::adopt_legacy_database(&config.data_dir).await?;
    let pool = db::open_with(&config.database_path(), config.db_pool_size).await?;
    let cipher = db::init_cipher(&pool, &secret).await?;
    if config.reset_password {
        auth::reset_password(&pool).await?;
        warn!(
            "DUMBMONIT_RESET_PASSWORD is set: all accounts and sessions removed — \
             remove the variable once the first admin has been created again"
        );
    }
    info!(database = %config.database_path().display(), "database ready");

    let victoria_url = config.effective_victoria_url();
    let victoria = tsdb::Victoria::new(&victoria_url)?;
    let embedded_vm = if config.vm_embedded() {
        // Sans URL externe, l'image se suffit : VictoriaMetrics est lancé ici même
        // et le démarrage attend qu'il réponde — une erreur à ce stade (binaire
        // absent, port pris) doit arrêter le serveur, pas le laisser tourner à vide.
        let vm = tsdb::EmbeddedVm::start(
            tsdb::EmbeddedConfig {
                binary: config.vm_binary.clone(),
                data_path: config.vm_data_path(),
                listen: config.vm_listen.clone(),
                retention: config.vm_retention.clone(),
                memory: config.vm_memory.clone(),
            },
            &victoria,
        )
        .await
        .context("starting the embedded VictoriaMetrics")?;
        Some(vm)
    } else {
        match victoria.health().await {
            Ok(()) => info!(url = %victoria_url, "VictoriaMetrics reachable"),
            // On ne bloque pas le démarrage : l'ordre de lancement des conteneurs
            // n'est pas garanti, et le tampon d'écriture retentera de lui-même.
            Err(error) => {
                warn!(url = %victoria_url, %error, "VictoriaMetrics unreachable at startup")
            }
        }
        None
    };

    let sink = tsdb::spawn_writer_with(
        victoria.clone(),
        config.write_flush_interval,
        config.write_flush_size,
    );

    let mut registry = collectors::Registry::new();

    // Équipements interrogés à distance.
    registry.register(Arc::new(
        collectors::SnmpCollector::new().with_request_timeout(config.probe_timeout),
    ));
    registry.register(Arc::new(collectors::ProxmoxCollector::new()));
    registry.register(Arc::new(
        collectors::PbsCollector::new()
            .with_observer(collectors::pbs_history::sqlite_observer(pool.clone())),
    ));
    registry.register(Arc::new(
        collectors::PdmCollector::new()
            .with_observer(collectors::pdm_history::sqlite_observer(pool.clone())),
    ));
    registry.register(Arc::new(
        collectors::PmgCollector::new()
            .with_observer(collectors::pmg_history::sqlite_observer(pool.clone())),
    ));
    registry.register(Arc::new(
        collectors::SynologyCollector::new()
            .with_abb_history(collectors::synology_history::sqlite_history(pool.clone())),
    ));
    registry.register(Arc::new(
        collectors::OpnsenseCollector::new()
            .with_observer(collectors::opnsense_history::sqlite_observer(pool.clone())),
    ));
    registry.register(Arc::new(
        collectors::TruenasCollector::new()
            .with_observer(collectors::truenas_history::sqlite_observer(pool.clone())),
    ));
    // Matériel serveur, lu sur le contrôleur de gestion (BMC) en Redfish.
    registry.register(Arc::new(collectors::RedfishCollector::new()));

    // Machines équipées de l'agent : les mesures arrivent en push, ce collecteur ne
    // fait que constater leur fraîcheur.
    registry.register(Arc::new(collectors::AgentCollector::new(pool.clone())));

    // Sondes de disponibilité, à la manière d'Uptime Kuma.
    registry.register(Arc::new(collectors::HttpCollector::new()));
    registry.register(Arc::new(collectors::TcpCollector::new()));
    registry.register(Arc::new(collectors::DnsCollector::new()));
    registry.register(Arc::new(collectors::PingCollector::new()));
    registry.register(Arc::new(collectors::TlsCollector::new()));

    // Sondes applicatives : elles mènent le début d'une vraie session plutôt que
    // de constater l'ouverture d'un port.
    registry.register(Arc::new(collectors::SmtpCollector::new()));
    registry.register(Arc::new(collectors::PostgresCollector::new()));
    registry.register(Arc::new(collectors::MysqlCollector::new()));
    registry.register(Arc::new(collectors::MqttCollector::new()));
    registry.register(Arc::new(collectors::WebsocketCollector::new()));

    // Moniteurs en poussée (heartbeat) : le travail surveillé appelle une URL, ce
    // collecteur ne fait que constater qu'il l'a fait à temps.
    registry.register(Arc::new(collectors::PushCollector::new(pool.clone())));

    // Collecteur de démonstration : il permet d'obtenir des graphes sans matériel,
    // le temps de configurer un premier équipement réel.
    registry.register(Arc::new(collectors::DummyCollector));
    info!(collectors = ?registry.kinds(), "collectors registered");

    let bind = config.bind;
    let state = AppState::new(Inner { config, pool, cipher, victoria, sink, collectors: registry });

    scheduler::spawn(state.clone());
    alerting::spawn(state.clone());
    collectors::agent::spawn_policy_scheduler(state.clone());
    // Sauvegardes locales planifiées de la base, et la mesure qui dit qu'elles
    // ont bien lieu (`backup::local`).
    dumbmonit_server::backup::local::spawn(state.clone());

    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .with_context(|| format!("cannot listen on {bind}"))?;
    info!(%bind, "interface available");

    // `ConnectInfo` : l'adresse du client, pour les compteurs de tentatives et
    // le journal d'audit (voir `auth::client_ip`).
    axum::serve(
        listener,
        api::router(state).into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await
    .context("HTTP server error")?;

    // VictoriaMetrics s'arrête après nous : les derniers lots du tampon d'écriture
    // ont ainsi une chance d'être acceptés.
    if let Some(vm) = embedded_vm {
        vm.stop().await;
    }

    info!("clean shutdown");
    Ok(())
}

fn init_tracing() {
    // Lecture brute des deux noms : `env_var` avertirait avant que l'abonné
    // n'existe, et l'avertissement serait perdu. Il est rejoué juste après.
    let raw = std::env::var("DUMBMONIT_LOG").or_else(|_| std::env::var("EZYMONIT_LOG")).ok();
    let filter = raw
        .as_deref()
        .and_then(|directives| EnvFilter::try_new(directives).ok())
        .unwrap_or_else(|| EnvFilter::new("info,sqlx=warn,hyper=warn"));
    tracing_subscriber::fmt().with_env_filter(filter).with_target(false).init();
    let _ = dumbmonit_server::config::env_var("DUMBMONIT_LOG");
}

/// Détermine le secret d'instance : variable d'environnement si fournie, sinon
/// fichier persistant, sinon génération au premier démarrage.
///
/// Le fichier permet à `docker compose up` de fonctionner sans configuration, tout
/// en gardant les identifiants déchiffrables après un redémarrage.
/// Vérifie tout de suite que le répertoire de données est accessible, avec un
/// message qui dit quoi faire : depuis 0.1.0-alpha.2 le conteneur ne tourne plus
/// en root, et un volume créé par une version antérieure (ou un bind mount)
/// appartient encore à root. Sans ce contrôle, l'erreur arrive plus loin, sur
/// `secret.key` ou sur la base, sous une forme qui n'explique rien.
async fn ensure_data_dir_writable(data_dir: &std::path::Path) -> Result<()> {
    let probe = data_dir.join(".write-test");
    let outcome = match tokio::fs::write(&probe, b"").await {
        Ok(()) => tokio::fs::remove_file(&probe).await,
        Err(error) => Err(error),
    };
    match outcome {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
            #[cfg(unix)]
            let uid = unsafe { libc::geteuid() };
            #[cfg(not(unix))]
            let uid = 0;
            anyhow::bail!(
                "{} is not writable by the server (running as uid {uid}). The container \
                 runs as user 65532 since 0.1.0-alpha.2: a volume created by an older \
                 version or a bind mount must be handed over once with\n  docker run --rm \
                 -v dumbmonit-data:/data alpine chown -R 65532:65532 /data\nor start the \
                 container as the owner of the files (`user: \"1000:1000\"` in \
                 docker-compose.yml).",
                data_dir.display()
            );
        }
        Err(error) => {
            Err(anyhow::Error::from(error).context(format!("writing to {}", data_dir.display())))
        }
    }
}

async fn resolve_secret(config: &Config) -> Result<String> {
    if let Some(secret) = &config.secret {
        info!("instance secret provided by the environment");
        return Ok(secret.clone());
    }

    let path = config.secret_path();
    match tokio::fs::read_to_string(&path).await {
        Ok(secret) => {
            let secret = secret.trim().to_string();
            if secret.is_empty() {
                anyhow::bail!(
                    "the file {} is empty: restore it, or delete it to generate a new one",
                    path.display()
                );
            }
            Ok(secret)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let secret = crypto::generate_secret();
            write_secret_file(&path, &secret).await?;
            warn!(
                path = %path.display(),
                "instance secret generated — back up this file with the database, \
                 without it the device credentials cannot be recovered"
            );
            Ok(secret)
        }
        Err(error) => {
            Err(anyhow::Error::from(error).context(format!("reading {}", path.display())))
        }
    }
}

async fn write_secret_file(path: &std::path::Path, secret: &str) -> Result<()> {
    tokio::fs::write(path, secret).await.with_context(|| format!("writing {}", path.display()))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .await
            .with_context(|| format!("restricting permissions on {}", path.display()))?;
    }
    Ok(())
}

/// Attend `SIGTERM` (arrêt de conteneur) ou `Ctrl+C`.
async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(error) => warn!(%error, "cannot listen for SIGTERM"),
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => info!("interrupt received"),
        _ = terminate => info!("SIGTERM received"),
    }
}
