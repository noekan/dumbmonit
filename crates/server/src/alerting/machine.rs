//! Machine à états d'une alerte, par empreinte.
//!
//! `ok` → `pending` (condition vraie, `for` non écoulé) → `firing` → `resolved`.
//!
//! La suppression, le silence et l'acquittement ne sont pas des états de cette
//! machine : ce sont des surcouches appliquées ensuite (voir
//! [`crate::alerting::suppress`], [`crate::alerting::silence`] et
//! [`AlertState::is_acked`]). La distinction est délibérée — pendant une fenêtre
//! de maintenance, la condition continue d'être suivie, si bien qu'à la sortie une
//! alerte toujours active n'est pas renotifiée comme si elle venait de naître.

use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    #[default]
    Ok,
    Pending,
    Firing,
    Resolved,
}

impl Phase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Pending => "pending",
            Self::Firing => "firing",
            Self::Resolved => "resolved",
        }
    }

    pub fn parse(raw: &str) -> Self {
        match raw {
            "pending" => Self::Pending,
            "firing" => Self::Firing,
            "resolved" => Self::Resolved,
            _ => Self::Ok,
        }
    }

    /// Vrai quand l'alerte compte comme active pour l'interface et les rappels.
    pub fn is_active(self) -> bool {
        matches!(self, Self::Firing)
    }
}

/// Phase telle qu'elle est présentée à l'utilisateur, surcouches comprises.
///
/// `suppressed` masque `firing` : c'est ce que l'interface doit afficher, et c'est
/// aussi ce qui explique l'absence de notification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EffectivePhase {
    Ok,
    Pending,
    Firing,
    Resolved,
    Suppressed,
}

impl EffectivePhase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Pending => "pending",
            Self::Firing => "firing",
            Self::Resolved => "resolved",
            Self::Suppressed => "suppressed",
        }
    }
}

/// État persistant d'une empreinte.
#[derive(Debug, Clone, PartialEq)]
pub struct AlertState {
    pub phase: Phase,
    /// Instant où la condition est devenue vraie sans discontinuer. C'est la seule
    /// donnée qui pilote `for` : la stocker plutôt qu'un compteur de cycles rend la
    /// durée exacte, même si un cycle a sauté ou si la période a changé.
    pub condition_since: Option<DateTime<Utc>>,
    pub firing_since: Option<DateTime<Utc>>,
    pub resolved_at: Option<DateTime<Utc>>,
    pub last_eval_at: Option<DateTime<Utc>>,
    pub last_notified_at: Option<DateTime<Utc>>,
    pub notify_count: u32,
    pub suppressed: bool,
    pub suppressed_by: Option<i64>,
    pub silenced: bool,
    pub learning: bool,
    pub value: Option<f64>,
    pub score: Option<f64>,
    /// Fin de l'acquittement. Tant qu'elle est à venir, l'alerte ne rappelle
    /// pas ; la résolution, elle, est toujours annoncée. Effacé dès que la
    /// condition retombe : une alerte qui revient plus tard notifie à nouveau.
    pub acked_until: Option<DateTime<Utc>>,
    /// Qui a acquitté : nom du compte, ou `token:nom` pour un jeton d'API.
    pub acked_by: Option<String>,
    /// Pourquoi, en une ligne, à l'intention des autres.
    pub ack_note: Option<String>,
}

impl Default for AlertState {
    fn default() -> Self {
        Self {
            phase: Phase::Ok,
            condition_since: None,
            firing_since: None,
            resolved_at: None,
            last_eval_at: None,
            last_notified_at: None,
            notify_count: 0,
            suppressed: false,
            suppressed_by: None,
            silenced: false,
            learning: false,
            value: None,
            score: None,
            acked_until: None,
            acked_by: None,
            ack_note: None,
        }
    }
}

impl AlertState {
    /// Vrai tant que l'acquittement court : l'alerte est active et sa fin
    /// d'acquittement est encore à venir. Un acquittement échu n'a pas besoin
    /// d'être effacé pour cesser d'agir.
    pub fn is_acked(&self, now: DateTime<Utc>) -> bool {
        matches!(self.phase, Phase::Firing | Phase::Pending)
            && self.acked_until.is_some_and(|until| until > now)
    }

    /// Oublie l'acquittement, quel qu'en soit l'état.
    pub fn clear_ack(&mut self) {
        self.acked_until = None;
        self.acked_by = None;
        self.ack_note = None;
    }

    pub fn effective_phase(&self) -> EffectivePhase {
        if self.suppressed && matches!(self.phase, Phase::Firing | Phase::Pending) {
            return EffectivePhase::Suppressed;
        }
        match self.phase {
            Phase::Ok => EffectivePhase::Ok,
            Phase::Pending => EffectivePhase::Pending,
            Phase::Firing => EffectivePhase::Firing,
            Phase::Resolved => EffectivePhase::Resolved,
        }
    }
}

