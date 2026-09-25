mod agent_commands;
mod agent_files;
mod alerts;
mod auth;
mod backup;
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
mod onboarding;
mod opnsense;
mod pbs;
mod pdm;
mod pmg;
mod prometheus;
mod proxmox;
mod push;
mod redfish;
mod relay;
mod spa;
mod status_pages;
mod synology;
mod targets;
mod tokens;
mod totp;
mod truenas;
mod users;

pub use error::{ApiError, ApiResult};

use axum::extract::{DefaultBodyLimit, Request};
use axum::http::HeaderValue;
use axum::middleware::Next;
use axum::response::Response;
use axum::routing::{delete, get, post, put};
use axum::{Extension, Router, middleware};
use tower_http::trace::TraceLayer;

use crate::auth::AuthState;
use crate::state::AppState;

pub fn router(state: AppState) -> Router {
    let auth_state = AuthState::from_env(&state.config);

    // Tout ce qui touche à l'instance — lecture comprise : la liste des
    // équipements d'un homelab est déjà une information à ne pas laisser traîner.
    //
    // Le garde applique aussi les rôles : POST/PUT/DELETE exigent un
    // administrateur, sauf `logout` et `password` que chacun fait pour soi.
    let protected = Router::new()
        .route("/auth/logout", post(auth::logout))
        .route("/auth/password", post(auth::change_password))
        .route("/auth/me", get(auth::me))
        // Second facteur et journal d'audit (`totp.rs`).
        .merge(totp::routes())
        .route(
            "/auth/oidc/config",
            get(oidc::get_config).put(oidc::put_config).delete(oidc::delete_config),
        )
        .route("/auth/oidc/test", post(oidc::test))
        .route("/users", get(users::list).post(users::create))
        .route("/users/{id}", put(users::update).delete(users::delete))
        .route("/collectors", get(collectors::list))
        .route("/discovery", post(discovery::scan))
        .route("/onboarding", get(onboarding::get).put(onboarding::put))
        .route("/targets", get(targets::list).post(targets::create))
        .route("/targets/{id}", get(targets::get_one).put(targets::update).delete(targets::delete))
        .route("/targets/{id}/probe", post(targets::probe_now))
        .route("/targets/{id}/discover", post(targets::discover))
        .route("/metrics/query", get(metrics::query))
        .route("/metrics/query_range", get(metrics::query_range))
        .route("/alerts", get(alerts::list_active))
        .route("/alerts/history", get(alerts::history))
        .route("/alerts/{fingerprint}/ack", post(alerts::ack_alert).delete(alerts::unack_alert))
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
        // Panneau Synology : vue d'ensemble et appareils Active Backup (`synology.rs`).
        .merge(synology::routes())
        // Agents relais proposés par le formulaire d'équipement (`relay.rs`).
        .merge(relay::ui_routes())
        // Tableau des invités d'un hyperviseur Proxmox VE (`proxmox.rs`).
        .merge(proxmox::routes())
        // Politique de notification et surcharges par équipement (`notify_policy.rs`).
        .merge(notify_policy::routes())
        // Pages de statut et incidents (`status_pages.rs`).
        .merge(status_pages::routes())
        // Calendrier des sauvegardes et travaux d'un Proxmox Backup Server (`pbs.rs`).
        .merge(pbs::routes())
        // Instances fédérées et santé d'un Proxmox Datacenter Manager (`pdm.rs`).
        .merge(pdm::routes())
        // Files d'attente, filtrage et santé d'une Proxmox Mail Gateway (`pmg.rs`).
        .merge(pmg::routes())
        // Passerelles, trafic et santé d'un pare-feu OPNsense (`opnsense.rs`).
        .merge(opnsense::routes())
        // Pools, protection des données et santé d'un NAS TrueNAS (`truenas.rs`).
        .merge(truenas::routes())
        // Matériel d'un serveur lu par son contrôleur de gestion (`redfish.rs`).
        .merge(redfish::routes())
        // Moniteurs en poussée : jeton d'une cible et sa régénération (`push.rs`).
        .merge(push::ui_routes())
        // Sauvegarde et restauration de l'instance (`backup.rs`).
        .merge(backup::routes())
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
        .route("/auth/login/totp", post(totp::login_step))
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
        // Sondes déléguées aux agents relais (`relay.rs`) : même canal, et un
        // compte rendu peut peser autant qu'un lot de mesures.
        .merge(relay::agent_routes().layer(DefaultBodyLimit::max(16 * 1024 * 1024)))
        // Pages de statut publiques : lecture seule, sans session, par conception.
        .merge(status_pages::public_routes())
        // Heartbeats : l'URL secrète qu'un cron ou un script appelle (`push.rs`).
        // Ouverte par nécessité — une crontab n'a ni session ni en-tête anti-CSRF.
        .merge(push::public_routes());

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
        // Lecture par un Prometheus ou un Grafana déjà en place, hors `/api` :
        // `/metrics` (santé de l'instance) et `/federate` (les mesures), sous
        // jeton `read` — voir `prometheus.rs`.
        .merge(prometheus::routes(state.clone()))
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
        .layer(middleware::from_fn(security_headers))
        // Le span ne porte que le chemin : la chaîne de requête d'une route peut
        // contenir un code d'autorisation OIDC ou un jeton — rien de tout cela
        // n'a sa place dans un journal, même en `debug`.
        .layer(TraceLayer::new_for_http().make_span_with(|request: &Request| {
            tracing::debug_span!(
                "request",
                method = %request.method(),
                path = %request.uri().path(),
            )
        }))
        .with_state(state)
}

