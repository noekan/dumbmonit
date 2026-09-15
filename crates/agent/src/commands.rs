//! Actions demandées par le serveur : redémarrer ou mettre à jour un conteneur.
//!
//! Le canal reste tiré par l'agent : après chaque lot accepté, il demande au
//! serveur s'il a une commande pour lui, l'exécute dans une tâche de fond et rend
//! compte. Une seule commande à la fois — deux mises à jour concurrentes sur la
//! même machine n'auraient aucun sens — et jamais dans la boucle de collecte,
//! qui continue de mesurer pendant qu'une image se télécharge.
//!
//! La mise à jour suit le schéma de Watchtower : tirer l'image, recréer le
//! conteneur avec la même configuration, vérifier qu'il tient debout, et sinon
//! remettre l'ancien en service. L'ancien conteneur n'est supprimé qu'une fois le
//! nouveau jugé sain ; entre les deux, il attend renommé.

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use ezymonit_proto::{
    AgentCommand, CMD_CONTAINER_RESTART, CMD_CONTAINER_UPDATE, CommandReport, CommandStatus,
};
use serde_json::{Map, Value, json};
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

use crate::client::PushClient;
use crate::collect::docker::{DockerClient, DockerResponse};

/// Taille maximale du compte rendu envoyé au serveur. Le journal complet reste
/// dans les traces de l'agent ; l'interface n'a besoin que de la fin.
pub const RESULT_MAX_BYTES: usize = 4096;

/// Conteneurs que le serveur ne peut pas toucher : ceux de la supervision
/// elle-même. Un « redémarre-toi » venu de l'interface finirait en boucle.
const RESERVED_PREFIXES: [&str; 2] = ["ezymonit", "dumbmonit"];

/// Suffixe du nom donné à l'ancien conteneur pendant la mise à jour.
const PREVIOUS_SUFFIX: &str = ".previous";

/// Délai accordé au conteneur pour s'arrêter proprement avant d'être tué.
const STOP_GRACE_SECS: u64 = 30;

/// Idem pour un redémarrage, plus court : c'est l'usage de `docker restart`.
const RESTART_GRACE_SECS: u64 = 10;

/// Délai des appels ordinaires au démon.
const API_TIMEOUT: Duration = Duration::from_secs(30);

/// Délai d'un `pull`. Dix minutes : une image d'un gigaoctet sur une liaison
/// domestique.
const PULL_TIMEOUT: Duration = Duration::from_secs(600);

/// Temps laissé à un conteneur pour devenir sain après son démarrage. Deux
/// minutes couvrent les `start_period` généreux des bases de données.
const HEALTH_WAIT: Duration = Duration::from_secs(120);

/// Sans `HEALTHCHECK`, on se contente de vérifier que le conteneur tourne
/// encore passé ce délai : un processus qui tombe au démarrage le fait vite.
const SETTLE_WAIT: Duration = Duration::from_secs(10);

const HEALTH_POLL: Duration = Duration::from_secs(2);

/// Ce que l'agent accepte de faire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Restart { name: String },
    Update { name: String, prune: bool },
}

impl Action {
    /// Vérifie une commande avant de la lancer. Tout ce qui est refusé ici
    /// l'est avec un message que l'interface affichera tel quel.
    pub fn from_command(command: &AgentCommand) -> Result<Self> {
        let name = command.arg_str("name").context("missing container name")?;
        validate_name(name)?;
        match command.kind.as_str() {
            CMD_CONTAINER_RESTART => Ok(Self::Restart { name: name.to_string() }),
            CMD_CONTAINER_UPDATE => Ok(Self::Update {
                name: name.to_string(),
                prune: command.arg_bool("prune").unwrap_or(true),
            }),
            other => bail!("unknown command kind '{other}'"),
        }
    }

    fn name(&self) -> &str {
        match self {
            Self::Restart { name } | Self::Update { name, .. } => name,
        }
    }
}

/// Un nom de conteneur tel que Docker les accepte, et qui n'est pas réservé.
///
/// La règle est stricte parce que le nom finit dans le chemin d'une requête au
/// démon : un `../` ou un `?` y changerait le sens de l'appel.
pub fn validate_name(name: &str) -> Result<()> {
    let mut chars = name.chars();
    let valid_first = chars.next().is_some_and(|c| c.is_ascii_alphanumeric());
    let valid_rest = chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'));
    if !valid_first || !valid_rest || name.len() > 128 {
        bail!("invalid container name '{name}'");
    }
    let lower = name.to_ascii_lowercase();
    if RESERVED_PREFIXES.iter().any(|prefix| lower.starts_with(prefix)) {
        bail!("'{name}' belongs to the monitoring stack and cannot be managed remotely");
    }
    Ok(())
}

/// Journal d'exécution d'une commande : ce qui sera rendu au serveur.
#[derive(Debug, Default)]
pub struct Log {
    lines: Vec<String>,
}

impl Log {
    pub fn push(&mut self, line: impl Into<String>) {
        let line = line.into();
        info!(step = %line, "command");
        self.lines.push(line);
    }