/// Résultat d'une transition : le nouvel état et la transition franchie, s'il y en a eu.
#[derive(Debug, Clone, PartialEq)]
pub struct Transition {
    pub from: Phase,
    pub to: Phase,
}

/// Fait avancer la machine d'un cycle.
///
/// `now` est l'horloge du cycle, passée explicitement : c'est ce qui rend le respect
/// de `for` vérifiable à la seconde près dans les tests, sans dormir.
pub fn advance(
    previous: &AlertState,
    condition_met: bool,
    now: DateTime<Utc>,
    for_duration: Duration,
) -> (AlertState, Option<Transition>) {
    let mut next = previous.clone();
    next.last_eval_at = Some(now);

    let from = previous.phase;

    if !condition_met {
        // La condition retombe : seule une alerte réellement partie mérite une
        // résolution. Un `pending` qui s'éteint n'a jamais rien notifié, il repart
        // silencieusement à `ok`.
        next.condition_since = None;
        next.phase = match from {
            Phase::Firing => Phase::Resolved,
            _ => Phase::Ok,
        };
        // L'acquittement disait « je sais, ne me le rappelle plus » : le problème
        // parti, il n'a plus d'objet. Si la condition revient, l'alerte repart de
        // zéro et notifie comme une alerte neuve.
        next.clear_ack();
        if next.phase == Phase::Resolved {
            next.resolved_at = Some(now);
        } else {
            next.resolved_at = None;
            next.firing_since = None;
            // Une alerte revenue à `ok` a soldé son historique de notifications :
            // la prochaine occurrence doit notifier comme une alerte neuve.
            if from != Phase::Ok {
                next.last_notified_at = None;
                next.notify_count = 0;
            }
        }
        let transition = (from != next.phase).then_some(Transition { from, to: next.phase });
        return (next, transition);
    }

    // La condition est vraie : on date son début si elle vient d'apparaître.
    let since = match from {
        Phase::Pending | Phase::Firing => previous.condition_since.unwrap_or(now),
        _ => now,
    };
    next.condition_since = Some(since);

    if from == Phase::Firing {
        // Déjà partie : rien à franchir, les rappels sont gérés ailleurs.
        return (next, None);
    }

    // Comparaison en signé : une horloge qui recule (NTP) donne un écart négatif,
    // qu'on ne doit surtout pas convertir en durée gigantesque.
    let elapsed = now.signed_duration_since(since);
    let held_long_enough = elapsed >= chrono::TimeDelta::from_std(for_duration).unwrap_or_default();

    next.phase = if held_long_enough { Phase::Firing } else { Phase::Pending };
    if next.phase == Phase::Firing {
        next.firing_since = Some(now);
        next.resolved_at = None;
    }

    let transition = (from != next.phase).then_some(Transition { from, to: next.phase });
    (next, transition)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(seconds: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_700_000_000 + seconds, 0).expect("valid timestamp")
    }

    const FOR_5M: Duration = Duration::from_secs(300);

    #[test]
    fn une_condition_vraie_passe_d_abord_en_pending() {
        let (state, transition) = advance(&AlertState::default(), true, t(0), FOR_5M);
        assert_eq!(state.phase, Phase::Pending);
        assert_eq!(state.condition_since, Some(t(0)));
        assert_eq!(transition, Some(Transition { from: Phase::Ok, to: Phase::Pending }));
    }

    #[test]
    fn le_for_est_respecte_a_la_seconde_pres() {
        let (pending, _) = advance(&AlertState::default(), true, t(0), FOR_5M);

        let (encore_pending, transition) = advance(&pending, true, t(299), FOR_5M);
        assert_eq!(encore_pending.phase, Phase::Pending, "299 s < 300 s: not yet");
        assert_eq!(transition, None);

        let (firing, transition) = advance(&encore_pending, true, t(300), FOR_5M);
        assert_eq!(firing.phase, Phase::Firing, "300 s: the threshold is reached");
        assert_eq!(firing.firing_since, Some(t(300)));
        assert_eq!(transition, Some(Transition { from: Phase::Pending, to: Phase::Firing }));
    }

    #[test]
    fn un_for_nul_declenche_immediatement() {
        let (state, transition) = advance(&AlertState::default(), true, t(0), Duration::ZERO);
        assert_eq!(state.phase, Phase::Firing);
        assert_eq!(transition, Some(Transition { from: Phase::Ok, to: Phase::Firing }));
    }

    #[test]
    fn une_condition_qui_retombe_pendant_le_for_ne_notifie_jamais() {
        let (pending, _) = advance(&AlertState::default(), true, t(0), FOR_5M);
        let (ok, transition) = advance(&pending, false, t(100), FOR_5M);
        assert_eq!(ok.phase, Phase::Ok);
        assert_eq!(ok.condition_since, None);
        assert_eq!(transition, Some(Transition { from: Phase::Pending, to: Phase::Ok }));
    }

    #[test]
    fn une_intermittence_relance_le_compteur_for() {
        let (pending, _) = advance(&AlertState::default(), true, t(0), FOR_5M);
        let (ok, _) = advance(&pending, false, t(299), FOR_5M);
        let (repending, _) = advance(&ok, true, t(300), FOR_5M);
        assert_eq!(repending.condition_since, Some(t(300)), "the counter restarts from zero");

        let (toujours_pending, _) = advance(&repending, true, t(599), FOR_5M);
        assert_eq!(toujours_pending.phase, Phase::Pending);
        let (firing, _) = advance(&toujours_pending, true, t(600), FOR_5M);
        assert_eq!(firing.phase, Phase::Firing);
    }

    #[test]
    fn une_alerte_partie_se_resout_quand_la_condition_retombe() {
        let (firing, _) = advance(&AlertState::default(), true, t(0), Duration::ZERO);
        let (resolved, transition) = advance(&firing, false, t(60), Duration::ZERO);
        assert_eq!(resolved.phase, Phase::Resolved);
        assert_eq!(resolved.resolved_at, Some(t(60)));
        assert_eq!(transition, Some(Transition { from: Phase::Firing, to: Phase::Resolved }));
    }

    #[test]
    fn un_resolved_confirme_retombe_en_ok_et_solde_les_notifications() {
        let (firing, _) = advance(&AlertState::default(), true, t(0), Duration::ZERO);
        let mut firing = firing;
        firing.notify_count = 3;
        firing.last_notified_at = Some(t(0));

        let (resolved, _) = advance(&firing, false, t(60), Duration::ZERO);
        let (ok, transition) = advance(&resolved, false, t(120), Duration::ZERO);
        assert_eq!(ok.phase, Phase::Ok);
        assert_eq!(ok.notify_count, 0, "the next occurrence must notify afresh");
        assert_eq!(ok.last_notified_at, None);
        assert_eq!(transition, Some(Transition { from: Phase::Resolved, to: Phase::Ok }));
    }

    #[test]
    fn un_firing_qui_dure_ne_produit_plus_de_transition() {
        let (firing, _) = advance(&AlertState::default(), true, t(0), Duration::ZERO);
        let (encore, transition) = advance(&firing, true, t(3600), Duration::ZERO);
        assert_eq!(encore.phase, Phase::Firing);
        assert_eq!(encore.firing_since, Some(t(0)), "the alert age is preserved");
        assert_eq!(transition, None);
    }

    #[test]
    fn une_horloge_qui_recule_ne_declenche_pas_par_accident() {
        let (pending, _) = advance(&AlertState::default(), true, t(1000), FOR_5M);
        let (toujours_pending, _) = advance(&pending, true, t(900), FOR_5M);
        assert_eq!(toujours_pending.phase, Phase::Pending);
    }

    #[test]
    fn l_acquittement_ne_tient_que_pendant_sa_duree_et_sur_une_alerte_active() {
        let mut state = AlertState {
            phase: Phase::Firing,
            acked_until: Some(t(3600)),
            acked_by: Some("admin".to_string()),
            ..Default::default()
        };
        assert!(state.is_acked(t(0)));
        assert!(state.is_acked(t(3599)));
        assert!(!state.is_acked(t(3600)), "expired: the reminders come back");
        state.phase = Phase::Resolved;
        assert!(!state.is_acked(t(0)), "a resolved alert is not acked, whatever the row says");
    }

    #[test]
    fn la_resolution_efface_l_acquittement() {
        let (firing, _) = advance(&AlertState::default(), true, t(0), Duration::ZERO);
        let mut firing = firing;
        firing.acked_until = Some(t(14_400));
        firing.acked_by = Some("admin".to_string());
        firing.ack_note = Some("on it".to_string());

        let (still, _) = advance(&firing, true, t(60), Duration::ZERO);
        assert_eq!(still.acked_until, Some(t(14_400)), "still firing: the ack holds");

        let (resolved, _) = advance(&still, false, t(120), Duration::ZERO);
        assert_eq!(resolved.phase, Phase::Resolved);
        assert_eq!(resolved.acked_until, None);
        assert_eq!(resolved.acked_by, None);
        assert_eq!(resolved.ack_note, None);

        // Une alerte qui revient plus tard repart sans acquittement.
        let (ok, _) = advance(&resolved, false, t(180), Duration::ZERO);
        let (again, _) = advance(&ok, true, t(240), Duration::ZERO);
        assert_eq!(again.phase, Phase::Firing);
        assert!(!again.is_acked(t(240)));
    }

    #[test]
    fn la_phase_effective_masque_firing_par_suppressed() {
        let mut state = AlertState { phase: Phase::Firing, ..Default::default() };
        assert_eq!(state.effective_phase(), EffectivePhase::Firing);
        state.suppressed = true;
        assert_eq!(state.effective_phase(), EffectivePhase::Suppressed);
        state.phase = Phase::Resolved;
        assert_eq!(state.effective_phase(), EffectivePhase::Resolved);
    }
}
