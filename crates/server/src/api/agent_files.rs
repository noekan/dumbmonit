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

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
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

pub async fn download(State(state): State<AppState>, Path(name): Path<String>) -> Response {
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
    }
}
