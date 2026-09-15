//! Vérification des mises à jour d'images auprès de leur dépôt.
//!
//! Une image est « à jour » quand l'empreinte du manifeste que le dépôt publie
//! pour son étiquette correspond à l'une des empreintes locales (`RepoDigests`).
//! C'est exactement ce que font Watchtower et Diun, et cela ne télécharge rien :
//! un `HEAD` sur le manifeste suffit, le dépôt renvoie l'empreinte en en-tête.
//!
//! Le tout tourne dans une tâche de fond, au plus une fois par heure et par
//! image : le cycle de collecte ne fait que lire le dernier résultat connu. Le
//! moindre échec — dépôt privé, panne réseau, image construite localement —
//! vaut « inconnu », jamais « à jour ».

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use tracing::debug;

/// Période minimale entre deux vérifications d'une même image.
pub const CHECK_INTERVAL: Duration = Duration::from_secs(3600);

/// Délai de chaque requête vers un dépôt. Un dépôt lent ne doit pas monopoliser
/// la tâche : les autres images attendent derrière.
const HTTP_TIMEOUT: Duration = Duration::from_secs(10);

/// Types de manifeste acceptés. Les listes multi-architectures d'abord — c'est
/// leur empreinte que `docker pull` enregistre — puis les manifestes simples pour
/// les images publiées pour une seule architecture.
const ACCEPT: &str = "application/vnd.docker.distribution.manifest.list.v2+json, \
     application/vnd.oci.image.index.v1+json, \
     application/vnd.docker.distribution.manifest.v2+json, \
     application/vnd.oci.image.manifest.v1+json";

/// Une référence d'image décomposée.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageRef {
    /// Hôte du dépôt, port compris (`registry-1.docker.io`, `ghcr.io`,
    /// `localhost:5000`).
    pub registry: String,
    /// Chemin du dépôt (`library/nginx`, `immich-app/immich-server`).
    pub repository: String,
    pub tag: String,
}

impl ImageRef {
    /// Décompose `[hôte/]chemin[:étiquette]`.
    ///
    /// Renvoie `None` pour une référence par empreinte (`@sha256:…`) : elle est
    /// figée par construction, il n'y a rien à vérifier.
    pub fn parse(reference: &str) -> Option<Self> {
        let reference = reference.trim();
        if reference.is_empty() || reference.contains('@') {
            return None;
        }

        // L'hôte est le premier segment s'il contient un point, un deux-points ou
        // vaut `localhost` : c'est la règle de Docker lui-même.
        let (host, rest) = match reference.split_once('/') {
            Some((first, rest))
                if first.contains('.') || first.contains(':') || first == "localhost" =>
            {
                (Some(first), rest)
            }
            _ => (None, reference),
        };

        // L'étiquette suit le dernier deux-points, à condition qu'il soit après la
        // dernière barre oblique — sinon c'est un port dans l'hôte.
        let (path, tag) = match rest.rsplit_once(':') {
            Some((path, tag)) if !path.contains('/') || !tag.contains('/') => {
                (path, tag.to_string())
            }
            _ => (rest, "latest".to_string()),
        };
        if path.is_empty() || tag.is_empty() {
            return None;
        }

        let (registry, repository) = match host {
            Some("docker.io") | Some("index.docker.io") | None => {
                let repository =
                    if path.contains('/') { path.to_string() } else { format!("library/{path}") };
                ("registry-1.docker.io".to_string(), repository)
            }
            Some(host) => (host.to_string(), path.to_string()),
        };

        Some(Self { registry, repository, tag })
    }

    /// URL du manifeste. Les dépôts locaux (`localhost`, port explicite sans
    /// nom de domaine) sont supposés en clair, comme le fait Docker.
    pub fn manifest_url(&self) -> String {
        let scheme = if self.registry.starts_with("localhost")
            || self.registry.starts_with("127.")
            || self.registry.starts_with("[::1]")
        {
            "http"
        } else {
            "https"
        };
        format!("{scheme}://{}/v2/{}/manifests/{}", self.registry, self.repository, self.tag)
    }
}

/// Vrai si l'empreinte publiée ne correspond à aucune empreinte locale.
///
/// Sans empreinte locale — image construite sur place, ou chargée depuis une
/// archive — on ne peut rien conclure, et l'appelant ne doit pas appeler ceci.
pub fn update_available(local_digests: &[String], remote_digest: &str) -> bool {
    let remote = remote_digest.trim();
    !local_digests.iter().any(|local| {
        let digest = local.rsplit_once('@').map(|(_, d)| d).unwrap_or(local);
        digest.trim() == remote
    })
}

