use axum::Json;
use axum::extract::State;
use serde::Serialize;

use crate::state::AppState;

#[derive(Serialize)]
pub struct Health {
    status: &'static str,
    version: &'static str,
    database: ComponentHealth,
    victoria: ComponentHealth,
}

#[derive(Serialize)]
struct ComponentHealth {
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    /// VictoriaMetrics seulement : `true` quand le serveur le lance lui-même,
    /// `false` quand `DUMBMONIT_VM_URL` désigne une instance externe.
    #[serde(skip_serializing_if = "Option::is_none")]
    embedded: Option<bool>,
}

impl ComponentHealth {
    fn from(result: anyhow::Result<()>) -> Self {
        match result {
            Ok(()) => Self { ok: true, error: None, embedded: None },
            Err(error) => Self { ok: false, error: Some(error.to_string()), embedded: None },
        }
    }

    fn embedded(self, embedded: bool) -> Self {
        Self { embedded: Some(embedded), ..self }
    }
}

/// État de santé du serveur et de ses dépendances.
///
/// Répond toujours 200 : c'est un rapport de diagnostic, pas une sonde de vivacité.
/// Un composant en panne se lit dans le corps de la réponse, ce qui permet à
/// l'interface d'afficher précisément ce qui ne va pas.
pub async fn health(State(state): State<AppState>) -> Json<Health> {
    let database =
        sqlx::query("SELECT 1").execute(&state.pool).await.map(|_| ()).map_err(anyhow::Error::from);

    let victoria = state.victoria.health().await;

    let all_ok = database.is_ok() && victoria.is_ok();

    Json(Health {
        status: if all_ok { "ok" } else { "degraded" },
        version: env!("CARGO_PKG_VERSION"),
        database: ComponentHealth::from(database),
        victoria: ComponentHealth::from(victoria).embedded(state.config.vm_embedded()),
    })
}