    /// Le journal, borné par la fin : en cas d'échec, c'est la dernière ligne
    /// qui explique.
    pub fn excerpt(&self) -> String {
        let text = self.lines.join("\n");
        if text.len() <= RESULT_MAX_BYTES {
            return text;
        }
        let mut start = text.len() - RESULT_MAX_BYTES + 1;
        while !text.is_char_boundary(start) {
            start += 1;
        }
        format!("…{}", &text[start..])
    }
}

/// Récupère et exécute les commandes du serveur, une à la fois.
pub struct CommandRunner {
    enabled: bool,
    client: PushClient,
    key: String,
    docker: DockerClient,
    /// La commande en cours, s'il y en a une.
    task: Option<JoinHandle<()>>,
}

impl CommandRunner {
    pub fn new(enabled: bool, client: PushClient, key: String, docker_socket: &Path) -> Self {
        Self { enabled, client, key, docker: DockerClient::new(docker_socket), task: None }
    }

    /// Vrai tant qu'une commande s'exécute.
    pub fn is_busy(&self) -> bool {
        self.task.as_ref().is_some_and(|task| !task.is_finished())
    }

    /// Demande les commandes en attente et lance la première exécutable.
    ///
    /// Ne bloque jamais plus qu'un aller-retour HTTP : l'exécution part dans sa
    /// propre tâche. Avec les actions désactivées, l'agent ne demande même pas ;
    /// le serveur finira par périmer la commande de lui-même.
    pub async fn poll(&mut self) {
        if !self.enabled || self.is_busy() {
            return;
        }
        self.task = None;

        let commands = match self.client.fetch_commands(&self.key).await {
            Ok(commands) => commands,
            Err(error) => {
                debug!(%error, "cannot fetch pending commands");
                return;
            }
        };
        let now_ms = chrono::Utc::now().timestamp_millis();
        for command in commands {
            if command.is_expired(now_ms) {
                warn!(id = command.id, kind = command.kind, "command expired before execution");
                report(
                    &self.client,
                    &self.key,
                    command.id,
                    CommandStatus::Failed,
                    "command expired (older than 10 min)".to_string(),
                )
                .await;
                continue;
            }
            let executor = Executor {
                client: self.client.clone(),
                key: self.key.clone(),
                docker: self.docker.clone(),
            };
            self.task = Some(tokio::spawn(async move { executor.execute(command).await }));
            return;
        }
    }

    /// Attend la fin de la commande en cours, au plus `timeout`.
    ///
    /// Appelé avant de quitter : une mise à jour interrompue à mi-chemin
    /// laisserait un conteneur renommé et un compte rendu jamais envoyé.
    pub async fn finish(&mut self, timeout: Duration) {
        let Some(task) = self.task.take() else { return };
        if task.is_finished() {
            return;
        }
        info!("waiting for the running command to finish");
        if tokio::time::timeout(timeout, task).await.is_err() {
            warn!(timeout_s = timeout.as_secs(), "command still running, giving up on it");
        }
    }
}

/// Compte rendu au serveur. Un échec d'envoi n'est que journalisé : le serveur
/// périmera la commande de lui-même, et l'action, elle, a bien eu lieu.
async fn report(client: &PushClient, key: &str, id: i64, status: CommandStatus, result: String) {
    let report = CommandReport { status, result };
    if let Err(error) = client.report_command(key, id, &report).await {
        warn!(id, status = status.as_str(), %error, "cannot report the command outcome");
    }
}

/// Tout ce qu'il faut pour exécuter une commande hors de la boucle principale.
struct Executor {
    client: PushClient,
    key: String,
    docker: DockerClient,
}

impl Executor {
    async fn execute(self, command: AgentCommand) {
        let action = match Action::from_command(&command) {
            Ok(action) => action,
            Err(error) => {
                warn!(id = command.id, kind = command.kind, %error, "command refused");
                report(
                    &self.client,
                    &self.key,
                    command.id,
                    CommandStatus::Failed,
                    format!("{error:#}"),
                )
                .await;
                return;
            }
        };
        info!(id = command.id, kind = command.kind, container = action.name(), "command started");
        report(&self.client, &self.key, command.id, CommandStatus::Running, String::new()).await;

        let mut log = Log::default();
        let outcome = match &action {
            Action::Restart { name } => restart(&self.docker, name, &mut log).await,
            Action::Update { name, prune } => update(&self.docker, name, *prune, &mut log).await,
        };
        let status = match outcome {
            Ok(()) => CommandStatus::Done,
            Err(error) => {
                log.push(format!("error: {error:#}"));
                CommandStatus::Failed
            }
        };
        info!(id = command.id, status = status.as_str(), "command finished");
        report(&self.client, &self.key, command.id, status, log.excerpt()).await;
    }
}

// ---------------------------------------------------------------- actions

/// `docker restart`, puis vérification que le conteneur tient debout.
async fn restart(docker: &DockerClient, name: &str, log: &mut Log) -> Result<()> {
    let before = inspect(docker, name).await?;
    log.push(format!(
        "restarting {name} ({})",
        before["Config"]["Image"].as_str().unwrap_or("unknown image")
    ));
    let path = format!("/containers/{name}/restart?t={RESTART_GRACE_SECS}");
    let timeout = API_TIMEOUT + Duration::from_secs(RESTART_GRACE_SECS);
    check(&docker.post(&path, None, timeout).await?, "restart")?;
    wait_until_healthy(docker, name, log).await
}