/// Défi `WWW-Authenticate: Bearer realm="…",service="…",scope="…"`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Challenge {
    pub realm: String,
    pub service: Option<String>,
    pub scope: Option<String>,
}

impl Challenge {
    pub fn parse(header: &str) -> Option<Self> {
        let rest = header.trim().strip_prefix("Bearer")?.trim();
        let mut realm = None;
        let mut service = None;
        let mut scope = None;
        for part in rest.split(',') {
            let (key, value) = part.trim().split_once('=')?;
            let value = value.trim().trim_matches('"').to_string();
            match key.trim() {
                "realm" => realm = Some(value),
                "service" => service = Some(value),
                "scope" => scope = Some(value),
                _ => {}
            }
        }
        Some(Self { realm: realm?, service, scope })
    }

    /// URL d'obtention du jeton anonyme.
    pub fn token_url(&self) -> String {
        let mut url = self.realm.clone();
        let mut separator = if url.contains('?') { '&' } else { '?' };
        for (key, value) in [("service", &self.service), ("scope", &self.scope)] {
            if let Some(value) = value {
                url.push(separator);
                url.push_str(key);
                url.push('=');
                url.push_str(&urlencode(value));
                separator = '&';
            }
        }
        url
    }
}

/// Encodage minimal des valeurs de requête : les deux-points et barres obliques
/// des portées (`repository:library/nginx:pull`) passent tels quels chez tous
/// les dépôts, seuls les caractères réellement gênants sont échappés.
fn urlencode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b':' | b'/' => {
                out.push(byte as char);
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// Résultat d'une vérification, tel que le cache le garde.
#[derive(Debug, Clone)]
struct Entry {
    checked_at: Instant,
    /// Empreintes locales au moment de la vérification : si l'image change
    /// entre-temps (mise à jour), le verdict ne vaut plus rien.
    local_digests: Vec<String>,
    /// `None` : vérification impossible (dépôt injoignable, refus, image locale).
    result: Option<bool>,
}

/// Cache des vérifications, partagé entre la tâche de fond et le cycle.
#[derive(Clone, Default)]
pub struct UpdateChecker {
    entries: Arc<Mutex<HashMap<String, Entry>>>,
    /// Vrai tant qu'une tâche de fond tourne : on n'en lance jamais deux.
    busy: Arc<Mutex<bool>>,
}

impl UpdateChecker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Dernier résultat connu pour une référence d'image, à condition qu'il
    /// porte encore sur l'image présente localement.
    pub fn result(&self, image: &str, local_digests: &[String]) -> Option<bool> {
        let entries = self.entries.lock().ok()?;
        let entry = entries.get(image)?;
        (entry.local_digests == local_digests).then_some(entry.result)?
    }

    /// Lance, si nécessaire, une vérification de fond des images dont le résultat
    /// a vieilli. Ne bloque jamais : la collecte lit le cache tel qu'il est.
    pub fn refresh(&self, images: Vec<(String, Vec<String>)>) {
        let due: Vec<(String, Vec<String>)> = {
            let Ok(entries) = self.entries.lock() else { return };
            let mut seen = std::collections::HashSet::new();
            images
                .into_iter()
                .filter(|(image, _)| seen.insert(image.clone()))
                .filter(|(image, digests)| {
                    entries.get(image).is_none_or(|entry| {
                        entry.checked_at.elapsed() >= CHECK_INTERVAL
                            || entry.local_digests != *digests
                    })
                })
                .collect()
        };
        if due.is_empty() {
            return;
        }
        {
            let Ok(mut busy) = self.busy.lock() else { return };
            if *busy {
                return;
            }
            *busy = true;
        }

        let checker = self.clone();
        tokio::spawn(async move {
            let http = reqwest::Client::builder()
                .timeout(HTTP_TIMEOUT)
                .user_agent(concat!("ezymonit-agent/", env!("CARGO_PKG_VERSION")))
                .build();
            let Ok(http) = http else {
                checker.release();
                return;
            };
            for (image, local_digests) in due {
                let result = match check_one(&http, &image, &local_digests).await {
                    Ok(available) => {
                        debug!(
                            image,
                            update_available = available,
                            "image checked against its registry"
                        );
                        Some(available)
                    }
                    Err(error) => {
                        debug!(image, %error, "image update check skipped");
                        None
                    }
                };
                if let Ok(mut entries) = checker.entries.lock() {
                    entries
                        .insert(image, Entry { checked_at: Instant::now(), local_digests, result });
                }
            }
            checker.release();
        });
    }

    fn release(&self) {
        if let Ok(mut busy) = self.busy.lock() {
            *busy = false;
        }
    }
}

