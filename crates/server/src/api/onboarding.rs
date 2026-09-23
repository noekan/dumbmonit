//! État du guide de premier lancement.
//!
//! L'écran d'accueil d'une instance vide n'est pas un bulletin vide : c'est un
//! chemin en trois étapes — ajouter un équipement, brancher un moyen d'être
//! prévenu, vérifier qu'un message arrive. Ce module dit à l'interface où en est
//! l'instance et retient ce que l'utilisateur a écarté.
//!
//! Deux raisons de le faire côté serveur plutôt que dans le navigateur :
//!
//! - un « passer » enregistré dans le stockage local reviendrait sur un autre
//!   navigateur, et le guide harcèlerait quelqu'un qui l'a déjà refusé ;
//! - « un message est-il vraiment arrivé ? » est une vérité du serveur
//!   (`notification_channels.last_sent_at`), pas une case cochée par l'interface.
//!
//! L'achèvement est *verrouillé* : une fois la date écrite, le guide ne revient
//! plus, même si le canal est supprimé ensuite. Un opérateur installé n'a pas à
//! revoir un tutoriel parce qu'il a fait le ménage dans ses notifications.
//!
//! Deux façons de le verrouiller, et la première compte autant que l'autre :
//!
//! - **à la toute première lecture**, si l'instance a déjà un équipement et un
//!   canal. C'est le cas de la mise à jour : un parc en place ne se fait pas
//!   proposer un tutoriel de premier lancement ;
//! - **quand les trois étapes sont réglées**, faites ou écartées une à une.

use axum::Json;
use axum::extract::State;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use sqlx::{Row, SqlitePool};

use crate::api::{ApiError, ApiResult};
use crate::auth::middleware::AdminIdentity;
use crate::auth::settings;
use crate::state::AppState;

/// Clé du réglage persistant, dans la table `settings`.
const KEY: &str = "onboarding";

/// Identifiants d'étapes acceptés. Une valeur inconnue est refusée plutôt
/// qu'enregistrée : le jour où une étape est renommée, les anciennes entrées ne
/// masqueraient pas silencieusement une étape existante.
const STEPS: [&str; 3] = ["device", "channel", "test"];

/// Ce qui est écrit en base. Tout le reste de la réponse est recalculé à chaque
/// lecture à partir de l'état réel de l'instance.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Stored {
    #[serde(default)]
    skipped: bool,
    /// Étapes écartées une à une, par leur identifiant.
    #[serde(default)]
    dismissed: Vec<String>,
    /// Première lecture du guide. Sert à distinguer une instance neuve d'un parc
    /// déjà en place le jour de la mise à jour.
    #[serde(default)]
    started_at: Option<String>,
    /// Date du verrouillage. Une fois posée, elle n'est jamais effacée.
    #[serde(default)]
    completed_at: Option<String>,
}

/// Ce que l'interface lit : l'avancement, et ce qui a été écarté.
#[derive(Debug, Serialize)]
pub struct OnboardingState {
    /// Vrai quand l'utilisateur a cliqué « Skip ».
    pub skipped: bool,
    /// Vrai une fois le guide terminé — ou jugé inutile sur un parc déjà en
    /// place. Verrouillé : il ne redevient jamais faux.
    pub complete: bool,
    /// Horodatage du verrouillage, UTC sans suffixe. `null` tant qu'il n'a pas eu lieu.
    pub completed_at: Option<String>,
    pub dismissed: Vec<String>,
    /// Au moins un équipement est enregistré.
    pub has_target: bool,
    /// Au moins un canal de notification actif est enregistré.
    pub has_channel: bool,
    /// Un message est réellement parti sur un canal — test compris. C'est la
    /// seule preuve que la troisième étape a abouti.
    pub notification_confirmed: bool,
}

/// Modification envoyée par l'interface. Chaque champ absent conserve sa valeur.
#[derive(Debug, Default, Deserialize)]
pub struct OnboardingPatch {
    #[serde(default)]
    pub skipped: Option<bool>,
    #[serde(default)]
    pub dismissed: Option<Vec<String>>,
}

/// `GET /api/onboarding` — où en est le guide de premier lancement.
///
/// La lecture écrit, une fois : c'est le seul moment où le serveur voit à la fois
/// l'état enregistré et l'état réel de l'instance, et c'est donc là que le verrou
/// se pose. Ensuite, plus rien n'est écrit.
pub async fn get(State(state): State<AppState>) -> ApiResult<Json<OnboardingState>> {
    let mut stored = load(&state.pool).await?;
    let facts = read_facts(&state.pool).await?;

    if stored.completed_at.is_none() {
        let now = Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
        // Un parc déjà en place à la première lecture n'a rien à apprendre ici.
        let established = stored.started_at.is_none() && facts.has_target && facts.has_channel;
        if stored.started_at.is_none() {
            stored.started_at = Some(now.clone());
        }
        if established || all_steps_settled(&stored, &facts) {
            stored.completed_at = Some(now);
        }
        settings::set(&state.pool, KEY, &stored).await?;
    }

    Ok(Json(render(stored, facts)))
}

