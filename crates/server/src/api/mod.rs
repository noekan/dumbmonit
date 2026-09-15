mod agent_commands;
mod agent_files;
mod alerts;
mod auth;
mod channels;
mod collectors;
mod discovery;
mod error;
mod health;
mod ingest;
mod mcp;
mod metrics;
mod notify_policy;
mod oidc;
mod spa;
mod status_pages;
mod targets;
mod tokens;
mod users;

pub use error::{ApiError, ApiResult};

use axum::extract::DefaultBodyLimit;
use axum::routing::{delete, get, post, put};
use axum::{Extension, Router, middleware};
use tower_http::trace::TraceLayer;

use crate::auth::AuthState;
use crate::state::AppState;

pub fn router(state: AppState) -> Router {
    let auth_state = AuthState::from_env();

    // Tout ce qui touche à l'instance — lecture comprise : la liste des
    // équipements d'un homelab est déjà une information à ne pas laisser traîner.
    //
    // Le garde applique aussi les rôles : POST/PUT/DELETE exigent un
    // administrateur, sauf `logout` et `password` que chacun fait pour soi.
    let protected = Router::new()
        .route("/auth/logout", post(auth::logout))
        .route("/auth/password", post(auth::change_password))
        .route("/auth/me", get(auth::me))
        .route(
            "/auth/oidc/config",
            get(oidc::get_config).put(oidc::put_config).delete(oidc::delete_config),
        )
        .route("/auth/oidc/test", post(oidc::test))
        .route("/users", get(users::list).post(users::create))
        .route("/users/{id}", put(users::update).delete(users::delete))
        .route("/collectors", get(collectors::list))
        .route("/discovery", get(discovery::scan))
        .route("/targets", get(targets::list).post(targets::create))
        .route("/targets/{id}", get(targets::get_one).put(targets::update).delete(targets::delete))
        .route("/targets/{id}/probe", post(targets::probe_now))
        .route("/targets/{id}/discover", post(targets::discover))
        .route("/metrics/query", get(metrics::query))
        .route("/metrics/query_range", get(metrics::query_range))
        .route("/alerts", get(alerts::list_active))
        .route("/alerts/history", get(alerts::history))
        .route("/alerts/rules", get(alerts::list_rules).post(alerts::create_rule))
        .route("/alerts/rules/{id}", put(alerts::update_rule).delete(alerts::delete_rule))
        .route("/alerts/rules/{id}/enable", post(alerts::set_rule_enabled))
        .route("/alerts/silences", get(alerts::list_silences).post(alerts::create_silence))
        .route("/alerts/silences/{id}", delete(alerts::delete_silence))
        .route("/notify/channels", get(channels::list).post(channels::create))
        .route("/notify/channels/{id}", put(channels::update).delete(channels::delete))
        .route("/notify/channels/{id}/test", post(channels::test))
        .route("/notify/kinds", get(channels::kinds))
        .route("/agent/tokens", get(ingest::list_tokens).post(ingest::create_token))
        .route("/agent/tokens/{id}", delete(ingest::revoke_token))
        .route("/tokens", get(tokens::list).post(tokens::create))
        .route("/tokens/{id}", delete(tokens::revoke))
        // Actions sur les conteneurs d'une machine (`agent_commands.rs`).
        .merge(agent_commands::ui_routes())
        // Politique de notification et surcharges par équipement (`notify_policy.rs`).
        .merge(notify_policy::routes())
        // Pages de statut et incidents (`status_pages.rs`).
        .merge(status_pages::routes())
        // `route_layer` plutôt que `layer` : le garde ne s'applique qu'aux routes
        // effectivement déclarées ici, jamais au repli qui sert l'interface.
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            crate::auth::middleware::require_session,
        ));

    // Les seules routes d'API ouvertes. `health` sert au diagnostic et aux sondes
    // d'orchestrateur ; les autres sont ce dont l'interface a besoin pour
    // afficher son écran de connexion et se connecter — les protéger la rendrait
    // inutilisable.
    let public = Router::new()
        .route("/health", get(health::health))
        .route("/auth/status", get(auth::status))
        .route("/auth/setup", post(auth::setup))
        .route("/auth/login", post(auth::login))
        .route("/auth/oidc/start", get(oidc::start))
        .route("/auth/oidc/callback", get(oidc::callback))
        // Réception des mesures poussées par les agents. Volontairement hors du
        // routeur protégé : un agent est un programme installé sur une machine
        // distante, il n'ouvre pas de session et présente un jeton
        // d'enregistrement dans son en-tête `Authorization`. Le passer sous le
        // garde de session couperait tout le parc.
        //
        // La limite de corps est relevée : un lot de rattrapage après une panne de
        // réseau pèse plusieurs mégaoctets, là où le défaut d'axum en refuse deux —
        // et le refuserait à chaque nouvelle tentative, sans issue.
        .route("/ingest", post(ingest::receive).layer(DefaultBodyLimit::max(16 * 1024 * 1024)))
        // Canal de commandes des agents : même jeton, même raison d'être ouvert.
        .merge(agent_commands::agent_routes())
        // Pages de statut publiques : lecture seule, sans session, par conception.
        .merge(status_pages::public_routes());

    // Serveur MCP : authentifié par jeton d'API, pas par session — un assistant
    // n'a pas de navigateur. Le garde ne couvre que cette route ; `GET` reste
    // libre, il ne fait qu'expliquer ce qu'est ce point d'entrée (405).
    let assistant = Router::new()
        .route("/mcp", post(mcp::post))
        .route_layer(crate::auth::token::require_token(
            state.clone(),
            crate::auth::token::Scope::Read,
        ))
        .route("/mcp", get(mcp::get));

    Router::new()
        .nest("/api", public.merge(protected).merge(assistant).fallback(spa::api_not_found))
        // Distribution de l'agent, hors `/api` : ce sont les URL que les scripts
        // d'installation lisent, publiques par nécessité (voir `agent_files`).
        .route("/install.sh", get(agent_files::install_sh))
        .route("/install.ps1", get(agent_files::install_ps1))
        .route("/download/{name}", get(agent_files::download))
        // Tout ce qui n'est pas une route d'API est servi par l'interface web, sans
        // authentification : c'est l'application elle-même qui demande le mot de
        // passe, elle doit donc pouvoir se charger d'abord.
        .fallback(spa::serve)
        .layer(Extension(auth_state))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}