/// Une vérification complète : décomposition, requête, éventuel jeton, comparaison.
async fn check_one(http: &reqwest::Client, image: &str, local_digests: &[String]) -> Result<bool> {
    if local_digests.is_empty() {
        bail!("no local digest: image built or loaded locally");
    }
    let reference = ImageRef::parse(image).context("reference pinned by digest or unparsable")?;
    let remote = remote_digest(http, &reference).await?;
    Ok(update_available(local_digests, &remote))
}

/// Empreinte que le dépôt publie pour cette étiquette.
async fn remote_digest(http: &reqwest::Client, reference: &ImageRef) -> Result<String> {
    let url = reference.manifest_url();
    let first = http.head(&url).header("Accept", ACCEPT).send().await.context("registry HEAD")?;

    let mut token = None;
    if first.status() == reqwest::StatusCode::UNAUTHORIZED {
        let challenge = first
            .headers()
            .get("www-authenticate")
            .and_then(|value| value.to_str().ok())
            .and_then(Challenge::parse)
            .context("401 without a Bearer challenge")?;
        token = Some(fetch_token(http, &challenge).await?);
    } else if let Some(digest) = digest_header(&first) {
        return Ok(digest);
    } else if !first.status().is_success() {
        bail!("registry answered {}", first.status());
    }

    // `HEAD` d'abord, `GET` en repli : certains dépôts n'ajoutent l'en-tête
    // d'empreinte qu'à la réponse complète.
    for method in [reqwest::Method::HEAD, reqwest::Method::GET] {
        let mut request = http.request(method, &url).header("Accept", ACCEPT);
        if let Some(token) = &token {
            request = request.bearer_auth(token);
        }
        let response = request.send().await.context("registry manifest request")?;
        if response.status() == reqwest::StatusCode::UNAUTHORIZED
            || response.status() == reqwest::StatusCode::FORBIDDEN
        {
            bail!("registry refused anonymous access ({})", response.status());
        }
        if !response.status().is_success() {
            bail!("registry answered {}", response.status());
        }
        if let Some(digest) = digest_header(&response) {
            return Ok(digest);
        }
    }
    bail!("registry did not return a manifest digest")
}

