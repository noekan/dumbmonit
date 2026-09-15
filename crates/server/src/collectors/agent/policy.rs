//! Automate des politiques par conteneur : redémarrage et mise à jour sans
//! intervention, dans les limites que l'utilisateur a fixées.
//!
//! La décision est une fonction pure ([`decide`]) : ce que l'automate ferait
//! dans une situation donnée se vérifie sans base ni VictoriaMetrics. La tâche
//! de fond ([`spawn_policy_scheduler`]) ne fait qu'aller chercher la situation
//! et déposer les commandes.

use std::collections::BTreeMap;

use chrono::{DateTime, TimeDelta, Utc};
use ezymonit_proto::{CMD_CONTAINER_RESTART, CMD_CONTAINER_UPDATE, TargetId};
use tokio::time::{MissedTickBehavior, interval};
use tracing::{debug, info, warn};

use super::commands::{self, ContainerPolicy};
use crate::alerting::silence;
use crate::db;
use crate::state::AppState;

/// Cadence de l'automate. Une minute : un conteneur tombé attend déjà les deux
/// minutes de la règle « arrêté », inutile d'aller plus vite que l'alerte.
const TICK: std::time::Duration = std::time::Duration::from_secs(60);

/// Pas plus d'un redémarrage automatique par dix minutes et par conteneur : un
/// conteneur qui retombe aussitôt a un problème qu'un redémarrage n'arrangera
/// pas, et le relancer en boucle ne ferait que masquer l'alerte.
const RESTART_COOLDOWN: TimeDelta = TimeDelta::minutes(10);

/// Une mise à jour ratée n'est pas retentée avant six heures : le temps de
/// s'en apercevoir dans l'interface.
const UPDATE_COOLDOWN: TimeDelta = TimeDelta::hours(6);

/// Ce que les dernières mesures disent d'un conteneur.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContainerObservation {
    pub name: String,
    pub up: bool,
    pub update_available: bool,
}

/// Une commande que l'automate veut déposer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    pub kind: &'static str,
    pub args: serde_json::Value,
}

/// Ce que l'automate fait pour un conteneur, étant donné sa politique et sa
/// situation.
pub fn decide(
    policy: &ContainerPolicy,
    obs: &ContainerObservation,
    now: DateTime<Utc>,
    last_restart: Option<DateTime<Utc>>,
    last_update: Option<DateTime<Utc>>,
    has_pending: bool,
    in_maintenance: bool,
) -> Vec<Decision> {
    let mut decisions = Vec::new();
    if has_pending {
        // Une commande attend déjà l'agent : la doubler n'apporterait rien.
        return decisions;
    }
    let cooled = |last: Option<DateTime<Utc>>, cooldown: TimeDelta| {
        last.is_none_or(|at| now - at >= cooldown)
    };

    if policy.auto_restart && !obs.up && cooled(last_restart, RESTART_COOLDOWN) {
        decisions.push(Decision {
            kind: CMD_CONTAINER_RESTART,
            args: serde_json::json!({ "name": obs.name }),
        });
    }

    let window_ok = !policy.only_in_maintenance || in_maintenance;
    if policy.auto_update
        && obs.update_available
        && window_ok
        && cooled(last_update, UPDATE_COOLDOWN)
    {
        decisions.push(Decision {
            kind: CMD_CONTAINER_UPDATE,
            args: serde_json::json!({ "name": obs.name, "prune": policy.prune_old_image }),
        });
    }
    decisions
}

/// Lance l'automate en tâche de fond.
pub fn spawn_policy_scheduler(state: AppState) {
    tokio::spawn(async move {
        let mut ticker = interval(TICK);
        ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
        info!(interval = ?TICK, "container policy scheduler started");
        loop {
            ticker.tick().await;
            if let Err(error) = run_once(&state).await {
                warn!(?error, "container policy cycle failed");
            }
        }
    });
}

/// Un passage de l'automate. Renvoie le nombre de commandes déposées.
pub async fn run_once(state: &AppState) -> anyhow::Result<usize> {
    let policies = commands::list_active_policies(&state.pool).await?;
    if policies.is_empty() {
        return Ok(0);
    }

    let observations = observe(state).await?;
    let silences = db::alerts::list_silences(&state.pool).await?;
    let now = Utc::now();
    let mut queued = 0;

    for (target_id, container, policy) in policies {
        let Some(obs) = observations.get(&(target_id, container.clone())) else {
            // Plus de mesure pour ce conteneur : il a été retiré, ou l'agent se
            // tait. Dans les deux cas, rien à faire de sûr.
            continue;
        };
        let labels: BTreeMap<String, String> =
            [("container".to_string(), container.clone())].into_iter().collect();
        let in_maintenance =
            silence::first_match(&silences, now, Some(target_id), &labels).is_some();

        let pending_restart =
            commands::has_pending(&state.pool, target_id, CMD_CONTAINER_RESTART, &container)
                .await?;
        let pending_update =
            commands::has_pending(&state.pool, target_id, CMD_CONTAINER_UPDATE, &container).await?;
        let last_restart =
            commands::last_command_at(&state.pool, target_id, CMD_CONTAINER_RESTART, &container)
                .await?;
        let last_update =
            commands::last_command_at(&state.pool, target_id, CMD_CONTAINER_UPDATE, &container)
                .await?;

        for decision in decide(
            &policy,
            obs,
            now,
            last_restart,
            last_update,
            pending_restart || pending_update,
            in_maintenance,
        ) {
            match commands::enqueue(&state.pool, target_id, decision.kind, &decision.args, "policy")
                .await
            {
                Ok(record) => {
                    info!(
                        target = target_id,
                        container,
                        kind = decision.kind,
                        command = record.id,
                        "command queued by policy"
                    );
                    queued += 1;
                }
                Err(commands::CommandError::Conflict(why)) => debug!(container, why),
                Err(error) => warn!(target = target_id, container, %error, "cannot queue command"),
            }
        }
    }
    Ok(queued)
}

