//! Inventaire des conteneurs Docker, lu sur le socket local, et petit client
//! HTTP réutilisé par les actions (`commands.rs`).
//!
//! La collecte reste en lecture seule : `GET /containers/json`, puis une
//! inspection par conteneur pour la santé et le nombre de redémarrages, et une
//! inspection par image — mise en cache — pour son âge et ses empreintes.
//! L'absence de Docker n'est pas une erreur, et une erreur de lecture ne fait
//! jamais échouer le cycle de collecte.
//!
//! Le dialogue HTTP est écrit à la main plutôt que délégué à une bibliothèque
//! cliente : parler HTTP/1.1 sur un socket Unix pour quelques requêtes tient en
//! une centaine de lignes, là où `bollard` ou `hyperlocal` ajouteraient une
//! dépendance entière au binaire de l'agent.

// Sur les systèmes sans socket Unix, seule la façade publique est compilée : le
// reste n'a pas d'appelant sans pour autant mériter d'être retiré du code.
#![cfg_attr(not(unix), allow(dead_code))]

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

/// État de santé d'un conteneur, tel que Docker le rapporte.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Health {
    /// Pas de `HEALTHCHECK` dans l'image.
    #[default]
    None,
    Healthy,
    Unhealthy,
    Starting,
}

impl Health {
    pub fn as_value(self) -> f64 {
        match self {
            Self::None => 0.0,
            Self::Healthy => 1.0,
            Self::Unhealthy => 2.0,
            Self::Starting => 3.0,
        }
    }

    pub fn parse(text: &str) -> Self {
        match text {
            "healthy" => Self::Healthy,
            "unhealthy" => Self::Unhealthy,
            "starting" => Self::Starting,
            _ => Self::None,
        }
    }
}

/// Un conteneur, réduit à ce qui se surveille.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ContainerStat {
    pub id: String,
    pub name: String,
    pub image: String,
    pub image_id: String,
    pub running: bool,
    pub health: Health,
    pub restart_count: u64,
    /// Secondes depuis le dernier démarrage ; zéro à l'arrêt.
    pub uptime_secs: u64,
    /// Secondes depuis la construction de l'image. `None` : image non inspectée.
    pub image_age_secs: Option<u64>,
    /// `None` : pas encore vérifié, ou vérification impossible.
    pub update_available: Option<bool>,
    pub labels: BTreeMap<String, String>,
}

/// Version d'API demandée. `v1.41` correspond à Docker 20.10, sorti en 2020 :
/// suffisamment ancienne pour être acceptée partout, et le démon accepte toute
/// version antérieure à la sienne.
const API_VERSION: &str = "/v1.41";

/// Au-delà, on considère que le démon ne répondra pas. Volontairement court : la
/// collecte des conteneurs ne doit jamais retarder l'envoi des métriques système.
const READ_TIMEOUT: Duration = Duration::from_secs(3);

/// Plafond par défaut de conteneurs détaillés par cycle. Chaque conteneur
/// vaut six séries et une inspection : au-delà de deux cents, le coût des
/// inspections dépasserait celui du reste de la collecte, et les séries
/// noieraient la base.
pub const DEFAULT_MAX_CONTAINERS: usize = 200;

/// Inventaire d'un cycle : les compteurs portent sur tous les conteneurs, le
/// détail sur les seuls premiers du plafond.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ContainerInventory {
    /// Conteneurs présents, en marche ou non.
    pub total: usize,
    /// Conteneurs en marche, plafond compris.
    pub running: usize,
    /// Conteneurs détaillés — inspectés, et remontés avec leurs étiquettes.
    pub detailed: Vec<ContainerStat>,
}

