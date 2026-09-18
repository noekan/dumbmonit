//! Distribution de l'agent : les empreintes SHA-256 servies à côté des binaires,
//! que les scripts d'installation vérifient après téléchargement.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::setup_with;
use http_body_util::BodyExt;
use sha2::{Digest, Sha256};
use tower::ServiceExt;

async fn fetch(app: &common::TestApp, uri: &str) -> (StatusCode, String) {
    let request = Request::builder().uri(uri).body(Body::empty()).unwrap();
    let response = app.router.clone().oneshot(request).await.expect("réponse");
    let status = response.status();
    let bytes = response.into_body().collect().await.expect("corps").to_bytes();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

#[tokio::test]
async fn the_checksum_of_a_shipped_binary_is_served_in_sha256sum_format() {
    let agents = tempfile::tempdir().expect("répertoire des agents");
    let binary = b"un faux binaire d'agent";
    std::fs::write(agents.path().join("dumbmonit-agent-linux-x86_64"), binary).unwrap();
    let app = setup_with(|config| config.agent_dir = agents.path().to_path_buf()).await;

    let (status, body) = fetch(&app, "/download/dumbmonit-agent-linux-x86_64.sha256").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let expected = hex::encode(Sha256::digest(binary));
    assert_eq!(body, format!("{expected}  dumbmonit-agent-linux-x86_64\n"));

    // Le binaire lui-même est toujours servi, sans que la route de l'empreinte
    // n'ait rien changé.
    let (status, served) = fetch(&app, "/download/dumbmonit-agent-linux-x86_64").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(served.as_bytes(), binary);
}

#[tokio::test]
async fn a_missing_binary_or_an_unknown_name_has_no_checksum() {
    let agents = tempfile::tempdir().expect("répertoire des agents");
    let app = setup_with(|config| config.agent_dir = agents.path().to_path_buf()).await;

    // Image sans binaires : 404 net, comme pour le binaire lui-même.
    let (status, _) = fetch(&app, "/download/dumbmonit-agent-linux-aarch64.sha256").await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // La liste des fichiers reste fermée : l'empreinte d'un fichier arbitraire
    // du répertoire n'est pas calculable.
    std::fs::write(agents.path().join("secret.key"), b"pas pour toi").unwrap();
    for uri in ["/download/secret.key.sha256", "/download/..%2Fsecret.key.sha256"] {
        let (status, _) = fetch(&app, uri).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{uri}");
    }
}