/// Dernier état connu de chaque conteneur, toutes cibles confondues.
async fn observe(
    state: &AppState,
) -> anyhow::Result<BTreeMap<(TargetId, String), ContainerObservation>> {
    let series =
        state.victoria.query(r#"{__name__=~"ezymonit_container_(up|update_available)"}"#).await?;
    let mut observations = BTreeMap::new();
    for serie in series {
        let (Some(target), Some(container), Some(name)) = (
            serie.metric.get("target").and_then(|t| t.parse::<TargetId>().ok()),
            serie.metric.get("container"),
            serie.metric.get("__name__"),
        ) else {
            continue;
        };
        let value: f64 = serie.value.1.parse().unwrap_or(f64::NAN);
        let entry = observations.entry((target, container.clone())).or_insert_with(|| {
            ContainerObservation { name: container.clone(), up: true, update_available: false }
        });
        match name.as_str() {
            "ezymonit_container_up" => entry.up = value != 0.0,
            "ezymonit_container_update_available" => entry.update_available = value == 1.0,
            _ => {}
        }
    }
    Ok(observations)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(iso: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(iso).unwrap().with_timezone(&Utc)
    }

    fn down() -> ContainerObservation {
        ContainerObservation { name: "web".into(), up: false, update_available: false }
    }

    fn outdated() -> ContainerObservation {
        ContainerObservation { name: "web".into(), up: true, update_available: true }
    }

    fn kinds(decisions: &[Decision]) -> Vec<&'static str> {
        decisions.iter().map(|d| d.kind).collect()
    }

    const NOW: &str = "2026-03-01T12:00:00Z";

    #[test]
    fn the_default_policy_never_acts() {
        let policy = ContainerPolicy::default();
        assert!(decide(&policy, &down(), at(NOW), None, None, false, true).is_empty());
        assert!(decide(&policy, &outdated(), at(NOW), None, None, false, true).is_empty());
    }

    #[test]
    fn a_stopped_container_is_restarted_once_per_ten_minutes() {
        let policy = ContainerPolicy { auto_restart: true, ..ContainerPolicy::default() };
        let first = decide(&policy, &down(), at(NOW), None, None, false, false);
        assert_eq!(kinds(&first), vec![CMD_CONTAINER_RESTART]);
        assert_eq!(first[0].args["name"], "web");

        // Redémarré il y a cinq minutes : on attend.
        let recent = Some(at("2026-03-01T11:55:00Z"));
        assert!(decide(&policy, &down(), at(NOW), recent, None, false, false).is_empty());
        // Il y a dix minutes pile : la limite est inclusive.
        let old = Some(at("2026-03-01T11:50:00Z"));
        assert_eq!(decide(&policy, &down(), at(NOW), old, None, false, false).len(), 1);
        // Un conteneur en marche n'est pas redémarré.
        let up = ContainerObservation { up: true, ..down() };
        assert!(decide(&policy, &up, at(NOW), None, None, false, false).is_empty());
    }

    #[test]
    fn updates_wait_for_a_maintenance_window_unless_told_otherwise() {
        let policy = ContainerPolicy { auto_update: true, ..ContainerPolicy::default() };
        assert!(decide(&policy, &outdated(), at(NOW), None, None, false, false).is_empty());
        let inside = decide(&policy, &outdated(), at(NOW), None, None, false, true);
        assert_eq!(kinds(&inside), vec![CMD_CONTAINER_UPDATE]);
        // Le nettoyage de l'image suit la politique.
        assert_eq!(inside[0].args["prune"], true);

        let anytime = ContainerPolicy {
            auto_update: true,
            only_in_maintenance: false,
            prune_old_image: false,
            ..ContainerPolicy::default()
        };
        let outside = decide(&anytime, &outdated(), at(NOW), None, None, false, false);
        assert_eq!(kinds(&outside), vec![CMD_CONTAINER_UPDATE]);
        assert_eq!(outside[0].args["prune"], false);
    }

    #[test]
    fn a_failed_update_is_not_retried_for_six_hours() {
        let policy = ContainerPolicy {
            auto_update: true,
            only_in_maintenance: false,
            ..ContainerPolicy::default()
        };
        let recent = Some(at("2026-03-01T08:00:00Z"));
        assert!(decide(&policy, &outdated(), at(NOW), None, recent, false, true).is_empty());
        let old = Some(at("2026-03-01T05:00:00Z"));
        assert_eq!(decide(&policy, &outdated(), at(NOW), None, old, false, true).len(), 1);
        // Rien à mettre à jour : rien à faire.
        let current = ContainerObservation { update_available: false, ..outdated() };
        assert!(decide(&policy, &current, at(NOW), None, None, false, true).is_empty());
    }

    #[test]
    fn a_pending_command_blocks_everything() {
        let policy = ContainerPolicy {
            auto_restart: true,
            auto_update: true,
            only_in_maintenance: false,
            ..ContainerPolicy::default()
        };
        let both = ContainerObservation { name: "web".into(), up: false, update_available: true };
        assert_eq!(
            kinds(&decide(&policy, &both, at(NOW), None, None, false, false)),
            vec![CMD_CONTAINER_RESTART, CMD_CONTAINER_UPDATE]
        );
        assert!(decide(&policy, &both, at(NOW), None, None, true, false).is_empty());
    }
}