/// Tire l'image, recrée le conteneur, garde l'ancien sous la main jusqu'à ce
/// que le nouveau soit jugé sain.
async fn update(docker: &DockerClient, name: &str, prune: bool, log: &mut Log) -> Result<()> {
    let old = inspect(docker, name).await?;
    let old_id = old["Id"].as_str().context("container without an id")?.to_string();
    let old_image_id = old["Image"].as_str().unwrap_or_default().to_string();
    let reference = old["Config"]["Image"].as_str().unwrap_or_default().to_string();
    let was_running = old["State"]["Running"].as_bool().unwrap_or(false);
    if reference.is_empty() || looks_like_an_image_id(&reference) {
        bail!("{name} was created from an image id, not from a reference that can be pulled");
    }

    log.push(format!("pulling {reference}"));
    pull(docker, &reference, log).await?;
    let new_image = inspect_image(docker, &reference).await?;
    let new_image_id = new_image["Id"].as_str().unwrap_or_default().to_string();
    if new_image_id == old_image_id {
        log.push(format!("{name} already runs the latest {reference}: nothing to do"));
        return Ok(());
    }
    log.push(format!("new image {} (was {})", short(&new_image_id), short(&old_image_id)));

    // L'ancienne image est encore là : sa configuration dit ce que le
    // conteneur a hérité d'elle, et qu'il ne faut pas figer.
    let old_image = inspect_image(docker, &old_image_id).await.ok();
    let spec = CreateSpec::from_inspect(&old, old_image.as_ref().map(|image| &image["Config"]))?;
    let previous_name = format!("{name}{PREVIOUS_SUFFIX}");
    remove_stale_previous(docker, &previous_name, &old, log).await?;
    let rename = format!("/containers/{old_id}/rename?name={previous_name}");
    check(&docker.post(&rename, None, API_TIMEOUT).await?, "rename")
        .context("renaming the current container")?;
    log.push(format!("current container kept as {previous_name}"));

    // À partir d'ici, tout échec doit remettre l'ancien conteneur en service.
    let mut new_id = None;
    match replace(docker, name, &old_id, was_running, spec, log, &mut new_id).await {
        Ok(()) => {
            log.push(format!("{name} updated to {reference}"));
            remove_container(docker, &old_id, log, "previous container").await;
            if prune && !old_image_id.is_empty() {
                prune_image(docker, &old_image_id, log).await;
            }
            Ok(())
        }
        Err(error) => {
            log.push(format!("update failed, rolling back: {error:#}"));
            rollback(docker, name, &old_id, was_running, new_id, log).await;
            Err(error)
        }
    }
}

/// Crée et démarre le remplaçant. `new_id` est renseigné dès la création, pour
/// que l'appelant sache quoi nettoyer en cas d'échec.
async fn replace(
    docker: &DockerClient,
    name: &str,
    old_id: &str,
    was_running: bool,
    spec: CreateSpec,
    log: &mut Log,
    new_id: &mut Option<String>,
) -> Result<()> {
    let body = serde_json::to_string(&spec.body)?;
    let created: Value = docker
        .post(&format!("/containers/create?name={name}"), Some(&body), API_TIMEOUT)
        .await?
        .json("container creation")?;
    let id = created["Id"].as_str().context("creation answer without an id")?.to_string();
    *new_id = Some(id.clone());
    log.push(format!("created {name} ({})", short(&id)));

    for (network, endpoint) in spec.extra_networks {
        let body = json!({ "Container": id, "EndpointConfig": endpoint }).to_string();
        let connect = format!("/networks/{network}/connect");
        check(&docker.post(&connect, Some(&body), API_TIMEOUT).await?, "network connect")
            .with_context(|| format!("connecting {name} to network {network}"))?;
        log.push(format!("connected to network {network}"));
    }

    if was_running {
        stop_container(docker, old_id, log, "previous container").await?;
        check(&docker.post(&format!("/containers/{id}/start"), None, API_TIMEOUT).await?, "start")
            .context("starting the new container")?;
        log.push(format!("started {name}"));
        wait_until_healthy(docker, &id, log).await?;
    } else {
        log.push(format!("{name} was stopped: left stopped"));
    }
    Ok(())
}

/// Remet l'ancien conteneur à sa place. Chaque étape est tentée même si la
/// précédente a échoué : ce qui compte est de rendre le service.
async fn rollback(
    docker: &DockerClient,
    name: &str,
    old_id: &str,
    was_running: bool,
    new_id: Option<String>,
    log: &mut Log,
) {
    if let Some(id) = new_id {
        remove_container(docker, &id, log, "new container").await;
    }
    match docker.post(&format!("/containers/{old_id}/rename?name={name}"), None, API_TIMEOUT).await
    {
        Ok(response) if response.is_success() => log.push(format!("restored the name {name}")),
        Ok(response) => log.push(format!("cannot restore the name {name}: {}", response.message())),
        Err(error) => log.push(format!("cannot restore the name {name}: {error:#}")),
    }
    if was_running {
        match docker.post(&format!("/containers/{old_id}/start"), None, API_TIMEOUT).await {
            // 304 : il tournait encore, l'arrêt n'avait pas eu lieu.
            Ok(response) if response.is_success() || response.status == 304 => {
                log.push("previous container back in service".to_string());
            }
            Ok(response) => {
                log.push(format!("cannot restart the previous container: {}", response.message()));
            }
            Err(error) => log.push(format!("cannot restart the previous container: {error:#}")),
        }
    }
}

