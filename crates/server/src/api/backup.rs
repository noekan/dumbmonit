//! Sauvegarde et restauration depuis l'interface.
//!
//! Quatre routes, toutes réservées à un administrateur **connecté** : un lot
//! contient en clair — pour qui a la phrase de passe — tous les identifiants du
//! parc. Un jeton d'API, même en écriture, ne l'obtient pas : un jeton vit dans
//! un script, et un script n'a pas à pouvoir exfiltrer l'instance entière.
//!
//! L'export est un `POST` et non un `GET` alors qu'il ne change rien : la phrase
//! de passe est dans le corps de la requête. Dans une URL, elle finirait dans
//! l'historique du navigateur, dans les journaux du mandataire inverse et dans
//! l'en-tête `Referer` de la page suivante.

use axum::Json;
use axum::extract::State;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Router};
use serde::{Deserialize, Serialize};

use crate::api::{ApiError, ApiResult};
use crate::auth::audit;
use crate::auth::client_ip::ClientIp;
use crate::auth::middleware::{CurrentPrincipal, Principal};
use crate::auth::users::User;
use crate::backup::{bundle, export, import, local};
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/backup", get(status).post(create))
        .route("/backup/restore", post(restore))
        .route("/backup/local", post(run_local))
}

/// Un lot n'est délivré qu'à un administrateur connecté dans l'interface.
fn admin(principal: &Principal) -> ApiResult<&User> {
    match principal {
        Principal::User(user) if user.role.is_admin() => Ok(user),
        Principal::User(_) => {
            Err(ApiError::Forbidden("Only an administrator can export or restore a backup.".into()))
        }
        Principal::Token(_) => Err(ApiError::Forbidden(
            "A backup holds every credential of this instance, so it is only ever handed to an \
             administrator signed in to the web interface — never to an API token. Sign in and \
             use Settings → Backup."
                .into(),
        )),
    }
}

// --------------------------------------------------------------------------
// GET /api/backup — ce qu'il y a à sauvegarder et ce qui l'a été
// --------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct Section {
    pub section: String,
    pub description: String,
    pub count: usize,
}

#[derive(Debug, Serialize)]
pub struct BackupStatus {
    /// Version de format écrite par ce serveur.
    pub bundle_version: u32,
    /// Longueur minimale exigée de la phrase de passe.
    pub min_passphrase_length: usize,
    /// Contenu du lot qui serait produit maintenant.
    pub contents: Vec<Section>,
    /// `file` quand le secret d'instance vit dans `secret.key`, `environment`
    /// quand il vient de `DUMBMONIT_SECRET`.
    pub secret_source: String,
    pub secret_path: String,
    pub schedule: local::Status,
}

pub async fn status(
    State(state): State<AppState>,
    Extension(CurrentPrincipal(principal)): Extension<CurrentPrincipal>,
) -> ApiResult<Json<BackupStatus>> {
    admin(&principal)?;
    // Le lot est réellement construit : les décomptes annoncés sont ceux qui
    // partiront, et non une estimation qui se trouverait fausse le jour où une
    // section cesse d'être exportée.
    let collected =
        export::collect(&state.pool, &state.cipher, export::ExportOptions::default()).await?;
    let counts = collected.summary();
    let settings = local::Settings::from_config(&state.config);

    Ok(Json(BackupStatus {
        bundle_version: bundle::VERSION,
        min_passphrase_length: bundle::MIN_PASSPHRASE_LEN,
        contents: export::sections()
            .into_iter()
            .map(|(section, description)| Section {
                count: counts.get(section).copied().unwrap_or(0),
                section: section.to_string(),
                description: description.to_string(),
            })
            .collect(),
        secret_source: if state.config.secret.is_some() { "environment" } else { "file" }
            .to_string(),
        secret_path: state.config.secret_path().display().to_string(),
        schedule: local::status(&state.pool, &settings).await?,
    }))
}

// --------------------------------------------------------------------------
// POST /api/backup — produire le lot
// --------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct ExportPayload {
    pub passphrase: String,
    /// Emporter aussi les empreintes de mots de passe, les secrets TOTP et les
    /// codes de secours des comptes. Faux par défaut.
    #[serde(default)]
    pub include_account_secrets: bool,
}

