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
    "dumbmonit-agent-freebsd-x86_64",
    "dumbmonit-agent-windows-x86_64.exe",
];

/// Binaires que cette image ne peut pas contenir.
///
/// Compiler pour macOS exige le SDK d'Apple, que sa licence interdit de
/// redistribuer : l'image ne peut donc pas l'embarquer, et aucune astuce ne
/// changera cela. Les binaires macOS sont construits sur un exécuteur macOS à
/// chaque version et attachés à la publication.
///
/// Ils restent listés ici pour une seule raison : un 404 nu, sur une machine
/// qu'on est en train d'installer, ne dit pas s'il faut corriger l'URL, le nom
/// ou l'architecture. Celui-là dit exactement où aller chercher.
pub const AGENT_FILES_RELEASED_ELSEWHERE: &[&str] =
    &["dumbmonit-agent-macos-aarch64", "dumbmonit-agent-macos-x86_64"];

/// Où trouver les binaires que l'image ne livre pas.
pub const RELEASES_URL: &str = "https://github.com/noekan/dumbmonit/releases/latest";

/// Ce que le serveur répond pour un binaire qu'il ne peut pas livrer.
///
/// Écrit pour être lu sur la machine qu'on est en train d'installer : le script
/// d'installation récupère ce corps et l'affiche avant d'abandonner.
fn released_elsewhere_message(name: &str) -> String {
    format!(
        "{name} is not shipped in the DumbMonit image: building the agent for macOS \
         requires Apple's SDK, which cannot be redistributed.\n\
         Download it from {RELEASES_URL} and install it with:\n\
         \x20 sudo ./install.sh --token=... --url=... --bin=./{name}\n"
    )
}

/// Réponse donnée pour un binaire qui n'est, par nature, jamais dans l'image.
fn released_elsewhere(name: &str) -> Response {
    (StatusCode::NOT_FOUND, released_elsewhere_message(name)).into_response()
}

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
    if AGENT_FILES_RELEASED_ELSEWHERE.contains(&name.as_str()) {
        return released_elsewhere(&name);
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
    if AGENT_FILES_RELEASED_ELSEWHERE.contains(&name) {
        return released_elsewhere(name);
    }
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
        assert!(INSTALL_SH.contains("/download/dumbmonit-agent-$PLATFORM-$ARCH"));
        assert!(INSTALL_PS1.contains("/download/dumbmonit-agent-windows-$architecture.exe"));
        for arch in ["x86_64", "aarch64"] {
            assert!(AGENT_FILES.contains(&format!("dumbmonit-agent-linux-{arch}").as_str()));
        }
        // Les noms que le script compose à partir de `uname` doivent tous être
        // connus d'un côté ou de l'autre : servis par l'image, ou publiés avec
        // la version. Un nom qu'aucune des deux listes ne connaît est un 404
        // que personne ne saura expliquer.
        // La matrice exacte que `install.sh` accepte : tout ce qu'il refuse, il
        // le refuse avec une phrase, avant le moindre téléchargement.
        for name in [
            "dumbmonit-agent-linux-x86_64",
            "dumbmonit-agent-linux-aarch64",
            "dumbmonit-agent-freebsd-x86_64",
            "dumbmonit-agent-macos-x86_64",
            "dumbmonit-agent-macos-aarch64",
        ] {
            assert!(
                AGENT_FILES.contains(&name) || AGENT_FILES_RELEASED_ELSEWHERE.contains(&name),
                "{name} is composed by install.sh but known nowhere"
            );
        }
        // Et ils vérifient ce qu'ils ont téléchargé contre l'empreinte servie à côté.
        assert!(INSTALL_SH.contains("$source_url.sha256"));
        assert!(INSTALL_SH.contains("sha256sum"));
        assert!(INSTALL_PS1.contains(".sha256"));
        assert!(INSTALL_PS1.contains("Get-FileHash"));
    }

    #[test]
    fn les_binaires_publies_ailleurs_ne_sont_jamais_servis_par_l_image() {
        // Les deux listes ne doivent pas se recouvrir : un nom présent dans les
        // deux serait servi ou expliqué selon l'ordre du code, ce qui est
        // exactement le genre de détail dont personne ne se souvient.
        for name in AGENT_FILES_RELEASED_ELSEWHERE {
            assert!(!AGENT_FILES.contains(name), "{name} is in both lists");
        }
    }

    #[test]
    fn un_binaire_absent_par_nature_dit_ou_le_trouver() {
        let name = "dumbmonit-agent-macos-aarch64";
        assert_eq!(released_elsewhere(name).status(), StatusCode::NOT_FOUND);
        // Le corps est la seule chose que verra quelqu'un en train d'installer
        // un agent : il doit porter la raison, l'adresse et la commande —
        // pas seulement un regret.
        let message = released_elsewhere_message(name);
        assert!(message.contains(name));
        assert!(message.contains(RELEASES_URL));
        assert!(message.contains("--bin="));
        assert!(message.contains("SDK"));
    }

    #[test]
    fn le_script_dinstallation_sait_reconnaitre_les_quatre_systemes() {
        for marker in ["systemd", "openrc", "launchd", "rcd"] {
            assert!(INSTALL_SH.contains(marker), "install.sh ignores {marker}");
        }
        // Et il sait nommer le binaire de chaque plateforme.
        for platform in ["linux", "freebsd", "macos"] {
            assert!(INSTALL_SH.contains(platform), "install.sh never mentions {platform}");
        }
    }
}