/// `PUT /api/onboarding` — écarte une étape, ou le guide entier.
///
/// Réservé aux administrateurs, comme toute écriture : un lecteur ne peut de
/// toute façon ni ajouter un équipement ni créer un canal, et l'interface ne lui
/// montre pas le guide.
pub async fn put(
    _: AdminIdentity,
    State(state): State<AppState>,
    Json(patch): Json<OnboardingPatch>,
) -> ApiResult<Json<OnboardingState>> {
    let mut stored = load(&state.pool).await?;

    if let Some(skipped) = patch.skipped {
        stored.skipped = skipped;
    }
    if let Some(dismissed) = patch.dismissed {
        for step in &dismissed {
            if !STEPS.contains(&step.as_str()) {
                return Err(ApiError::BadRequest(format!(
                    "Unknown step \"{step}\" (available: {})",
                    STEPS.join(", ")
                )));
            }
        }
        // Dédoublonné dans l'ordre des étapes : la valeur enregistrée ne dépend
        // pas de l'ordre dans lequel l'interface a coché.
        stored.dismissed = STEPS
            .iter()
            .filter(|step| dismissed.iter().any(|s| s == *step))
            .map(|s| s.to_string())
            .collect();
    }

    settings::set(&state.pool, KEY, &stored).await?;
    let facts = read_facts(&state.pool).await?;
    Ok(Json(render(stored, facts)))
}

// --------------------------------------------------------------------------

/// Une étape est réglée quand elle est faite, ou quand l'utilisateur l'a écartée.
fn settled(step: &str, stored: &Stored, facts: &Facts) -> bool {
    let done = match step {
        "device" => facts.has_target,
        "channel" => facts.has_channel,
        "test" => facts.notification_confirmed,
        _ => false,
    };
    done || stored.dismissed.iter().any(|s| s == step)
}

fn all_steps_settled(stored: &Stored, facts: &Facts) -> bool {
    STEPS.iter().all(|step| settled(step, stored, facts))
}

/// L'état réel de l'instance, en une requête par question.
struct Facts {
    has_target: bool,
    has_channel: bool,
    notification_confirmed: bool,
}

async fn load(pool: &SqlitePool) -> ApiResult<Stored> {
    // Un réglage illisible (écrit à la main, ou par une version future) ne doit
    // pas rendre l'accueil inaccessible : on repart des valeurs par défaut.
    Ok(settings::get::<Stored>(pool, KEY).await.unwrap_or_default().unwrap_or_default())
}

async fn read_facts(pool: &SqlitePool) -> ApiResult<Facts> {
    let has_target: i64 =
        sqlx::query("SELECT EXISTS(SELECT 1 FROM targets)").fetch_one(pool).await?.try_get(0)?;

    let row = sqlx::query(
        "SELECT EXISTS(SELECT 1 FROM notification_channels WHERE enabled = 1) AS has_channel,
                EXISTS(SELECT 1 FROM notification_channels WHERE last_sent_at IS NOT NULL) AS sent",
    )
    .fetch_one(pool)
    .await?;

    Ok(Facts {
        has_target: has_target != 0,
        has_channel: row.try_get::<i64, _>("has_channel")? != 0,
        notification_confirmed: row.try_get::<i64, _>("sent")? != 0,
    })
}

fn render(stored: Stored, facts: Facts) -> OnboardingState {
    OnboardingState {
        skipped: stored.skipped,
        complete: stored.completed_at.is_some(),
        completed_at: stored.completed_at,
        dismissed: stored.dismissed,
        has_target: facts.has_target,
        has_channel: facts.has_channel,
        notification_confirmed: facts.notification_confirmed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(has_target: bool, has_channel: bool, sent: bool) -> Facts {
        Facts { has_target, has_channel, notification_confirmed: sent }
    }

    #[test]
    fn a_fresh_instance_shows_the_guide() {
        let state = render(Stored::default(), facts(false, false, false));
        assert!(!state.complete);
        assert!(!state.skipped);
        assert!(state.dismissed.is_empty());
    }

    #[test]
    fn completion_is_latched_not_recomputed() {
        // Le canal a disparu depuis, le guide ne revient pas.
        let stored =
            Stored { completed_at: Some("2026-09-22 10:00:00".into()), ..Stored::default() };
        let state = render(stored, facts(true, false, false));
        assert!(state.complete);
        assert!(!state.has_channel);
    }

    #[test]
    fn nothing_is_settled_on_a_fresh_instance() {
        assert!(!all_steps_settled(&Stored::default(), &facts(false, false, false)));
    }

    #[test]
    fn a_device_and_a_channel_are_not_enough_the_test_remains() {
        // Sans preuve qu'un message est parti, la troisième étape reste à faire :
        // c'est tout l'intérêt de la proposer.
        assert!(!all_steps_settled(&Stored::default(), &facts(true, true, false)));
    }

    #[test]
    fn the_three_steps_done_settle_the_guide() {
        assert!(all_steps_settled(&Stored::default(), &facts(true, true, true)));
    }

    #[test]
    fn a_dismissed_step_counts_as_settled() {
        let stored = Stored { dismissed: vec!["test".into()], ..Stored::default() };
        assert!(all_steps_settled(&stored, &facts(true, true, false)));
    }

    #[test]
    fn the_steps_are_the_ones_the_interface_can_dismiss() {
        assert_eq!(STEPS, ["device", "channel", "test"]);
    }
}
