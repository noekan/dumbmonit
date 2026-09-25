//! Logo d'une page de statut : dépôt par l'administrateur, lecture publique.
//!
//! Le fichier vit sous `<data>/status-pages/<id>.logo`, son type en base. Seuls
//! PNG, JPEG et WebP sont admis, reconnus à leurs premiers octets et non à ce
//! que le client prétend : pas de SVG, qui peut porter du script et serait
//! servi depuis la même origine que l'administration.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use base64::Engine;
use serde::Deserialize;

use super::{invalidate_cache, page_not_found, public_not_found};
use crate::api::{ApiError, ApiResult};
use crate::db;
use crate::state::AppState;

/// Taille maximale d'un logo : de quoi tenir une image nette de 512 px.
pub const MAX_LOGO_BYTES: usize = 256 * 1024;

#[derive(Debug, Deserialize)]
pub struct LogoPayload {
    /// Contenu du fichier en base64 (standard, avec ou sans préfixe `data:`).
    pub data: String,
}

fn logo_path(state: &AppState, id: i64) -> std::path::PathBuf {
    state.config.data_dir.join("status-pages").join(format!("{id}.logo"))
}

/// Type d'image reconnu à sa signature, ou `None`.
pub fn sniff(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

fn decode(raw: &str) -> ApiResult<Vec<u8>> {
    // `data:image/png;base64,…` tel que le produit un FileReader.
    let raw = raw.trim();
    let encoded = match raw.split_once(',') {
        Some((prefix, rest)) if prefix.starts_with("data:") => rest,
        _ => raw,
    };
    if encoded.len() > MAX_LOGO_BYTES * 4 / 3 + 8 {
        return Err(too_large());
    }
    base64::engine::general_purpose::STANDARD
        .decode(encoded.trim())
        .map_err(|_| ApiError::BadRequest("The logo could not be read (invalid base64).".into()))
}

fn too_large() -> ApiError {
    ApiError::BadRequest(format!("The logo is limited to {} KiB.", MAX_LOGO_BYTES / 1024))
}

pub async fn upload_logo(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(payload): Json<LogoPayload>,
) -> ApiResult<StatusCode> {
    db::status_pages::get_page(&state.pool, id).await?.ok_or_else(|| page_not_found(id))?;
    let bytes = decode(&payload.data)?;
    if bytes.is_empty() {
        return Err(ApiError::BadRequest("The logo file is empty.".into()));
    }
    if bytes.len() > MAX_LOGO_BYTES {
        return Err(too_large());
    }
    let Some(kind) = sniff(&bytes) else {
        return Err(ApiError::BadRequest("The logo must be a PNG, JPEG or WebP image.".into()));
    };
    let path = logo_path(&state, id);
    if let Some(dir) = path.parent() {
        tokio::fs::create_dir_all(dir).await.map_err(anyhow::Error::from)?;
    }
    // Écriture puis renommage : un lecteur ne voit jamais un fichier à moitié écrit.
    let staging = path.with_extension("logo.tmp");
    tokio::fs::write(&staging, &bytes).await.map_err(anyhow::Error::from)?;
    tokio::fs::rename(&staging, &path).await.map_err(anyhow::Error::from)?;
    db::status_pages::set_logo_type(&state.pool, id, Some(kind)).await?;
    invalidate_cache();
    tracing::info!(page = id, kind, size = bytes.len(), "status page logo stored");
    Ok(StatusCode::NO_CONTENT)
}

pub async fn delete_logo(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> ApiResult<StatusCode> {
    db::status_pages::get_page(&state.pool, id).await?.ok_or_else(|| page_not_found(id))?;
    remove_logo_file(&state, id).await;
    db::status_pages::set_logo_type(&state.pool, id, None).await?;
    invalidate_cache();
    Ok(StatusCode::NO_CONTENT)
}

/// Efface le fichier du logo, s'il existe. Une erreur est journalisée : la page
/// est déjà supprimée ou sans logo, et le fichier orphelin ne se sert plus.
pub async fn remove_logo_file(state: &AppState, id: i64) {
    match tokio::fs::remove_file(logo_path(state, id)).await {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => tracing::warn!(%error, page = id, "status page logo not removed"),
    }
}

/// Le logo avec les en-têtes qui empêchent de l'interpréter autrement qu'en image.
async fn serve(state: &AppState, id: i64, kind: &str, cache: &'static str) -> ApiResult<Response> {
    let bytes = match tokio::fs::read(logo_path(state, id)).await {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(ApiError::NotFound("This page has no logo.".into()));
        }
        Err(error) => return Err(ApiError::Internal(error.into())),
    };
    // Le type annoncé est celui de la signature relue, jamais celui en base seul.
    let kind = match sniff(&bytes) {
        Some(sniffed) if sniffed == kind => sniffed,
        _ => return Err(ApiError::NotFound("This page has no logo.".into())),
    };
    Ok((
        [
            (header::CONTENT_TYPE, kind),
            (header::CACHE_CONTROL, cache),
            (header::CONTENT_DISPOSITION, "inline; filename=\"logo\""),
        ],
        bytes,
    )
        .into_response())
}

/// Logo public : même règle que le document, une page non publiée n'existe pas.
pub async fn public_logo(
    State(state): State<AppState>,
    Path(slug): Path<String>,
) -> ApiResult<Response> {
    let page = match db::status_pages::get_page_by_slug(&state.pool, &slug).await? {
        Some(page) if page.published => page,
        _ => return Err(public_not_found()),
    };
    let Some(kind) = page.logo_type.as_deref() else {
        return Err(ApiError::NotFound("This page has no logo.".into()));
    };
    // L'adresse porte la version (`?v=`) : le logo peut rester en cache un jour.
    serve(&state, page.id, kind, "public, max-age=86400").await
}

/// Aperçu pour l'éditeur, y compris pour une page encore en brouillon.
pub async fn admin_logo(State(state): State<AppState>, Path(id): Path<i64>) -> ApiResult<Response> {
    let page =
        db::status_pages::get_page(&state.pool, id).await?.ok_or_else(|| page_not_found(id))?;
    let Some(kind) = page.logo_type.as_deref() else {
        return Err(ApiError::NotFound("This page has no logo.".into()));
    };
    serve(&state, id, kind, "private, no-cache").await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_raster_images_are_recognised() {
        assert_eq!(sniff(b"\x89PNG\r\n\x1a\n0000"), Some("image/png"));
        assert_eq!(sniff(&[0xFF, 0xD8, 0xFF, 0xE0]), Some("image/jpeg"));
        assert_eq!(sniff(b"RIFF\0\0\0\0WEBPVP8 "), Some("image/webp"));
        assert_eq!(sniff(b"<svg xmlns=\"http://www.w3.org/2000/svg\"><script/></svg>"), None);
        assert_eq!(sniff(b"GIF89a"), None);
        assert_eq!(sniff(b""), None);
    }

    #[test]
    fn a_data_url_is_accepted() {
        let Ok(bytes) = decode("data:image/png;base64,iVBORw0KGgo=") else { panic!("decoded") };
        assert_eq!(sniff(&bytes), Some("image/png"));
    }
}
