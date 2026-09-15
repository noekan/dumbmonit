//! Silences : fenêtres de maintenance pendant lesquelles on n'envoie rien.
//!
//! Un silence n'interrompt pas l'évaluation : la machine à états continue de tourner
//! derrière. C'est ce qui permet, à la fin d'une maintenance, de constater qu'une
//! alerte est toujours active et de la notifier une fois, plutôt que de la voir
//! « naître » alors qu'elle dure depuis deux heures.

use std::collections::BTreeMap;

use chrono::{DateTime, Datelike, TimeDelta, Timelike, Utc};
use serde::{Deserialize, Serialize};

use crate::alerting::model::TargetId;

/// Nombre de minutes dans une journée, borne haute d'une fenêtre récurrente.
pub const MINUTES_PER_DAY: u32 = 24 * 60;

/// Planification d'un silence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Schedule {
    /// Fenêtre ponctuelle : bornes absolues, inclusive à gauche, exclusive à droite.
    Once { starts_at: DateTime<Utc>, ends_at: DateTime<Utc> },
    /// Fenêtre hebdomadaire, exprimée dans le fuseau de l'utilisateur.
    ///
    /// Le décalage est porté par le silence lui-même plutôt que par une configuration
    /// globale : « tous les dimanches de 2 h à 4 h » doit rester à 2 h locales même
    /// après un changement d'heure, et c'est aussi ce que l'utilisateur a saisi.
    Weekly {
        /// Jours concernés, 0 = lundi … 6 = dimanche.
        days: Vec<u8>,
        /// Minute de début dans la journée locale, 0..1440.
        start_minute: u32,
        /// Minute de fin. Si elle est inférieure ou égale au début, la fenêtre
        /// déborde sur le lendemain (« 23 h → 1 h »).
        end_minute: u32,
        #[serde(default)]
        utc_offset_minutes: i32,
    },
}

impl Schedule {
    /// Vrai si `now` tombe dans la fenêtre.
    pub fn covers(&self, now: DateTime<Utc>) -> bool {
        match self {
            Self::Once { starts_at, ends_at } => now >= *starts_at && now < *ends_at,
            Self::Weekly { days, start_minute, end_minute, utc_offset_minutes } => {
                let local = now + TimeDelta::minutes(i64::from(*utc_offset_minutes));
                let minute = local.hour() * 60 + local.minute();
                let day = local.weekday().num_days_from_monday() as u8;

                let start = *start_minute % MINUTES_PER_DAY;
                let end = *end_minute % MINUTES_PER_DAY;

                if start < end {
                    days.contains(&day) && minute >= start && minute < end
                } else {
                    // Fenêtre à cheval sur minuit : elle appartient au jour de son
                    // début, la partie après minuit s'évalue donc sur la veille.
                    let previous_day = (day + 6) % 7;
                    (days.contains(&day) && minute >= start)
                        || (days.contains(&previous_day) && minute < end)
                }
            }
        }
    }

    /// Vrai si la fenêtre est déjà entièrement derrière nous, et le silence purgeable.
    pub fn is_expired(&self, now: DateTime<Utc>) -> bool {
        match self {
            Self::Once { ends_at, .. } => now >= *ends_at,
            Self::Weekly { .. } => false,
        }
    }
}

/// Une fenêtre de maintenance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Silence {
    pub id: i64,
    pub name: String,
    #[serde(default)]
    pub comment: String,
    /// Cible visée. `None` avec des `matchers` vides signifie « toute l'instance ».
    pub target_id: Option<TargetId>,
    /// Correspondance exacte sur les étiquettes de la série. Toutes doivent
    /// correspondre — un silence trop large est plus dangereux qu'un silence trop
    /// étroit, puisqu'il fait disparaître des alertes sans laisser de trace visible.
    #[serde(default)]
    pub matchers: BTreeMap<String, String>,
    pub schedule: Schedule,
    pub enabled: bool,
}

impl Silence {
    pub fn matches(
        &self,
        now: DateTime<Utc>,
        target: Option<TargetId>,
        labels: &BTreeMap<String, String>,
    ) -> bool {
        if !self.enabled || !self.schedule.covers(now) {
            return false;
        }
        if let Some(wanted) = self.target_id
            && target != Some(wanted)
        {
            return false;
        }
        self.matchers.iter().all(|(key, value)| labels.get(key) == Some(value))
    }
}