/// Politique de contenu commune, sans la partie qui dépend de la requête.
///
/// L'interface ne charge rien qui ne vienne d'elle-même : les polices sont
/// embarquées dans le build, uPlot aussi, et le seul appel réseau est celui de
/// son propre `/api`. Tout part donc de `default-src 'none'`, et chaque
/// directive ouverte ci-dessous l'est pour une raison vérifiée sur le build :
///
/// - `script-src` : `'self'` pour les modules de `_app/`, plus le nonce de la
///   réponse pour les deux scripts en ligne de la page (choix du thème avant le
///   premier rendu, amorce de SvelteKit). Leur contenu — donc leur empreinte —
///   change à chaque construction ; un nonce par réponse est ce qui permet de
///   s'en passer sans rouvrir `'unsafe-inline'`.
/// - `style-src` : `'unsafe-inline'` est **nécessaire et ne peut pas être
///   resserré**. Svelte pose des attributs `style=` sur les éléments (les
///   directives `style:` des composants, le `display: contents` du gabarit), et
///   un attribut n'est couvert ni par un nonce ni par une empreinte — seul
///   `'unsafe-hashes'` le serait, ce qui revient au même en moins lisible. Le
///   risque résiduel est l'exfiltration par feuille de style injectée, qui
///   suppose déjà une injection HTML.
/// - `img-src` : les icônes de l'interface sont des `data:` SVG produits par le
///   build (les flèches de `<select>`, par exemple).
/// - `font-src`, `connect-src` : `'self'` et rien d'autre. Aucune police
///   Google, aucun CDN, aucune télémétrie — et la politique le rend vérifiable.
/// - `base-uri 'none'`, `form-action 'self'`, `object-src` hérité de
///   `default-src 'none'` : de quoi rendre inopérantes les variantes
///   d'injection qui ne passent pas par un script.
const CSP_BASE: &str = "default-src 'none'; \
     style-src 'self' 'unsafe-inline'; \
     img-src 'self' data:; \
     font-src 'self'; \
     connect-src 'self'; \
     base-uri 'none'; \
     form-action 'self'";

/// Chemins qu'une page tierce peut encadrer : une page de statut
/// (`/s/<slug>`) et sa vue compacte (`/s/<slug>/embed`), rien d'autre — ni les
/// pages de confirmation ou de désabonnement, ni un chemin qui ne ferait que
/// commencer par `/s/` pour qu'une application monopage y serve autre chose.
fn is_embeddable(path: &str) -> bool {
    let Some(rest) = path.strip_prefix("/s/") else { return false };
    let rest = rest.strip_suffix('/').unwrap_or(rest);
    let slug = rest.strip_suffix("/embed").unwrap_or(rest);
    (2..=40).contains(&slug.len())
        && slug.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// En-têtes de protection posés sur toute réponse, interface comme API.
///
/// L'interface est une application monopage : encadrée dans une page tierce,
/// elle se prête au détournement de clic. Seules les pages de statut publiques
/// (`/s/…`) sont faites pour être intégrées ailleurs ; elles restent encadrables.
async fn security_headers(mut request: Request, next: Next) -> Response {
    let embeddable = is_embeddable(request.uri().path());
    // Un nonce par réponse : 128 bits d'aléa, inutilisables une seconde fois.
    let nonce = hex::encode(rand::random::<[u8; 16]>());
    request.extensions_mut().insert(spa::Nonce(nonce.clone()));

    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert("x-content-type-options", HeaderValue::from_static("nosniff"));
    headers.insert("referrer-policy", HeaderValue::from_static("same-origin"));
    let policy = match embeddable {
        // Une page de statut est faite pour être intégrée dans l'intranet de
        // quelqu'un : lui interdire d'être encadrée la rendrait inutile.
        true => format!("{CSP_BASE}; script-src 'self' 'nonce-{nonce}'"),
        false => {
            headers.insert("x-frame-options", HeaderValue::from_static("DENY"));
            format!("{CSP_BASE}; script-src 'self' 'nonce-{nonce}'; frame-ancestors 'none'")
        }
    };
    if let Ok(value) = HeaderValue::from_str(&policy) {
        headers.insert("content-security-policy", value);
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_policy_forbids_everything_it_does_not_name() {
        assert!(CSP_BASE.starts_with("default-src 'none'"));
        // Rien d'extérieur : pas de joker, pas de `http:`, aucun domaine tiers.
        assert!(!CSP_BASE.contains('*'), "un joker ouvrirait la porte : {CSP_BASE}");
        assert!(!CSP_BASE.contains("http"), "aucune origine externe : {CSP_BASE}");
        // Les scripts ne sont jamais autorisés en ligne sans nonce.
        assert!(!CSP_BASE.contains("script-src"), "script-src dépend de la réponse");
    }

    #[test]
    fn only_status_pages_and_their_embed_can_be_framed() {
        assert!(is_embeddable("/s/home-lab"));
        assert!(is_embeddable("/s/home-lab/"));
        assert!(is_embeddable("/s/home-lab/embed"));
        assert!(!is_embeddable("/s/home-lab/unsubscribe"));
        assert!(!is_embeddable("/s/home-lab/confirm"));
        assert!(!is_embeddable("/s/x%2F..%2Fsettings"));
        assert!(!is_embeddable("/s/../settings"));
        assert!(!is_embeddable("/settings"));
        assert!(!is_embeddable("/status/3"));
        assert!(!is_embeddable("/api/public/status/home-lab"));
        assert!(!is_embeddable("/s/"));
    }

    #[test]
    fn each_response_gets_its_own_nonce() {
        let first = hex::encode(rand::random::<[u8; 16]>());
        let second = hex::encode(rand::random::<[u8; 16]>());
        assert_ne!(first, second);
        // Le nonce doit tenir dans un en-tête HTTP tel quel.
        assert!(HeaderValue::from_str(&format!("{CSP_BASE}; script-src 'nonce-{first}'")).is_ok());
    }
}