/// Un `.previous` oublié par une mise à jour interrompue — l'agent tué entre
/// le renommage et le nettoyage — est supprimé, mais seulement s'il est
/// reconnaissable : même image, mêmes étiquettes que le conteneur courant,
/// puisque celui-ci en est la copie. Un homonyme créé par quelqu'un d'autre
/// bloque la mise à jour plutôt que d'être écrasé.
async fn remove_stale_previous(
    docker: &DockerClient,
    previous_name: &str,
    current: &Value,
    log: &mut Log,
) -> Result<()> {
    let response = docker.get(&format!("/containers/{previous_name}/json")).await?;
    if response.status == 404 {
        return Ok(());
    }
    let stale: Value = response.json("container inspection")?;
    if !is_our_previous(&stale, current) {
        bail!("a container named {previous_name} already exists and is not ours");
    }
    let id = stale["Id"].as_str().unwrap_or(previous_name).to_string();
    log.push(format!("removing {previous_name} left by an earlier update"));
    remove_container(docker, &id, log, previous_name).await;
    Ok(())
}

/// Vrai si `stale` ressemble à l'ancêtre de `current` : même référence d'image
/// et mêmes étiquettes, que la recréation recopie à l'identique.
pub fn is_our_previous(stale: &Value, current: &Value) -> bool {
    stale["Config"]["Image"] == current["Config"]["Image"]
        && stale["Config"]["Labels"] == current["Config"]["Labels"]
}

async fn stop_container(docker: &DockerClient, id: &str, log: &mut Log, what: &str) -> Result<()> {
    let path = format!("/containers/{id}/stop?t={STOP_GRACE_SECS}");
    let timeout = API_TIMEOUT + Duration::from_secs(STOP_GRACE_SECS);
    let response = docker.post(&path, None, timeout).await?;
    // 304 : déjà arrêté.
    if !response.is_success() && response.status != 304 {
        bail!("cannot stop the {what}: {}", response.message());
    }
    log.push(format!("stopped the {what}"));
    Ok(())
}

/// Suppression forcée. Un échec est journalisé, jamais propagé : à ce stade on
/// nettoie, on ne décide plus de rien.
async fn remove_container(docker: &DockerClient, id: &str, log: &mut Log, what: &str) {
    let path = format!("/containers/{id}?force=1&v=0");
    match docker.delete(&path, API_TIMEOUT + Duration::from_secs(STOP_GRACE_SECS)).await {
        Ok(response) if response.is_success() || response.status == 404 => {
            log.push(format!("removed the {what}"));
        }
        Ok(response) => log.push(format!("cannot remove the {what}: {}", response.message())),
        Err(error) => log.push(format!("cannot remove the {what}: {error:#}")),
    }
}

/// Supprime l'ancienne image. Un `409` signifie qu'un autre conteneur s'en
/// sert encore : ce n'est pas un échec, elle sera reprise la prochaine fois.
async fn prune_image(docker: &DockerClient, image_id: &str, log: &mut Log) {
    match docker.delete(&format!("/images/{image_id}"), API_TIMEOUT).await {
        Ok(response) if response.is_success() => {
            log.push(format!("removed the old image {}", short(image_id)));
        }
        Ok(response) if response.status == 409 => {
            log.push(format!("old image {} kept: still in use", short(image_id)));
        }
        Ok(response) => log.push(format!("old image kept: {}", response.message())),
        Err(error) => log.push(format!("old image kept: {error:#}")),
    }
}

/// `docker pull`. Le démon répond `200` puis raconte sa progression en JSON ;
/// une erreur de dépôt n'apparaît que dans ce flux, jamais dans le statut.
async fn pull(docker: &DockerClient, reference: &str, log: &mut Log) -> Result<()> {
    let path = format!("/images/create?fromImage={}", encode_query(reference));
    let response = docker.post(&path, None, PULL_TIMEOUT).await.context("pulling the image")?;
    check(&response, "pull")?;
    let status = pull_outcome(&response.body)?;
    log.push(status);
    Ok(())
}

/// Lit le flux de progression d'un `pull` : la dernière ligne de statut, ou
/// l'erreur qu'il contient.
pub fn pull_outcome(body: &[u8]) -> Result<String> {
    let mut last_status = None;
    for event in serde_json::Deserializer::from_slice(body).into_iter::<Value>() {
        let Ok(event) = event else { break };
        if let Some(error) = event.get("error").and_then(Value::as_str) {
            let detail = event["errorDetail"]["message"].as_str().unwrap_or(error);
            bail!("pull failed: {detail}");
        }
        if let Some(status) = event.get("status").and_then(Value::as_str) {
            last_status = Some(status.to_string());
        }
    }
    Ok(last_status.unwrap_or_else(|| "pulled".to_string()))
}