impl ContainerInventory {
    /// Retient au plus `max_detailed` conteneurs, les conteneurs en marche
    /// d'abord puis par nom : l'ordre doit être stable d'un cycle à l'autre, sans
    /// quoi les séries de la frontière apparaîtraient et disparaîtraient au gré
    /// de l'ordre du démon — la pire des cardinalités.
    pub fn new(mut containers: Vec<ContainerStat>, max_detailed: usize) -> Self {
        let total = containers.len();
        let running = containers.iter().filter(|container| container.running).count();
        containers.sort_by(|a, b| b.running.cmp(&a.running).then_with(|| a.name.cmp(&b.name)));
        containers.truncate(max_detailed);
        Self { total, running, detailed: containers }
    }

    /// Conteneurs présents mais non détaillés.
    pub fn skipped(&self) -> usize {
        self.total.saturating_sub(self.detailed.len())
    }
}

/// Une image ne change pas d'âge ni d'empreintes : la réinspecter à chaque cycle
/// serait du gaspillage. Dix minutes couvrent un `docker pull` manuel.
const IMAGE_CACHE_TTL: Duration = Duration::from_secs(600);

// ------------------------------------------------------------------ client

/// Client HTTP minimal sur le socket Unix du démon.
#[derive(Debug, Clone)]
pub struct DockerClient {
    socket: PathBuf,
}

/// Réponse brute du démon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DockerResponse {
    pub status: u16,
    pub body: Vec<u8>,
}

impl DockerResponse {
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }

    /// Message d'erreur `{"message": …}` du démon, ou le corps brut tronqué.
    pub fn message(&self) -> String {
        serde_json::from_slice::<serde_json::Value>(&self.body)
            .ok()
            .and_then(|value| value.get("message")?.as_str().map(str::to_string))
            .unwrap_or_else(|| {
                String::from_utf8_lossy(&self.body).trim().chars().take(200).collect()
            })
    }

    /// Décode le corps, ou rend l'erreur du démon lisible.
    pub fn json<T: serde::de::DeserializeOwned>(&self, what: &str) -> Result<T> {
        if !self.is_success() {
            bail!("Docker answered {} to {what}: {}", self.status, self.message());
        }
        serde_json::from_slice(&self.body).with_context(|| format!("unreadable {what}"))
    }
}

impl DockerClient {
    pub fn new(socket: &Path) -> Self {
        Self { socket: socket.to_path_buf() }
    }

    pub fn socket(&self) -> &Path {
        &self.socket
    }

    pub fn exists(&self) -> bool {
        self.socket.exists()
    }

    pub async fn get(&self, path: &str) -> Result<DockerResponse> {
        self.request("GET", path, None, READ_TIMEOUT).await
    }

    pub async fn delete(&self, path: &str, timeout: Duration) -> Result<DockerResponse> {
        self.request("DELETE", path, None, timeout).await
    }

    pub async fn post(
        &self,
        path: &str,
        json_body: Option<&str>,
        timeout: Duration,
    ) -> Result<DockerResponse> {
        self.request("POST", path, json_body, timeout).await
    }

    #[cfg(unix)]
    pub async fn request(
        &self,
        method: &str,
        path: &str,
        body: Option<&str>,
        timeout: Duration,
    ) -> Result<DockerResponse> {
        match tokio::time::timeout(timeout, raw_request(&self.socket, method, path, body)).await {
            Ok(result) => result,
            Err(_) => bail!("the Docker daemon did not answer {method} {path} in time"),
        }
    }

    /// Sur Windows, Docker s'expose par un tube nommé, que `tokio::net` ne
    /// couvre pas sans dépendance supplémentaire. La fonctionnalité est donc
    /// absente plutôt que mal faite, et le reste de la collecte est identique.
    #[cfg(not(unix))]
    pub async fn request(
        &self,
        _method: &str,
        _path: &str,
        _body: Option<&str>,
        _timeout: Duration,
    ) -> Result<DockerResponse> {
        bail!("Docker is only reachable through a Unix socket on this platform")
    }
}

