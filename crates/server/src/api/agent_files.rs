//! Distribution de l'agent : scripts d'installation et binaires.
//!
//! La commande affichée à la création d'un jeton — `curl …/install.sh | sh` —
//! suppose que ce serveur sait livrer l'agent lui-même. Les scripts sont
//! embarqués dans le binaire ; les exécutables, trop lourds pour cela, sont lus
//! dans `Config::agent_dir`, rempli à la construction de l'image.
//!
//! Ces routes sont publiques : la machine qui s'installe n'a pas de session, et
//! rien ici n'est secret — le jeton voyage dans les arguments, jamais dans
//! l'URL du script.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{LazyLock, Mutex};

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use sha2::{Digest, Sha256};
use tokio_util::io::ReaderStream;

use crate::state::AppState;

const INSTALL_SH: &str = include_str!("../../../agent/install/install.sh");
const INSTALL_PS1: &str = include_str!("../../../agent/install/install.ps1");

pub async fn install_sh() -> Response {
    script(INSTALL_SH, "text/x-shellscript; charset=utf-8")
}

pub async fn install_ps1() -> Response {
    script(INSTALL_PS1, "text/plain; charset=utf-8")
}

fn script(body: &'static str, content_type: &'static str) -> Response {
    ([(header::CONTENT_TYPE, content_type), (header::CACHE_CONTROL, "no-cache")], body)
        .into_response()
}

/// Noms de fichiers que les scripts d'installation demandent.
///
/// Liste fermée : c'est ce qui empêche `/download/../secret.key` ou toute autre
/// fantaisie de sortir du répertoire, sans avoir à normaliser un chemin.
pub const AGENT_FILES: &[&str] = &[
    "dumbmonit-agent-linux-x86_64",
    "dumbmonit-agent-linux-aarch64",
    "dumbmonit-agent-windows-x86_64.exe",
];

/// Suffixe sous lequel l'empreinte d'un binaire est servie : `/download/<nom>.sha256`.
const CHECKSUM_SUFFIX: &str = ".sha256";

/// Empreintes déjà calculées, par chemin. Les binaires sont figés dans l'image :
/// une empreinte ne change pas pendant la vie du processus, et la recalculer à
/// chaque installation relirait dix mégaoctets pour rien.
static CHECKSUMS: LazyLock<Mutex<HashMap<PathBuf, String>>> = LazyLock::new(Mutex::default);

/// `GET /download/{name}` — le binaire, ou son empreinte SHA-256 quand `name` se
/// termine par `.sha256`.
///
/// Les scripts d'installation vérifient le binaire téléchargé contre cette
/// empreinte, et l'interface l'affiche à côté de la commande d'installation : un
/// binaire remplacé en chemin (HTTP en clair, mandataire) ne s'installe pas.
pub async fn download(State(state): State<AppState>, Path(name): Path<String>) -> Response {
    if let Some(binary) = name.strip_suffix(CHECKSUM_SUFFIX) {
        return checksum(&state, binary).await;
    }
    if !AGENT_FILES.contains(&name.as_str()) {
        return (StatusCode::NOT_FOUND, "Unknown file.").into_response();
    }

    let path = state.config.agent_dir.join(&name);
    let file = match tokio::fs::File::open(&path).await {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            tracing::warn!(path = %path.display(), "agent binary missing from this image");
            return (
                StatusCode::NOT_FOUND,
                "This image does not ship the agent binaries. Install the agent with --bin=PATH.",
            )
                .into_response();
        }
        Err(error) => {
            tracing::error!(path = %path.display(), %error, "failed to read agent binary");
            return (StatusCode::INTERNAL_SERVER_ERROR, "Internal server error.").into_response();
        }
    };

    let mut response = Body::from_stream(ReaderStream::new(file)).into_response();
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, "application/octet-stream".parse().unwrap());
    if let Ok(value) = format!("attachment; filename=\"{name}\"").parse() {
        headers.insert(header::CONTENT_DISPOSITION, value);
    }
    response
}

/// Empreinte au format de `sha256sum` (`<hex>  <nom>`), pour que le script
/// d'installation puisse la passer telle quelle à `sha256sum -c`.
async fn checksum(state: &AppState, name: &str) -> Response {
    if !AGENT_FILES.contains(&name) {
        return (StatusCode::NOT_FOUND, "Unknown file.").into_response();
    }
    let path = state.config.agent_dir.join(name);
    let cached = CHECKSUMS.lock().expect("cache des empreintes").get(&path).cloned();
    let digest = match cached {
        Some(digest) => digest,
        None => match tokio::fs::read(&path).await {
            Ok(bytes) => {
                let digest = hex::encode(Sha256::digest(&bytes));
                CHECKSUMS.lock().expect("cache des empreintes").insert(path, digest.clone());
                digest
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return (StatusCode::NOT_FOUND, "This image does not ship the agent binaries.")
                    .into_response();
            }
            Err(error) => {
                tracing::error!(path = %path.display(), %error, "failed to hash agent binary");
                return (StatusCode::INTERNAL_SERVER_ERROR, "Internal server error.")
                    .into_response();
            }
        },
    };
    (
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8"), (header::CACHE_CONTROL, "no-cache")],
        format!("{digest}  {name}\n"),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn les_scripts_embarques_demandent_les_fichiers_que_le_serveur_sait_servir() {
        // Un renommage d'un côté sans l'autre ne casserait rien à la compilation,
        // seulement l'installation chez l'utilisateur, en 404.
        assert!(INSTALL_SH.contains("/download/dumbmonit-agent-linux-$ARCH"));
        assert!(INSTALL_PS1.contains("/download/dumbmonit-agent-windows-$architecture.exe"));
        for arch in ["x86_64", "aarch64"] {
            assert!(AGENT_FILES.contains(&format!("dumbmonit-agent-linux-{arch}").as_str()));
        }
        // Et ils vérifient ce qu'ils ont téléchargé contre l'empreinte servie à côté.
        assert!(INSTALL_SH.contains("$source_url.sha256"));
        assert!(INSTALL_SH.contains("sha256sum"));
        assert!(INSTALL_PS1.contains(".sha256"));
        assert!(INSTALL_PS1.contains("Get-FileHash"));
    }
}
