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

/// Nonce de la politique de contenu, posé par `security_headers` sur la requête.
///
/// Le build de l'interface contient deux scripts en ligne : le choix du thème
/// avant le premier rendu, et l'amorce de SvelteKit. Leur contenu change à
/// chaque construction, donc leur empreinte aussi : un nonce par réponse est ce
/// qui permet de les autoriser sans ouvrir `'unsafe-inline'` à tout le monde.
#[derive(Clone)]
pub struct Nonce(pub String);

/// Sert un fichier du build, avec repli sur `index.html`.
///
/// Le repli est indispensable : l'interface est une application monopage dont les
/// routes (`/targets/42`) n'existent pas sur le disque. Sans lui, un rechargement
/// de page ou un lien partagé renverrait 404.
pub async fn serve(request: Request<Body>) -> Response {
    let path = request.uri().path().trim_start_matches('/');
    let nonce = request.extensions().get::<Nonce>().map(|nonce| nonce.0.clone());

    if let Some(response) = respond_with(path, nonce.as_deref()) {
        return response;
    }

    // Une ressource absente sous `/_app/` est une erreur de construction, pas une
    // route applicative : renvoyer `index.html` masquerait le problème derrière une
    // page blanche et un message d'erreur JavaScript incompréhensible.
    if path.starts_with("_app/") {
        return (StatusCode::NOT_FOUND, "Resource not found.").into_response();
    }

    respond_with("index.html", nonce.as_deref()).unwrap_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            "Web interface missing from this image: build `web/` before the binary.",
        )
            .into_response()
    })
}

fn respond_with(path: &str, nonce: Option<&str>) -> Option<Response> {
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

    // Seule la page porte des scripts en ligne ; les ressources de `_app/` sont
    // des fichiers servis tels quels, et les toucher invaliderait leur empreinte.
    let body = match (path.ends_with(".html"), nonce) {
        (true, Some(nonce)) => Body::from(with_nonce(&asset.data, nonce)),
        _ => Body::from(asset.data.into_owned()),
    };
    response.body(body).ok()
}

/// Ajoute le nonce à chaque balise `<script>` de la page.
///
/// Une substitution de texte, et non un analyseur HTML : le fichier est le
/// nôtre, produit par notre propre build, et le test ci-dessous vérifie sur le
/// build réel qu'aucun script ne reste sans nonce — sans quoi l'interface
/// s'afficherait blanche, ce qui ne passerait pas inaperçu.
fn with_nonce(html: &[u8], nonce: &str) -> Vec<u8> {
    let Ok(text) = std::str::from_utf8(html) else {
        return html.to_vec();
    };
    text.replace("<script", &format!("<script nonce=\"{nonce}\"")).into_bytes()
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Le build réel est embarqué : s'il change de forme, le test le voit.
    fn index() -> Vec<u8> {
        Assets::get("index.html").expect("interface construite").data.into_owned()
    }

    #[test]
    fn every_inline_script_of_the_page_carries_the_nonce() {
        let page = index();
        let opens = std::str::from_utf8(&page).unwrap().matches("<script").count();
        assert!(opens > 0, "la page doit porter au moins l'amorce de SvelteKit");

        let served = with_nonce(&page, "abc123");
        let text = std::str::from_utf8(&served).expect("utf-8");
        assert_eq!(
            text.matches("<script nonce=\"abc123\"").count(),
            opens,
            "un script sans nonce afficherait une page blanche"
        );
        assert!(!text.contains("<script>"), "il reste une balise sans nonce");
    }

    #[test]
    fn the_page_is_otherwise_untouched() {
        // Rien d'autre que l'attribut ne doit bouger : la page est servie telle
        // que le build l'a produite, empreintes de ressources comprises.
        let page = index();
        let served = with_nonce(&page, "abc123");
        let text = std::str::from_utf8(&served).expect("utf-8");
        assert!(text.contains("/_app/immutable/"), "les ressources doivent rester référencées");
        assert!(text.trim_start().starts_with("<!doctype html>"));
        let stripped = text.replace(" nonce=\"abc123\"", "");
        assert_eq!(stripped.as_bytes(), page.as_slice());
    }
}