#[cfg(unix)]
async fn raw_request(
    socket: &Path,
    method: &str,
    path: &str,
    body: Option<&str>,
) -> Result<DockerResponse> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let mut stream =
        tokio::net::UnixStream::connect(socket).await.context("connecting to the Docker socket")?;

    // `Connection: close` évite d'avoir à gérer le maintien en vie : le démon
    // ferme, et la lecture jusqu'au bout suffit à délimiter la réponse.
    let mut request = format!(
        "{method} {API_VERSION}{path} HTTP/1.1\r\n\
         Host: localhost\r\n\
         Accept: application/json\r\n\
         User-Agent: ezymonit-agent/{}\r\n\
         Connection: close\r\n",
        env!("CARGO_PKG_VERSION")
    );
    match body {
        Some(json) => {
            request.push_str(&format!(
                "Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{json}",
                json.len()
            ));
        }
        // Un `POST` sans corps doit tout de même annoncer une longueur nulle,
        // sans quoi le démon attend un corps qui ne viendra jamais.
        None if method == "POST" => request.push_str("Content-Length: 0\r\n\r\n"),
        None => request.push_str("\r\n"),
    }
    stream.write_all(request.as_bytes()).await.context("sending the request to Docker")?;

    let mut response = Vec::new();
    stream.read_to_end(&mut response).await.context("reading the Docker response")?;

    parse_response(&response)
}

/// Découpe une réponse HTTP/1.1 en statut et corps, en gérant le découpage en
/// morceaux.
fn parse_response(response: &[u8]) -> Result<DockerResponse> {
    let separator = b"\r\n\r\n";
    let split = response
        .windows(separator.len())
        .position(|window| window == separator)
        .context("truncated HTTP response: incomplete headers")?;

    let head = String::from_utf8_lossy(&response[..split]);
    let body = &response[split + separator.len()..];

    let status = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok())
        .context("unreadable HTTP status line")?;

    let chunked =
        head.lines().skip(1).filter_map(|line| line.split_once(':')).any(|(name, value)| {
            name.eq_ignore_ascii_case("transfer-encoding")
                && value.to_ascii_lowercase().contains("chunked")
        });

    let body = if chunked { dechunk(body)? } else { body.to_vec() };
    Ok(DockerResponse { status, body })
}

/// Recompose un corps découpé en morceaux (`Transfer-Encoding: chunked`).
fn dechunk(body: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(body.len());
    let mut rest = body;

    loop {
        let line_end = rest
            .windows(2)
            .position(|window| window == b"\r\n")
            .context("HTTP chunk without a line ending")?;
        // La taille peut être suivie d'extensions séparées par un point-virgule,
        // que la spécification autorise et que personne n'utilise — mais les
        // ignorer coûte une ligne.
        let header = String::from_utf8_lossy(&rest[..line_end]);
        let size_text = header.split(';').next().unwrap_or("").trim();
        let size = usize::from_str_radix(size_text, 16)
            .with_context(|| format!("invalid chunk size '{size_text}'"))?;

        rest = &rest[line_end + 2..];
        if size == 0 {
            return Ok(out);
        }
        if rest.len() < size {
            bail!("truncated HTTP chunk: {} bytes expected, {} received", size, rest.len());
        }
        out.extend_from_slice(&rest[..size]);
        // Chaque morceau est suivi de son propre `\r\n`.
        rest = rest.get(size + 2..).unwrap_or(&[]);
    }
}

// --------------------------------------------------------------- analyse

/// Ce que `GET /containers/json` renvoie et que nous retenons.
#[derive(Debug, Deserialize)]
struct DockerContainer {
    #[serde(rename = "Id", default)]
    id: String,
    #[serde(rename = "Names", default)]
    names: Vec<String>,
    #[serde(rename = "Image", default)]
    image: String,
    #[serde(rename = "ImageID", default)]
    image_id: String,
    #[serde(rename = "State", default)]
    state: String,
    #[serde(rename = "Labels", default)]
    labels: BTreeMap<String, String>,
}

/// Ce que `GET /containers/{id}/json` apporte en plus.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ContainerDetail {
    pub running: bool,
    pub health: Health,
    pub restart_count: u64,
    /// Secondes depuis le démarrage ; zéro à l'arrêt ou sans horodatage.
    pub uptime_secs: u64,
}

