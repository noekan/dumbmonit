//! Service de l'interface web, embarquée dans le binaire.
//!
//! Le build SvelteKit est incorporé à la compilation : l'image finale reste un
//! fichier unique, sans répertoire de ressources à monter ni serveur Node.

use axum::body::Body;
use axum::extract::Request;
use axum::http::{HeaderValue, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use rust_embed::Embed;

#[derive(Embed)]
#[folder = "../../web/build/"]
struct Assets;

/// Sert un fichier du build, avec repli sur `index.html`.
///
/// Le repli est indispensable : l'interface est une application monopage dont les
/// routes (`/targets/42`) n'existent pas sur le disque. Sans lui, un rechargement
/// de page ou un lien partagé renverrait 404.
pub async fn serve(request: Request<Body>) -> Response {
    let path = request.uri().path().trim_start_matches('/');

    if let Some(response) = respond_with(path) {
        return response;
    }

    // Une ressource absente sous `/_app/` est une erreur de construction, pas une
    // route applicative : renvoyer `index.html` masquerait le problème derrière une
    // page blanche et un message d'erreur JavaScript incompréhensible.
    if path.starts_with("_app/") {
        return (StatusCode::NOT_FOUND, "Resource not found.").into_response();
    }

    respond_with("index.html").unwrap_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            "Web interface missing from this image: build `web/` before the binary.",
        )
            .into_response()
    })
}

fn respond_with(path: &str) -> Option<Response> {
    let path = if path.is_empty() { "index.html" } else { path };
    let asset = Assets::get(path)?;

    let mime = mime_guess::from_path(path).first_or_octet_stream();
    let mut response = Response::builder()
        .header(header::CONTENT_TYPE, HeaderValue::from_str(mime.as_ref()).ok()?);

    // Les ressources de `_app/` portent une empreinte dans leur nom : elles peuvent
    // être mises en cache indéfiniment. `index.html` ne doit jamais l'être, sinon
    // une mise à jour du produit resterait invisible.
    let cache =
        if path.starts_with("_app/") { "public, max-age=31536000, immutable" } else { "no-cache" };
    response = response.header(header::CACHE_CONTROL, cache);

    response.body(Body::from(asset.data.into_owned())).ok()
}

/// Répond aux requêtes d'API inconnues.
///
/// Sans cela, une faute de frappe dans une URL d'API renverrait la page HTML de
/// l'interface, et le client recevrait du HTML là où il attend du JSON.
pub async fn api_not_found(uri: Uri) -> Response {
    (
        StatusCode::NOT_FOUND,
        axum::Json(serde_json::json!({
            "error": format!("Unknown API route: {}", uri.path())
        })),
    )
        .into_response()
}