/// Premier silence couvrant l'alerte, s'il en existe un.
///
/// Les fenêtres peuvent se chevaucher sans conséquence : il suffit qu'une seule
/// couvre l'instant présent pour se taire, et le chevauchement est le cas normal
/// quand une maintenance ponctuelle tombe pendant la fenêtre hebdomadaire.
pub fn first_match<'a>(
    silences: &'a [Silence],
    now: DateTime<Utc>,
    target: Option<TargetId>,
    labels: &BTreeMap<String, String>,
) -> Option<&'a Silence> {
    silences.iter().find(|silence| silence.matches(now, target, labels))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(iso: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(iso).expect("valid ISO timestamp").with_timezone(&Utc)
    }

    fn labels(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect()
    }

    fn once(id: i64, start: &str, end: &str) -> Silence {
        Silence {
            id,
            name: format!("maintenance-{id}"),
            comment: String::new(),
            target_id: None,
            matchers: BTreeMap::new(),
            schedule: Schedule::Once { starts_at: at(start), ends_at: at(end) },
            enabled: true,
        }
    }

    #[test]
    fn une_fenetre_ponctuelle_est_inclusive_a_gauche_exclusive_a_droite() {
        let schedule = Schedule::Once {
            starts_at: at("2026-03-01T22:00:00Z"),
            ends_at: at("2026-03-02T02:00:00Z"),
        };
        assert!(!schedule.covers(at("2026-03-01T21:59:59Z")));
        assert!(schedule.covers(at("2026-03-01T22:00:00Z")));
        assert!(schedule.covers(at("2026-03-02T01:59:59Z")));
        assert!(!schedule.covers(at("2026-03-02T02:00:00Z")));
    }

    #[test]
    fn une_fenetre_hebdomadaire_ne_couvre_que_ses_jours() {
        // Dimanche (6) de 2 h à 4 h, heure locale UTC+1.
        let schedule = Schedule::Weekly {
            days: vec![6],
            start_minute: 120,
            end_minute: 240,
            utc_offset_minutes: 60,
        };
        // 2026-03-01 est un dimanche. 01:30 UTC = 02:30 locales.
        assert!(schedule.covers(at("2026-03-01T01:30:00Z")));
        assert!(!schedule.covers(at("2026-03-01T03:30:00Z")), "04:30 local: too late");
        assert!(!schedule.covers(at("2026-03-02T01:30:00Z")), "Monday: wrong day");
    }

    #[test]
    fn une_fenetre_hebdomadaire_a_cheval_sur_minuit_deborde_sur_le_lendemain() {
        // Samedi (5) de 23 h à 1 h, en UTC pour simplifier la lecture.
        let schedule = Schedule::Weekly {
            days: vec![5],
            start_minute: 23 * 60,
            end_minute: 60,
            utc_offset_minutes: 0,
        };
        // 2026-02-28 est un samedi.
        assert!(schedule.covers(at("2026-02-28T23:30:00Z")), "Saturday evening");
        assert!(schedule.covers(at("2026-03-01T00:30:00Z")), "early Sunday morning");
        assert!(!schedule.covers(at("2026-03-01T01:30:00Z")), "past the window");
        assert!(!schedule.covers(at("2026-02-28T22:30:00Z")), "before the window");
    }

    #[test]
    fn des_fenetres_qui_se_chevauchent_font_taire_une_seule_fois() {
        let silences = vec![
            once(1, "2026-03-01T20:00:00Z", "2026-03-01T23:00:00Z"),
            once(2, "2026-03-01T22:00:00Z", "2026-03-02T02:00:00Z"),
        ];
        // Dans le chevauchement, c'est le premier déclaré qui explique le silence.
        assert_eq!(
            first_match(&silences, at("2026-03-01T22:30:00Z"), None, &labels(&[])).map(|s| s.id),
            Some(1)
        );
        // Hors du premier mais dans le second, la couverture reste continue.
        assert_eq!(
            first_match(&silences, at("2026-03-02T01:00:00Z"), None, &labels(&[])).map(|s| s.id),
            Some(2)
        );
        // Après les deux, plus rien.
        assert!(first_match(&silences, at("2026-03-02T02:00:00Z"), None, &labels(&[])).is_none());
    }

    #[test]
    fn un_silence_cible_ne_deborde_pas_sur_les_autres_cibles() {
        let mut silence = once(1, "2026-03-01T00:00:00Z", "2026-03-02T00:00:00Z");
        silence.target_id = Some(7);
        let now = at("2026-03-01T12:00:00Z");
        assert!(silence.matches(now, Some(7), &labels(&[])));
        assert!(!silence.matches(now, Some(8), &labels(&[])));
        assert!(!silence.matches(now, None, &labels(&[])));
    }

    #[test]
    fn un_silence_par_etiquettes_exige_toutes_les_correspondances() {
        let mut silence = once(1, "2026-03-01T00:00:00Z", "2026-03-02T00:00:00Z");
        silence.matchers = labels(&[("tag_role", "lab"), ("__name__", "ezymonit_up")]);
        let now = at("2026-03-01T12:00:00Z");
        assert!(silence.matches(
            now,
            None,
            &labels(&[("tag_role", "lab"), ("__name__", "ezymonit_up"), ("host", "x")])
        ));
        assert!(!silence.matches(now, None, &labels(&[("tag_role", "lab")])));
    }

    #[test]
    fn un_silence_desactive_ne_fait_plus_taire() {
        let mut silence = once(1, "2026-03-01T00:00:00Z", "2026-03-02T00:00:00Z");
        silence.enabled = false;
        assert!(!silence.matches(at("2026-03-01T12:00:00Z"), None, &labels(&[])));
    }

    #[test]
    fn seules_les_fenetres_ponctuelles_expirent() {
        let ponctuelle = Schedule::Once {
            starts_at: at("2026-03-01T00:00:00Z"),
            ends_at: at("2026-03-01T01:00:00Z"),
        };
        assert!(ponctuelle.is_expired(at("2026-03-01T01:00:00Z")));
        assert!(!ponctuelle.is_expired(at("2026-03-01T00:30:00Z")));

        let recurrente = Schedule::Weekly {
            days: vec![0],
            start_minute: 0,
            end_minute: 60,
            utc_offset_minutes: 0,
        };
        assert!(!recurrente.is_expired(at("2030-01-01T00:00:00Z")));
    }
}