#[derive(Debug, Deserialize)]
struct InspectContainer {
    #[serde(rename = "RestartCount", default)]
    restart_count: u64,
    #[serde(rename = "State", default)]
    state: InspectState,
}

#[derive(Debug, Deserialize, Default)]
struct InspectState {
    #[serde(rename = "Running", default)]
    running: bool,
    #[serde(rename = "StartedAt", default)]
    started_at: String,
    #[serde(rename = "Health")]
    health: Option<InspectHealth>,
}

#[derive(Debug, Deserialize)]
struct InspectHealth {
    #[serde(rename = "Status", default)]
    status: String,
}

/// Ce que `GET /images/{id}/json` apporte.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ImageDetail {
    pub created_at_secs: Option<i64>,
    pub repo_digests: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct InspectImage {
    #[serde(rename = "Created", default)]
    created: String,
    #[serde(rename = "RepoDigests", default)]
    repo_digests: Vec<String>,
}

pub fn parse_containers(body: &[u8]) -> Result<Vec<ContainerStat>> {
    let raw: Vec<DockerContainer> =
        serde_json::from_slice(body).context("unreadable Docker inventory")?;

    Ok(raw
        .into_iter()
        .map(|container| ContainerStat {
            id: container.id,
            // Docker préfixe les noms d'une barre oblique, héritage de l'époque des
            // liens entre conteneurs. Elle n'apporte rien à une étiquette.
            name: container
                .names
                .first()
                .map(|name| name.trim_start_matches('/').to_string())
                .unwrap_or_else(|| "unnamed".to_string()),
            image: container.image,
            image_id: container.image_id,
            running: container.state == "running",
            labels: container.labels,
            ..ContainerStat::default()
        })
        .collect())
}

/// Lit l'inspection d'un conteneur. `now_secs` sert au calcul du temps de
/// fonctionnement, passé en paramètre pour rester testable.
pub fn parse_container_detail(body: &[u8], now_secs: i64) -> Result<ContainerDetail> {
    let raw: InspectContainer =
        serde_json::from_slice(body).context("unreadable container inspection")?;
    let uptime_secs = if raw.state.running {
        parse_rfc3339_secs(&raw.state.started_at)
            .map(|started| now_secs.saturating_sub(started).max(0) as u64)
            .unwrap_or(0)
    } else {
        0
    };
    Ok(ContainerDetail {
        running: raw.state.running,
        health: raw.state.health.map(|h| Health::parse(&h.status)).unwrap_or_default(),
        restart_count: raw.restart_count,
        uptime_secs,
    })
}

pub fn parse_image_detail(body: &[u8]) -> Result<ImageDetail> {
    let raw: InspectImage = serde_json::from_slice(body).context("unreadable image inspection")?;
    Ok(ImageDetail {
        created_at_secs: parse_rfc3339_secs(&raw.created),
        repo_digests: raw.repo_digests,
    })
}

/// Horodatage RFC 3339 de Docker (nanosecondes comprises) en secondes Unix.
/// L'année 1 — « jamais » pour Docker — est traitée comme absente.
fn parse_rfc3339_secs(text: &str) -> Option<i64> {
    let parsed = chrono::DateTime::parse_from_rfc3339(text).ok()?;
    let secs = parsed.timestamp();
    if secs <= 0 { None } else { Some(secs) }
}

// ------------------------------------------------------------- inventaire

/// Lecteur d'inventaire, conservé d'un cycle à l'autre pour le cache des images.
pub struct DockerProbe {
    client: DockerClient,
    images: HashMap<String, (Instant, ImageDetail)>,
    /// Plafond de conteneurs détaillés par cycle.
    max_containers: usize,
}

impl DockerProbe {
    pub fn new(socket: &Path, max_containers: usize) -> Self {
        Self { client: DockerClient::new(socket), images: HashMap::new(), max_containers }
    }