fn digest_header(response: &reqwest::Response) -> Option<String> {
    response
        .headers()
        .get("docker-content-digest")
        .and_then(|value| value.to_str().ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// Jeton anonyme de lecture, selon le défi du dépôt.
async fn fetch_token(http: &reqwest::Client, challenge: &Challenge) -> Result<String> {
    #[derive(serde::Deserialize)]
    struct TokenResponse {
        #[serde(default)]
        token: Option<String>,
        #[serde(default)]
        access_token: Option<String>,
    }
    let response = http.get(challenge.token_url()).send().await.context("token request")?;
    if !response.status().is_success() {
        bail!("token endpoint answered {}", response.status());
    }
    let body: TokenResponse = response.json().await.context("unreadable token response")?;
    body.token.or(body.access_token).filter(|t| !t.is_empty()).context("empty token")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(reference: &str) -> ImageRef {
        ImageRef::parse(reference).unwrap_or_else(|| panic!("unparsable: {reference}"))
    }

    #[test]
    fn docker_hub_official_images_live_under_library() {
        assert_eq!(
            parsed("nginx:1.25-alpine"),
            ImageRef {
                registry: "registry-1.docker.io".into(),
                repository: "library/nginx".into(),
                tag: "1.25-alpine".into()
            }
        );
        assert_eq!(parsed("postgres:16-alpine").repository, "library/postgres");
    }

    #[test]
    fn docker_hub_user_images_default_to_latest() {
        let reference = parsed("vaultwarden/server");
        assert_eq!(reference.registry, "registry-1.docker.io");
        assert_eq!(reference.repository, "vaultwarden/server");
        assert_eq!(reference.tag, "latest");
        assert_eq!(parsed("docker.io/vaultwarden/server:1.30").repository, "vaultwarden/server");
    }

    #[test]
    fn other_registries_keep_their_host_and_path() {
        assert_eq!(
            parsed("ghcr.io/immich-app/immich-server:release"),
            ImageRef {
                registry: "ghcr.io".into(),
                repository: "immich-app/immich-server".into(),
                tag: "release".into()
            }
        );
        assert_eq!(parsed("code.forgejo.org/forgejo/forgejo:16.0.2").registry, "code.forgejo.org");
        let local = parsed("localhost:5000/x");
        assert_eq!(local.registry, "localhost:5000");
        assert_eq!(local.repository, "x");
        assert_eq!(local.tag, "latest");
        assert!(local.manifest_url().starts_with("http://localhost:5000/v2/x/manifests/latest"));
        let custom = parsed("registry.maison.lan:8443/apps/web:2.1");
        assert_eq!(custom.registry, "registry.maison.lan:8443");
        assert_eq!(custom.repository, "apps/web");
        assert_eq!(custom.tag, "2.1");
        assert_eq!(
            custom.manifest_url(),
            "https://registry.maison.lan:8443/v2/apps/web/manifests/2.1"
        );
    }

    #[test]
    fn digest_references_and_junk_are_skipped() {
        // Une image épinglée par empreinte ne bouge jamais : rien à vérifier.
        assert_eq!(ImageRef::parse("nginx@sha256:abcd"), None);
        assert_eq!(ImageRef::parse(""), None);
        assert_eq!(ImageRef::parse("nginx:"), None);
    }

    #[test]
    fn the_manifest_url_follows_the_registry_api() {
        assert_eq!(
            parsed("nginx:1.25-alpine").manifest_url(),
            "https://registry-1.docker.io/v2/library/nginx/manifests/1.25-alpine"
        );
    }

    #[test]
    fn an_update_is_available_when_no_local_digest_matches() {
        let local = vec!["vaultwarden/server@sha256:094b".to_string()];
        assert!(!update_available(&local, "sha256:094b"));
        assert!(update_available(&local, "sha256:1587"));
        // Plusieurs empreintes locales (image tirée sous deux noms) : une seule
        // correspondance suffit.
        let several = vec!["a@sha256:1".to_string(), "b@sha256:2".to_string()];
        assert!(!update_available(&several, "sha256:2"));
        assert!(update_available(&[], "sha256:2"));
    }

    #[test]
    fn the_bearer_challenge_is_parsed_into_a_token_url() {
        let challenge = Challenge::parse(
            r#"Bearer realm="https://auth.docker.io/token",service="registry.docker.io",scope="repository:library/nginx:pull""#,
        )
        .expect("défi");
        assert_eq!(challenge.realm, "https://auth.docker.io/token");
        assert_eq!(challenge.service.as_deref(), Some("registry.docker.io"));
        assert_eq!(
            challenge.token_url(),
            "https://auth.docker.io/token?service=registry.docker.io&scope=repository:library/nginx:pull"
        );
    }

    #[test]
    fn a_challenge_without_service_still_yields_a_url() {
        let challenge =
            Challenge::parse(r#"Bearer realm="https://ghcr.io/token",scope="repository:a/b:pull""#)
                .expect("défi");
        assert_eq!(challenge.token_url(), "https://ghcr.io/token?scope=repository:a/b:pull");
        assert_eq!(Challenge::parse("Basic realm=\"x\""), None);
        assert_eq!(Challenge::parse("Bearer service=\"x\""), None, "realm is required");
    }

    #[test]
    fn scope_values_with_spaces_are_encoded() {
        assert_eq!(urlencode("repository:a/b:pull,push c"), "repository:a/b:pull%2Cpush%20c");
    }

    #[tokio::test]
    async fn the_cache_starts_empty_and_never_blocks() {
        let checker = UpdateChecker::new();
        assert_eq!(checker.result("nginx:latest", &[]), None);
        // Sans empreinte locale, la tâche conclut « inconnu » sans réseau.
        checker.refresh(vec![("locally-built:dev".to_string(), Vec::new())]);
        for _ in 0..50 {
            tokio::time::sleep(Duration::from_millis(20)).await;
            if checker.entries.lock().unwrap().contains_key("locally-built:dev") {
                break;
            }
        }
        assert!(checker.entries.lock().unwrap().contains_key("locally-built:dev"));
        assert_eq!(checker.result("locally-built:dev", &[]), None);
    }

    #[test]
    fn a_verdict_is_forgotten_once_the_local_image_changes() {
        let checker = UpdateChecker::new();
        let before = vec!["sha256:old".to_string()];
        checker.entries.lock().unwrap().insert(
            "nginx:1.27-alpine".to_string(),
            Entry { checked_at: Instant::now(), local_digests: before.clone(), result: Some(true) },
        );
        assert_eq!(checker.result("nginx:1.27-alpine", &before), Some(true));
        // Après `container.update`, l'image locale est la nouvelle : le vieux
        // « mise à jour disponible » ne doit plus être publié.
        let after = vec!["sha256:new".to_string()];
        assert_eq!(checker.result("nginx:1.27-alpine", &after), None);
    }
}