/// Attend que le conteneur soit sain — ou, sans `HEALTHCHECK`, qu'il ait
/// simplement survécu à ses premières secondes.
async fn wait_until_healthy(docker: &DockerClient, id: &str, log: &mut Log) -> Result<()> {
    let started = tokio::time::Instant::now();
    loop {
        let inspected = inspect(docker, id).await?;
        let state = &inspected["State"];
        if !state["Running"].as_bool().unwrap_or(false) {
            bail!(
                "container is not running (status {}, exit code {})",
                state["Status"].as_str().unwrap_or("unknown"),
                state["ExitCode"].as_i64().unwrap_or(-1)
            );
        }
        match state["Health"]["Status"].as_str() {
            None => {
                if started.elapsed() >= SETTLE_WAIT {
                    log.push("running (no healthcheck)".to_string());
                    return Ok(());
                }
            }
            Some("healthy") => {
                log.push("healthy".to_string());
                return Ok(());
            }
            Some("unhealthy") => bail!("container reports unhealthy"),
            Some(_) => {
                if started.elapsed() >= HEALTH_WAIT {
                    bail!("container still not healthy after {} s", HEALTH_WAIT.as_secs());
                }
            }
        }
        tokio::time::sleep(HEALTH_POLL).await;
    }
}

async fn inspect(docker: &DockerClient, name: &str) -> Result<Value> {
    let response = docker.get(&format!("/containers/{name}/json")).await?;
    if response.status == 404 {
        bail!("no container named {name}");
    }
    response.json("container inspection")
}

async fn inspect_image(docker: &DockerClient, reference: &str) -> Result<Value> {
    docker.get(&format!("/images/{reference}/json")).await?.json("image inspection")
}

/// Transforme une réponse d'échec du démon en erreur lisible.
fn check(response: &DockerResponse, what: &str) -> Result<()> {
    if response.is_success() {
        return Ok(());
    }
    bail!("Docker refused the {what} ({}): {}", response.status, response.message())
}

fn looks_like_an_image_id(reference: &str) -> bool {
    reference.starts_with("sha256:")
        || (reference.len() == 64 && reference.chars().all(|c| c.is_ascii_hexdigit()))
}

/// Douze caractères d'un identifiant, comme `docker ps`.
fn short(id: &str) -> &str {
    let id = id.strip_prefix("sha256:").unwrap_or(id);
    id.get(..12).unwrap_or(id)
}