    /// Liste les conteneurs, en marche ou non, avec leur détail.
    ///
    /// Renvoie `None` — et non une erreur — quand le socket est absent : c'est le
    /// cas nominal sur une machine sans Docker, et il ne doit produire aucune
    /// trace inquiétante ni aucune série vide.
    pub async fn read(&mut self) -> Option<ContainerInventory> {
        if !self.client.exists() {
            return None;
        }
        let containers = match self.client.get("/containers/json?all=1").await {
            Ok(response) if response.is_success() => match parse_containers(&response.body) {
                Ok(list) => list,
                Err(error) => {
                    tracing::warn!(%error, "cannot read Docker containers");
                    return None;
                }
            },
            // Le cas le plus fréquent : l'agent n'est pas dans le groupe `docker`.
            // Un avertissement suffit, la collecte système continue normalement.
            Ok(response) => {
                tracing::warn!(
                    status = response.status,
                    message = %response.message(),
                    socket = %self.client.socket().display(),
                    "cannot read Docker containers"
                );
                return None;
            }
            Err(error) => {
                tracing::warn!(%error, socket = %self.client.socket().display(), "cannot read Docker containers");
                return None;
            }
        };

        let mut inventory = ContainerInventory::new(containers, self.max_containers);
        if inventory.skipped() > 0 {
            tracing::debug!(
                total = inventory.total,
                detailed = inventory.detailed.len(),
                "container cap reached, the rest is only counted"
            );
        }
        // Seuls les conteneurs retenus sont inspectés : c'est le plafond qui
        // borne le nombre de requêtes au démon, pas l'inventaire.
        let now_secs = chrono::Utc::now().timestamp();
        for container in inventory.detailed.iter_mut() {
            let path = format!("/containers/{}/json", container.id);
            match self.client.get(&path).await {
                Ok(response) if response.is_success() => {
                    match parse_container_detail(&response.body, now_secs) {
                        Ok(detail) => {
                            container.running = detail.running;
                            container.health = detail.health;
                            container.restart_count = detail.restart_count;
                            container.uptime_secs = detail.uptime_secs;
                        }
                        Err(error) => {
                            tracing::debug!(%error, name = container.name, "container detail skipped")
                        }
                    }
                }
                // Un conteneur disparu entre la liste et l'inspection n'est pas une
                // panne : il ne sera simplement pas détaillé ce cycle-ci.
                Ok(_) => {}
                Err(error) => {
                    tracing::debug!(%error, name = container.name, "container detail skipped")
                }
            }

            if let Some(image) = self.image_detail(&container.image_id).await {
                container.image_age_secs = image
                    .created_at_secs
                    .map(|created| now_secs.saturating_sub(created).max(0) as u64);
            }
        }
        Some(inventory)
    }

    /// Détail d'une image, depuis le cache ou le démon.
    async fn image_detail(&mut self, image_id: &str) -> Option<ImageDetail> {
        if image_id.is_empty() {
            return None;
        }
        if let Some((at, detail)) = self.images.get(image_id)
            && at.elapsed() < IMAGE_CACHE_TTL
        {
            return Some(detail.clone());
        }
        let path = format!("/images/{image_id}/json");
        let detail = match self.client.get(&path).await {
            Ok(response) if response.is_success() => parse_image_detail(&response.body).ok()?,
            _ => return None,
        };
        self.images.insert(image_id.to_string(), (Instant::now(), detail.clone()));
        Some(detail)
    }