pub async fn create(
    State(state): State<AppState>,
    Extension(CurrentPrincipal(principal)): Extension<CurrentPrincipal>,
    ClientIp(ip): ClientIp,
    Json(payload): Json<ExportPayload>,
) -> ApiResult<Response> {
    let me = admin(&principal)?.username.clone();
    bundle::check_passphrase(&payload.passphrase)
        .map_err(|e| ApiError::BadRequest(e.to_string()))?;

    let options =
        export::ExportOptions { include_account_secrets: payload.include_account_secrets };
    let collected = export::collect(&state.pool, &state.cipher, options).await?;
    let envelope = bundle::seal(&collected, &payload.passphrase)?;
    let body = serde_json::to_vec_pretty(&envelope)?;

    let name = format!("dumbmonit-backup-{}.json", chrono::Utc::now().format("%Y%m%d-%H%M%S"));
    tracing::info!(actor = %me, accounts = payload.include_account_secrets, "backup exported");
    audit::record(&state.pool, Some(&me), "backup.exported", Some(&name), ip).await;

    Ok((
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/json".to_string()),
            (header::CONTENT_DISPOSITION, format!("attachment; filename=\"{name}\"")),
            // Un lot n'a rien à faire dans un cache, ni du navigateur ni d'un
            // mandataire.
            (header::CACHE_CONTROL, "no-store".to_string()),
        ],
        body,
    )
        .into_response())
}

// --------------------------------------------------------------------------
// POST /api/backup/restore — simuler, puis appliquer
// --------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct RestorePayload {
    /// L'enveloppe telle qu'elle est dans le fichier.
    pub bundle: serde_json::Value,
    pub passphrase: String,
    /// Faux — le défaut — : simulation. Le compte rendu dit ce qui se passerait,
    /// rien n'est écrit.
    #[serde(default)]
    pub apply: bool,
}

pub async fn restore(
    State(state): State<AppState>,
    Extension(CurrentPrincipal(principal)): Extension<CurrentPrincipal>,
    ClientIp(ip): ClientIp,
    Json(payload): Json<RestorePayload>,
) -> ApiResult<Json<import::RestoreReport>> {
    let me = admin(&principal)?.username.clone();

    let envelope: bundle::Envelope = serde_json::from_value(payload.bundle).map_err(|_| {
        ApiError::BadRequest(
            "This file is not a DumbMonit backup: its header could not be read.".into(),
        )
    })?;
    // Format, version et algorithme d'abord : inutile de faire saisir une
    // phrase de passe pour un fichier que nous refuserons de toute façon.
    bundle::check_envelope(&envelope).map_err(|e| ApiError::BadRequest(e.to_string()))?;

    let opened = bundle::open(&envelope, &payload.passphrase)
        .map_err(|e| ApiError::BadRequest(e.to_string()))?;

    let report = import::restore(&state.pool, &state.cipher, &opened, payload.apply).await?;

    if payload.apply {
        tracing::info!(
            actor = %me,
            created = report.created,
            updated = report.updated,
            skipped = report.skipped,
            "backup restored"
        );
        audit::record(
            &state.pool,
            Some(&me),
            "backup.restored",
            Some(&format!("{} created, {} updated", report.created, report.updated)),
            ip,
        )
        .await;
    }
    Ok(Json(report))
}

// --------------------------------------------------------------------------
// POST /api/backup/local — écrire une sauvegarde locale tout de suite
// --------------------------------------------------------------------------

pub async fn run_local(
    State(state): State<AppState>,
    Extension(CurrentPrincipal(principal)): Extension<CurrentPrincipal>,
    ClientIp(ip): ClientIp,
) -> ApiResult<Json<local::Status>> {
    let me = admin(&principal)?.username.clone();
    let settings = local::Settings::from_config(&state.config);
    let record = local::run_once(&state.pool, &settings).await;
    local::publish(&state).await;
    audit::record(&state.pool, Some(&me), "backup.local_run", Some(&record.file), ip).await;
    Ok(Json(local::status(&state.pool, &settings).await?))
}