/// Encodage d'un paramètre de requête : seuls les caractères non réservés
/// passent tels quels.
fn encode_query(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

// ------------------------------------------------------- recréation

/// Ce qu'on envoie à `POST /containers/create` pour reproduire un conteneur.
#[derive(Debug, Clone, PartialEq)]
pub struct CreateSpec {
    /// `Config` du conteneur, complété de `HostConfig` et `NetworkingConfig`.
    pub body: Value,
    /// Réseaux au-delà du premier : l'API n'en accepte qu'un à la création,
    /// les autres se connectent après coup.
    pub extra_networks: Vec<(String, Value)>,
}

impl CreateSpec {
    /// Reconstruit la demande de création depuis une inspection.
    ///
    /// `Config` et `HostConfig` repassent tels quels, c'est ce que fait Docker
    /// lui-même pour `docker commit`. Les corrections sont celles que Watchtower
    /// a apprises à ses dépens : ce qui venait de l'ancienne image (variables,
    /// étiquettes, commande) est retiré pour que la nouvelle impose le sien, le
    /// nom d'hôte auto-attribué et l'alias réseau qui répète l'ancien
    /// identifiant sont oubliés, les volumes anonymes sont rattachés.
    pub fn from_inspect(inspect: &Value, old_image_config: Option<&Value>) -> Result<Self> {
        let id = inspect["Id"].as_str().unwrap_or_default();
        let short_id = short(id);
        let mut config = match inspect["Config"].clone() {
            Value::Object(map) => map,
            _ => bail!("inspection without a Config block"),
        };
        let mut host_config = match inspect["HostConfig"].clone() {
            Value::Object(map) => map,
            _ => bail!("inspection without a HostConfig block"),
        };
        if let Some(image) = old_image_config {
            subtract_image_defaults(&mut config, image);
        }

        // Sans `--hostname`, Docker donne au conteneur son propre identifiant
        // court : le recopier figerait un nom qui ne correspond plus à rien.
        if config.get("Hostname").and_then(Value::as_str) == Some(short_id) {
            config.remove("Hostname");
        }

        // Les volumes anonymes ne sont nommés que dans `Mounts` : sans les
        // rattacher explicitement, le nouveau conteneur repartirait avec des
        // volumes vides et les données resteraient orphelines.
        let mut binds: Vec<Value> =
            host_config.get("Binds").and_then(Value::as_array).cloned().unwrap_or_default();
        let mut covered: Vec<String> = binds
            .iter()
            .filter_map(Value::as_str)
            .filter_map(|bind| bind.split(':').nth(1).map(str::to_string))
            .collect();
        covered.extend(
            host_config
                .get("Mounts")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|mount| mount["Target"].as_str().map(str::to_string)),
        );
        for mount in inspect["Mounts"].as_array().into_iter().flatten() {
            let (Some(volume), Some(destination)) =
                (mount["Name"].as_str(), mount["Destination"].as_str())
            else {
                continue;
            };
            if mount["Type"].as_str() != Some("volume")
                || volume.is_empty()
                || covered.iter().any(|c| c == destination)
            {
                continue;
            }
            let mode = if mount["RW"].as_bool() == Some(false) { ":ro" } else { "" };
            binds.push(Value::String(format!("{volume}:{destination}{mode}")));
        }
        if !binds.is_empty() {
            host_config.insert("Binds".into(), Value::Array(binds));
        }

        let mut networks: Vec<(String, Value)> = inspect["NetworkSettings"]["Networks"]
            .as_object()
            .map(|map| {
                map.iter()
                    .map(|(network, endpoint)| {
                        (network.clone(), clean_endpoint(endpoint, short_id))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let mut endpoints = Map::new();
        if !networks.is_empty() {
            let (first, endpoint) = networks.remove(0);
            endpoints.insert(first, endpoint);
        }

        config.insert("HostConfig".into(), Value::Object(host_config));
        config.insert("NetworkingConfig".into(), json!({ "EndpointsConfig": endpoints }));
        Ok(Self { body: Value::Object(config), extra_networks: networks })
    }
}

/// Retire de la configuration ce que le conteneur tenait de son image.
///
/// L'inspection mélange ce que l'utilisateur a demandé et ce que l'image
/// apportait : `NGINX_VERSION`, les étiquettes du mainteneur, la commande par
/// défaut. Recopier tout cela figerait les valeurs de l'ancienne image sur le
/// conteneur censé tourner avec la nouvelle.
fn subtract_image_defaults(config: &mut Map<String, Value>, image: &Value) {
    for key in ["Env", "ExposedPorts", "Volumes", "Labels"] {
        let Some(from_image) = image.get(key).filter(|v| !v.is_null()) else { continue };
        match config.get_mut(key) {
            Some(Value::Array(entries)) => {
                let inherited = from_image.as_array().cloned().unwrap_or_default();
                entries.retain(|entry| !inherited.contains(entry));
            }
            Some(Value::Object(entries)) => {
                if let Some(inherited) = from_image.as_object() {
                    entries.retain(|k, v| inherited.get(k) != Some(v));
                }
            }
            _ => {}
        }
    }
    // Une commande identique à celle de l'image n'a pas été choisie par
    // l'utilisateur : la nouvelle image doit pouvoir en changer.
    for key in ["Cmd", "Entrypoint", "WorkingDir", "User"] {
        if let Some(from_image) = image.get(key)
            && config.get(key) == Some(from_image)
        {
            config.remove(key);
        }
    }
}

/// Ne garde d'un point de terminaison réseau que ce qui se demande à la
/// création ; adresses et identifiants d'instance sont attribués par Docker.
fn clean_endpoint(endpoint: &Value, short_id: &str) -> Value {
    let mut out = Map::new();
    let aliases: Vec<Value> = endpoint["Aliases"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter(|alias| *alias != short_id)
        .fold(Vec::new(), |mut acc, alias| {
            if !acc.iter().any(|a: &Value| a.as_str() == Some(alias)) {
                acc.push(Value::String(alias.to_string()));
            }
            acc
        });
    if !aliases.is_empty() {
        out.insert("Aliases".into(), Value::Array(aliases));
    }
    for key in ["IPAMConfig", "Links", "DriverOpts"] {
        if let Some(value) = endpoint.get(key)
            && !value.is_null()
        {
            out.insert(key.into(), value.clone());
        }
    }
    Value::Object(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command(kind: &str, args: Value) -> AgentCommand {
        AgentCommand { id: 7, kind: kind.into(), args, created_at_ms: 0 }
    }

    #[test]
    fn commands_are_turned_into_actions() {
        assert_eq!(
            Action::from_command(&command(CMD_CONTAINER_RESTART, json!({"name": "web"}))).unwrap(),
            Action::Restart { name: "web".into() }
        );
        assert_eq!(
            Action::from_command(&command(
                CMD_CONTAINER_UPDATE,
                json!({"name": "web", "prune": false})
            ))
            .unwrap(),
            Action::Update { name: "web".into(), prune: false }
        );
        // Sans consigne, l'ancienne image est nettoyée : c'est ce que veut
        // quelqu'un qui demande une mise à jour.
        assert_eq!(
            Action::from_command(&command(CMD_CONTAINER_UPDATE, json!({"name": "web"}))).unwrap(),
            Action::Update { name: "web".into(), prune: true }
        );
    }

    #[test]
    fn unknown_kinds_and_missing_names_are_refused() {
        assert!(
            Action::from_command(&command("container.explode", json!({"name": "web"}))).is_err()
        );
        assert!(Action::from_command(&command(CMD_CONTAINER_RESTART, json!({}))).is_err());
        assert!(
            Action::from_command(&command(CMD_CONTAINER_RESTART, json!({"name": " "}))).is_err()
        );
    }

    #[test]
    fn the_monitoring_stack_cannot_be_managed_remotely() {
        assert!(validate_name("ezymonit-ezymonit-1").is_err());
        assert!(validate_name("DumbMonit").is_err());
        assert!(validate_name("vaultwarden").is_ok());
    }

    #[test]
    fn names_that_would_change_the_request_path_are_refused() {
        // Le nom finit dans l'URL envoyée au démon : il ne doit rien pouvoir y
        // ajouter.
        assert!(validate_name("../containers").is_err());
        assert!(validate_name("web?force=1").is_err());
        assert!(validate_name("-web").is_err());
        assert!(validate_name("").is_err());
        assert!(validate_name("lab-victim").is_ok());
        assert!(validate_name("immich_server.2").is_ok());
    }

    #[test]
    fn the_log_excerpt_keeps_the_end() {
        let mut log = Log::default();
        for i in 0..500 {
            log.push(format!("step {i}: something happened on the way"));
        }
        let excerpt = log.excerpt();
        assert!(excerpt.len() <= RESULT_MAX_BYTES + '…'.len_utf8());
        assert!(excerpt.starts_with('…'));
        assert!(excerpt.ends_with("step 499: something happened on the way"));

        let mut small = Log::default();
        small.push("done");
        assert_eq!(small.excerpt(), "done");
    }

    #[test]
    fn the_pull_stream_yields_its_final_status_or_its_error() {
        let ok = br#"{"status":"Pulling from library/nginx","id":"1.27-alpine"}
{"status":"Digest: sha256:abc"}
{"status":"Status: Downloaded newer image for nginx:1.27-alpine"}"#;
        assert_eq!(
            pull_outcome(ok).unwrap(),
            "Status: Downloaded newer image for nginx:1.27-alpine"
        );

        let failed = br#"{"status":"Pulling from library/nope"}
{"errorDetail":{"message":"pull access denied for nope"},"error":"pull access denied for nope"}"#;
        let error = pull_outcome(failed).unwrap_err().to_string();
        assert!(error.contains("pull access denied"), "{error}");
        assert_eq!(pull_outcome(b"").unwrap(), "pulled");
    }

    #[test]
    fn image_references_are_encoded_for_the_query_string() {
        assert_eq!(encode_query("nginx:1.27-alpine"), "nginx%3A1.27-alpine");
        assert_eq!(encode_query("ghcr.io/a/b@sha256:1"), "ghcr.io%2Fa%2Fb%40sha256%3A1");
        assert!(looks_like_an_image_id("sha256:094b"));
        assert!(looks_like_an_image_id(&"a".repeat(64)));
        assert!(!looks_like_an_image_id("nginx:latest"));
        assert_eq!(short("sha256:516475cc129da42866742567714ddc681e5eed7b"), "516475cc129d");
    }

    /// Inspection réduite d'un conteneur Compose sur un réseau utilisateur,
    /// avec un volume anonyme et un second réseau.
    fn inspect_sample() -> Value {
        json!({
            "Id": "45dd9effdb61cab13b47bc6a1d1edf9fc2c19fcc339ff6d5ccc011f644e9363d",
            "Image": "sha256:516475cc129d",
            "Name": "/lab-victim",
            "State": {"Running": true},
            "Config": {
                "Hostname": "45dd9effdb61",
                "Image": "nginx:1.25-alpine",
                "Env": ["NGINX_VERSION=1.25.5"],
                "Labels": {"com.docker.compose.service": "lab-victim"},
                "Volumes": {"/var/cache/nginx": {}}
            },
            "HostConfig": {
                "Binds": ["/srv/web:/usr/share/nginx/html:ro"],
                "RestartPolicy": {"Name": "unless-stopped", "MaximumRetryCount": 0},
                "NetworkMode": "ezymonit_default"
            },
            "Mounts": [
                {"Type": "bind", "Source": "/srv/web", "Destination": "/usr/share/nginx/html", "RW": false},
                {"Type": "volume", "Name": "0f1e2d3c", "Destination": "/var/cache/nginx", "RW": true}
            ],
            "NetworkSettings": {"Networks": {
                "ezymonit_default": {
                    "Aliases": ["lab-victim", "lab-victim", "45dd9effdb61"],
                    "IPAMConfig": null, "Links": null, "DriverOpts": null,
                    "IPAddress": "172.20.0.12", "EndpointID": "37c4", "NetworkID": "04eb"
                },
                "backend": {
                    "Aliases": ["victim"],
                    "IPAMConfig": {"IPv4Address": "10.9.0.5"},
                    "IPAddress": "10.9.0.5"
                }
            }}
        })
    }

    #[test]
    fn the_create_request_reproduces_the_container_with_the_known_fixes() {
        let spec = CreateSpec::from_inspect(&inspect_sample(), None).expect("spec");
        let body = &spec.body;

        // Configuration et HostConfig repassent tels quels…
        assert_eq!(body["Image"], "nginx:1.25-alpine");
        assert_eq!(body["Env"][0], "NGINX_VERSION=1.25.5");
        assert_eq!(body["HostConfig"]["RestartPolicy"]["Name"], "unless-stopped");
        // … sauf le nom d'hôte auto-attribué.
        assert!(body.get("Hostname").is_none());

        // Le volume anonyme devient un montage explicite, le montage lié reste.
        let binds = body["HostConfig"]["Binds"].as_array().unwrap();
        assert_eq!(binds.len(), 2);
        assert_eq!(binds[1], "0f1e2d3c:/var/cache/nginx");

        // Un seul réseau à la création, sans l'alias qui répète l'identifiant
        // ni les champs attribués par Docker ; l'autre attend la connexion.
        let endpoints = body["NetworkingConfig"]["EndpointsConfig"].as_object().unwrap();
        assert_eq!(endpoints.len(), 1);
        let (network, endpoint) = endpoints.iter().next().unwrap();
        let (extra_network, extra_endpoint) = &spec.extra_networks[0];
        let names = [network.as_str(), extra_network.as_str()];
        assert!(names.contains(&"ezymonit_default") && names.contains(&"backend"));
        for endpoint in [endpoint, extra_endpoint] {
            assert!(endpoint.get("IPAddress").is_none());
            assert!(endpoint.get("EndpointID").is_none());
        }
        let default = if network == "ezymonit_default" { endpoint } else { extra_endpoint };
        assert_eq!(default["Aliases"], json!(["lab-victim"]));
        assert!(default.get("IPAMConfig").is_none(), "a null IPAMConfig is dropped");
        let backend = if network == "backend" { endpoint } else { extra_endpoint };
        assert_eq!(backend["IPAMConfig"]["IPv4Address"], "10.9.0.5");
    }

    #[test]
    fn what_came_from_the_old_image_is_not_frozen_onto_the_new_one() {
        let mut inspect = inspect_sample();
        inspect["Config"]["Env"] = json!(["NGINX_VERSION=1.25.5", "MARKER=yes", "PATH=/usr/bin"]);
        inspect["Config"]["Labels"] =
            json!({"maintainer": "NGINX", "lab": "victim", "com.docker.compose.service": "x"});
        inspect["Config"]["Cmd"] = json!(["nginx", "-g", "daemon off;"]);
        inspect["Config"]["Entrypoint"] = json!(["/docker-entrypoint.sh"]);
        inspect["Config"]["ExposedPorts"] = json!({"80/tcp": {}, "8443/tcp": {}});
        let image = json!({
            "Env": ["NGINX_VERSION=1.25.5", "PATH=/usr/bin"],
            "Labels": {"maintainer": "NGINX"},
            "Cmd": ["nginx", "-g", "daemon off;"],
            "Entrypoint": ["/docker-entrypoint.sh"],
            "ExposedPorts": {"80/tcp": {}}
        });
        let spec = CreateSpec::from_inspect(&inspect, Some(&image)).unwrap();
        assert_eq!(spec.body["Env"], json!(["MARKER=yes"]));
        assert_eq!(
            spec.body["Labels"],
            json!({"lab": "victim", "com.docker.compose.service": "x"})
        );
        assert_eq!(spec.body["ExposedPorts"], json!({"8443/tcp": {}}));
        assert!(spec.body.get("Cmd").is_none(), "the image's own command is not pinned");
        assert!(spec.body.get("Entrypoint").is_none());

        // Une commande choisie par l'utilisateur, elle, reste.
        inspect["Config"]["Cmd"] = json!(["sleep", "infinity"]);
        let spec = CreateSpec::from_inspect(&inspect, Some(&image)).unwrap();
        assert_eq!(spec.body["Cmd"], json!(["sleep", "infinity"]));
    }

    #[test]
    fn an_explicit_hostname_is_kept() {
        let mut inspect = inspect_sample();
        inspect["Config"]["Hostname"] = json!("web-1");
        let spec = CreateSpec::from_inspect(&inspect, None).unwrap();
        assert_eq!(spec.body["Hostname"], "web-1");
    }

    #[test]
    fn a_leftover_previous_container_is_recognised_by_its_image_and_labels() {
        let current = inspect_sample();
        let mut stale = inspect_sample();
        stale["Id"] = json!("older");
        stale["Config"]["Hostname"] = json!("older-host");
        assert!(is_our_previous(&stale, &current));
        stale["Config"]["Labels"]["com.docker.compose.service"] = json!("something-else");
        assert!(!is_our_previous(&stale, &current));
    }

    #[test]
    fn an_inspection_without_configuration_is_refused() {
        assert!(CreateSpec::from_inspect(&json!({"Id": "x"}), None).is_err());
    }

    fn client() -> PushClient {
        PushClient::new("http://127.0.0.1:1", "ezym_test", Duration::from_secs(1)).expect("client")
    }

    #[tokio::test]
    async fn a_disabled_runner_never_polls() {
        let socket = Path::new("/tmp/ezymonit-no-such-socket.sock");
        let mut runner = CommandRunner::new(false, client(), "key".into(), socket);
        runner.poll().await;
        assert!(!runner.is_busy());
        runner.finish(Duration::from_millis(10)).await;
    }

    #[tokio::test]
    async fn an_unreachable_server_is_not_an_error() {
        let socket = Path::new("/tmp/ezymonit-no-such-socket.sock");
        let mut runner = CommandRunner::new(true, client(), "key".into(), socket);
        runner.poll().await;
        assert!(!runner.is_busy());
    }

    #[tokio::test]
    async fn a_missing_container_fails_cleanly() {
        let docker = DockerClient::new(Path::new("/var/run/docker.sock"));
        if !docker.exists() {
            return;
        }
        let mut log = Log::default();
        let error = restart(&docker, "ezymonit-test-no-such-container", &mut log)
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("no container named"), "{error}");
    }

    /// Redémarre pour de bon le conteneur de laboratoire.
    #[tokio::test]
    #[ignore]
    async fn the_lab_victim_is_restarted_for_real() {
        let docker = DockerClient::new(Path::new("/var/run/docker.sock"));
        let mut log = Log::default();
        restart(&docker, "lab-victim", &mut log).await.expect("restart");
        assert!(log.excerpt().contains("running (no healthcheck)"));
    }
}