    /// Empreintes locales connues d'une image, pour la vérification de mise à jour.
    pub fn repo_digests(&self, image_id: &str) -> Vec<String> {
        self.images.get(image_id).map(|(_, detail)| detail.repo_digests.clone()).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Extrait le corps d'une réponse `200`, ou échoue sur tout autre statut.
    fn extract_body(response: &[u8]) -> Result<Vec<u8>> {
        let parsed = parse_response(response)?;
        if parsed.status != 200 {
            bail!("the Docker daemon answered {}", parsed.status);
        }
        Ok(parsed.body)
    }

    const INVENTORY: &str = r#"[
        {"Id":"5625ba4f478c33149c3d5fc33c5348ce252860d2a685b2d69a917eccbd67bac4","Names":["/vaultwarden"],"Image":"vaultwarden/server:1.30","ImageID":"sha256:094b","State":"running","Labels":{"com.docker.compose.project":"docker"}},
        {"Names":["/sauvegarde"],"Image":"restic:latest","State":"exited"}
    ]"#;

    const INSPECT: &str = r#"{
        "RestartCount": 3,
        "State": {"Status":"running","Running":true,"StartedAt":"2026-09-15T10:08:06.789112102Z","FinishedAt":"0001-01-01T00:00:00Z",
                  "Health":{"Status":"healthy","FailingStreak":0,"Log":[]}},
        "Name": "/vaultwarden",
        "Image": "sha256:094b"
    }"#;

    const IMAGE: &str = r#"{"Id":"sha256:094b","RepoDigests":["vaultwarden/server@sha256:094b"],"RepoTags":["vaultwarden/server:latest"],"Created":"2026-08-22T12:20:23.445788501Z"}"#;

    #[test]
    fn a_response_with_a_content_length_is_read_as_is() {
        let response =
            format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n{INVENTORY}");
        let body = extract_body(response.as_bytes()).expect("corps");
        assert_eq!(body, INVENTORY.as_bytes());
    }

    #[test]
    fn a_chunked_response_is_reassembled() {
        // C'est la forme que le démon Docker utilise en pratique ; sans ce
        // traitement, le JSON serait précédé d'une taille hexadécimale et illisible.
        let first = &INVENTORY[..40];
        let second = &INVENTORY[40..];
        let response = format!(
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n{first}\r\n{:x}\r\n{second}\r\n0\r\n\r\n",
            first.len(),
            second.len()
        );
        let body = extract_body(response.as_bytes()).expect("corps");
        assert_eq!(String::from_utf8(body).unwrap(), INVENTORY);
    }

    #[test]
    fn a_chunk_size_with_an_extension_is_still_understood() {
        let response =
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2;nom=valeur\r\n[]\r\n0\r\n\r\n";
        assert_eq!(extract_body(response.as_bytes()).unwrap(), b"[]");
    }

    #[test]
    fn an_error_status_is_reported_rather_than_parsed() {
        let response = "HTTP/1.1 403 Forbidden\r\n\r\n{\"message\":\"refusé\"}";
        let error = extract_body(response.as_bytes()).unwrap_err().to_string();
        assert!(error.contains("403"), "unexpected message: {error}");
    }

    #[test]
    fn a_non_200_status_is_still_parsed_with_its_message() {
        // Les actions ont besoin du statut et du message : un `404` sur un
        // conteneur inconnu doit être rapporté tel quel, pas comme une panne.
        let response = "HTTP/1.1 404 Not Found\r\nContent-Type: application/json\r\n\r\n{\"message\":\"No such container: x\"}";
        let parsed = parse_response(response.as_bytes()).expect("réponse");
        assert_eq!(parsed.status, 404);
        assert!(!parsed.is_success());
        assert_eq!(parsed.message(), "No such container: x");
    }

    #[test]
    fn a_truncated_response_is_refused_instead_of_silently_accepted() {
        assert!(extract_body(b"HTTP/1.1 200 OK\r\nContent-Type: app").is_err());
    }

    #[test]
    fn the_inventory_becomes_container_statistics() {
        let containers = parse_containers(INVENTORY.as_bytes()).expect("inventaire");
        assert_eq!(containers.len(), 2);
        assert_eq!(containers[0].name, "vaultwarden");
        assert_eq!(containers[0].image, "vaultwarden/server:1.30");
        assert_eq!(containers[0].image_id, "sha256:094b");
        assert!(containers[0].running);
        assert_eq!(
            containers[0].labels.get("com.docker.compose.project").map(String::as_str),
            Some("docker")
        );
        assert_eq!(containers[1].name, "sauvegarde");
        assert!(!containers[1].running);
        assert_eq!(containers[1].health, Health::None);
    }

    #[test]
    fn an_unnamed_container_still_gets_a_label() {
        // Une étiquette vide fusionnerait toutes les séries anonymes en une seule.
        let containers =
            parse_containers(br#"[{"Names":[],"Image":"x","State":"running"}]"#).expect("liste");
        assert_eq!(containers[0].name, "unnamed");
    }

    #[test]
    fn an_empty_inventory_is_valid() {
        assert!(parse_containers(b"[]").expect("liste").is_empty());
    }

    #[test]
    fn the_inspection_yields_health_restarts_and_uptime() {
        // 2026-09-15T10:08:06Z = 1789466886 ; une heure plus tard.
        let detail = parse_container_detail(INSPECT.as_bytes(), 1_789_466_886 + 3_600).unwrap();
        assert_eq!(
            detail,
            ContainerDetail {
                running: true,
                health: Health::Healthy,
                restart_count: 3,
                uptime_secs: 3_600
            }
        );
    }

    #[test]
    fn a_stopped_container_has_no_uptime_and_no_health() {
        let body =
            br#"{"RestartCount":0,"State":{"Running":false,"StartedAt":"2026-09-15T10:08:06Z"}}"#;
        let detail = parse_container_detail(body, 1_800_000_000).unwrap();
        assert_eq!(detail.uptime_secs, 0);
        assert_eq!(detail.health, Health::None);
        assert!(!detail.running);
    }

    #[test]
    fn health_states_map_to_the_documented_values() {
        assert_eq!(Health::parse("healthy").as_value(), 1.0);
        assert_eq!(Health::parse("unhealthy").as_value(), 2.0);
        assert_eq!(Health::parse("starting").as_value(), 3.0);
        assert_eq!(Health::parse("none").as_value(), 0.0);
    }

    #[test]
    fn the_image_inspection_yields_its_creation_and_digests() {
        let image = parse_image_detail(IMAGE.as_bytes()).unwrap();
        assert_eq!(image.created_at_secs, Some(1_787_401_223));
        assert_eq!(image.repo_digests, vec!["vaultwarden/server@sha256:094b".to_string()]);
    }

    #[test]
    fn dockers_year_one_means_never() {
        assert_eq!(parse_rfc3339_secs("0001-01-01T00:00:00Z"), None);
        assert_eq!(parse_rfc3339_secs(""), None);
    }

    #[tokio::test]
    async fn a_missing_socket_is_not_an_error() {
        let missing = Path::new("/tmp/ezymonit-socket-inexistant.sock");
        assert!(!DockerClient::new(missing).exists());
        assert!(DockerProbe::new(missing, DEFAULT_MAX_CONTAINERS).read().await.is_none());
    }

    #[test]
    fn the_inventory_keeps_running_containers_first_within_the_cap() {
        let stat = |name: &str, running: bool| ContainerStat {
            name: name.into(),
            running,
            ..ContainerStat::default()
        };
        let inventory = ContainerInventory::new(
            vec![
                stat("zeta", false),
                stat("beta", true),
                stat("alpha", false),
                stat("gamma", true),
            ],
            3,
        );
        assert_eq!(inventory.total, 4);
        assert_eq!(inventory.running, 2);
        assert_eq!(inventory.skipped(), 1);
        let names: Vec<&str> = inventory.detailed.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["beta", "gamma", "alpha"]);
    }

    #[test]
    fn a_zero_cap_only_counts() {
        let inventory =
            ContainerInventory::new(parse_containers(INVENTORY.as_bytes()).expect("inventaire"), 0);
        assert_eq!(inventory.total, 2);
        assert_eq!(inventory.running, 1);
        assert!(inventory.detailed.is_empty());
        assert_eq!(inventory.skipped(), 2);
    }
}
