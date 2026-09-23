//! Silences : fenêtres de maintenance pendant lesquelles on n'envoie rien.
//!
//! Un silence n'interrompt pas l'évaluation : la machine à états continue de tourner
//! derrière. C'est ce qui permet, à la fin d'une maintenance, de constater qu'une
//! alerte est toujours active et de la notifier une fois, plutôt que de la voir
//! « naître » alors qu'elle dure depuis deux heures.
//!
//! Une fenêtre récurrente se lit toujours de la même façon : une date locale
//! retenue par la récurrence, une heure de début à l'horloge murale, puis une
//! durée qui s'écoule en temps absolu. C'est ce couple « heure locale + durée »
//! qui traverse les changements d'heure sans dériver : « tous les dimanches de
//! 2 h à 4 h à Paris » reste à 2 h locales toute l'année, et dure deux heures
//! pleines même la nuit où l'horloge recule.

use std::collections::BTreeMap;
use std::str::FromStr;

use chrono::{DateTime, Datelike, Days, FixedOffset, NaiveDate, TimeDelta, TimeZone, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};

use crate::alerting::model::TargetId;

/// Nombre de minutes dans une journée, borne haute d'une heure de début.
pub const MINUTES_PER_DAY: u32 = 24 * 60;

/// Durée maximale d'une fenêtre récurrente : trente jours. Au-delà, ce n'est
/// plus une maintenance, c'est une règle à désactiver.
pub const MAX_DURATION_MINUTES: u32 = 30 * MINUTES_PER_DAY;

/// Horizon de recherche de la prochaine occurrence, en jours. Une fenêtre
/// « 5ᵉ dimanche de février » peut sauter plusieurs mois ; un peu plus d'un an
/// suffit à en trouver une, et borne la boucle si la récurrence est vide.
const NEXT_SEARCH_DAYS: u64 = 400;

/// Rang d'un jour de la semaine dans le mois : 1 à 5, ou -1 pour « le dernier ».
///
/// « Le premier dimanche » et « le dernier vendredi » sont la façon dont les
/// calendriers de maintenance s'écrivent réellement ; un cinquième dimanche
/// n'existe pas tous les mois, et le mois sans occurrence est alors simplement
/// sauté.
pub const LAST_OF_MONTH: i8 = -1;

/// « Le n-ième `weekday` du mois ».
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct NthWeekday {
    /// 1..=5, ou [`LAST_OF_MONTH`].
    pub nth: i8,
    /// 0 = lundi … 6 = dimanche, comme partout ailleurs dans le module.
    pub weekday: u8,
}

/// Horloge d'une fenêtre récurrente : un fuseau nommé, ou un décalage fixe.
///
/// Le décalage fixe est ce que stockaient les fenêtres d'avant les fuseaux
/// nommés ; il reste pris en charge tel quel, et se comporte exactement comme
/// avant puisqu'un décalage fixe ignore, par construction, les changements
/// d'heure.
#[derive(Debug, Clone, Copy)]
enum Clock {
    Named(Tz),
    Fixed(FixedOffset),
}

impl Clock {
    fn resolve(timezone: Option<&str>, offset_minutes: i32) -> Self {
        if let Some(name) = timezone.map(str::trim).filter(|name| !name.is_empty())
            && let Ok(tz) = Tz::from_str(name)
        {
            return Self::Named(tz);
        }
        let seconds = offset_minutes.clamp(-14 * 60, 14 * 60) * 60;
        Self::Fixed(
            FixedOffset::east_opt(seconds)
                .unwrap_or_else(|| FixedOffset::east_opt(0).expect("UTC is a valid fixed offset")),
        )
    }

    /// Date locale de `now` dans cette horloge.
    fn date_of(self, now: DateTime<Utc>) -> NaiveDate {
        match self {
            Self::Named(tz) => now.with_timezone(&tz).date_naive(),
            Self::Fixed(offset) => now.with_timezone(&offset).date_naive(),
        }
    }

    /// Instant absolu où l'horloge murale locale marque `minute` ce jour-là.
    ///
    /// Deux cas particuliers, et un seul choix raisonnable pour chacun : l'heure
    /// qui n'existe pas (passage à l'heure d'été) ouvre la fenêtre dès que
    /// l'horloge repart, l'heure qui existe deux fois (passage à l'heure d'hiver)
    /// l'ouvre à sa première occurrence.
    fn instant_at(self, date: NaiveDate, minute: u32) -> Option<DateTime<Utc>> {
        let minute = minute % MINUTES_PER_DAY;
        let naive = date.and_hms_opt(minute / 60, minute % 60, 0)?;
        match self {
            Self::Fixed(offset) => {
                offset.from_local_datetime(&naive).earliest().map(|at| at.with_timezone(&Utc))
            }
            Self::Named(tz) => tz
                .from_local_datetime(&naive)
                .earliest()
                .or_else(|| {
                    (1..=4).find_map(|hours| {
                        tz.from_local_datetime(&(naive + TimeDelta::hours(hours))).earliest()
                    })
                })
                .map(|at| at.with_timezone(&Utc)),
        }
    }
}

/// Planification d'un silence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Schedule {
    /// Fenêtre ponctuelle : bornes absolues, inclusive à gauche, exclusive à droite.
    Once { starts_at: DateTime<Utc>, ends_at: DateTime<Utc> },
    /// Fenêtre hebdomadaire, exprimée dans le fuseau de l'utilisateur.
    ///
    /// Le fuseau est porté par le silence lui-même plutôt que par une configuration
    /// globale : « tous les dimanches de 2 h à 4 h » doit rester à 2 h locales même
    /// après un changement d'heure, et c'est aussi ce que l'utilisateur a saisi.
    Weekly {
        /// Jours concernés, 0 = lundi … 6 = dimanche.
        days: Vec<u8>,
        /// Minute de début dans la journée locale, 0..1440.
        start_minute: u32,
        /// Minute de fin. Si elle est inférieure ou égale au début, la fenêtre
        /// déborde sur le lendemain (« 23 h → 1 h »). Elle ne sert qu'à dire la
        /// durée : la fenêtre s'ouvre à `start_minute` locales et dure
        /// `end_minute - start_minute` minutes pleines.
        end_minute: u32,
        /// Décalage fixe, pour les fenêtres posées avant les fuseaux nommés.
        #[serde(default)]
        utc_offset_minutes: i32,
        /// Fuseau IANA (« Europe/Paris »). Prioritaire sur le décalage fixe : lui
        /// seul sait où tombent les changements d'heure.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timezone: Option<String>,
    },
    /// Fenêtre mensuelle : des quantièmes, et/ou des « n-ièmes jours de la semaine ».
    ///
    /// Les deux listes s'additionnent (OU) : « le 1er du mois, et le premier
    /// dimanche » est une seule fenêtre. Un mois qui ne contient aucune
    /// occurrence — pas de 31, pas de cinquième dimanche — est simplement sauté.
    Monthly {
        /// Quantièmes, 1..=31.
        #[serde(default)]
        days: Vec<u8>,
        /// « Premier dimanche », « dernier vendredi »…
        #[serde(default)]
        nth_weekdays: Vec<NthWeekday>,
        /// Minute de début dans la journée locale, 0..1440.
        start_minute: u32,
        /// Durée de la fenêtre, en minutes de temps réel. Une maintenance
        /// mensuelle peut durer plus d'une journée, ce qu'une heure de fin ne
        /// saurait dire.
        duration_minutes: u32,
        #[serde(default)]
        utc_offset_minutes: i32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timezone: Option<String>,
    },
}

impl Schedule {
    /// Horloge de la fenêtre, `None` pour une fenêtre ponctuelle (déjà absolue).
    fn clock(&self) -> Option<Clock> {
        match self {
            Self::Once { .. } => None,
            Self::Weekly { utc_offset_minutes, timezone, .. }
            | Self::Monthly { utc_offset_minutes, timezone, .. } => {
                Some(Clock::resolve(timezone.as_deref(), *utc_offset_minutes))
            }
        }
    }

    /// Nom du fuseau tel qu'il a été saisi, s'il est valide.
    pub fn timezone(&self) -> Option<&str> {
        match self {
            Self::Once { .. } => None,
            Self::Weekly { timezone, .. } | Self::Monthly { timezone, .. } => timezone.as_deref(),
        }
    }

    /// Durée d'une occurrence, en temps absolu.
    ///
    /// C'est ici que se joue la robustesse aux changements d'heure : une fenêtre
    /// de deux heures dure deux heures, même la nuit où l'horloge locale recule
    /// et où « de 2 h à 4 h » couvrirait trois heures réelles.
    pub fn duration(&self) -> TimeDelta {
        match self {
            Self::Once { starts_at, ends_at } => *ends_at - *starts_at,
            Self::Weekly { start_minute, end_minute, .. } => {
                let start = start_minute % MINUTES_PER_DAY;
                let end = end_minute % MINUTES_PER_DAY;
                let minutes = if end > start { end - start } else { MINUTES_PER_DAY - start + end };
                TimeDelta::minutes(i64::from(minutes))
            }
            Self::Monthly { duration_minutes, .. } => {
                TimeDelta::minutes(i64::from((*duration_minutes).max(1)))
            }
        }
    }

    /// Minute locale d'ouverture, pour les fenêtres récurrentes.
    fn start_minute(&self) -> u32 {
        match self {
            Self::Once { .. } => 0,
            Self::Weekly { start_minute, .. } | Self::Monthly { start_minute, .. } => {
                start_minute % MINUTES_PER_DAY
            }
        }
    }

    /// Vrai si la récurrence retient cette date locale.
    fn selects(&self, date: NaiveDate) -> bool {
        match self {
            Self::Once { .. } => false,
            Self::Weekly { days, .. } => {
                days.contains(&(date.weekday().num_days_from_monday() as u8))
            }
            Self::Monthly { days, nth_weekdays, .. } => {
                let day = date.day();
                days.iter().any(|wanted| u32::from(*wanted) == day)
                    || nth_weekdays.iter().any(|nth| nth_matches(date, *nth))
            }
        }
    }

    /// Vrai si `now` tombe dans la fenêtre.
    pub fn covers(&self, now: DateTime<Utc>) -> bool {
        self.window_containing(now).is_some()
    }

    /// Bornes de l'occurrence qui couvre `now`, s'il y en a une.
    ///
    /// Les pages d'état publiques s'en servent pour dire jusqu'à quand la
    /// maintenance est prévue, plutôt que d'afficher un rouge que personne ne
    /// doit traiter.
    pub fn window_containing(&self, now: DateTime<Utc>) -> Option<(DateTime<Utc>, DateTime<Utc>)> {
        if let Self::Once { starts_at, ends_at } = self {
            return (now >= *starts_at && now < *ends_at).then_some((*starts_at, *ends_at));
        }
        let clock = self.clock()?;
        let duration = self.duration();
        let start_minute = self.start_minute();
        // Une fenêtre longue a pu s'ouvrir plusieurs jours plus tôt : on remonte
        // d'autant de jours que la durée en compte, plus un pour le débordement
        // de minuit.
        let back = duration.num_days().max(0) as u64 + 1;
        let today = clock.date_of(now);
        for offset in 0..=back {
            let Some(date) = today.checked_sub_days(Days::new(offset)) else { continue };
            if !self.selects(date) {
                continue;
            }
            let Some(start) = clock.instant_at(date, start_minute) else { continue };
            let end = start + duration;
            if now >= start && now < end {
                return Some((start, end));
            }
        }
        None
    }

    /// Début de la prochaine occurrence à venir, hors de celle en cours.
    pub fn next_start(&self, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
        if let Self::Once { starts_at, .. } = self {
            return (*starts_at > now).then_some(*starts_at);
        }
        let clock = self.clock()?;
        let start_minute = self.start_minute();
        let today = clock.date_of(now);
        for offset in 0..=NEXT_SEARCH_DAYS {
            let Some(date) = today.checked_add_days(Days::new(offset)) else { break };
            if !self.selects(date) {
                continue;
            }
            let Some(start) = clock.instant_at(date, start_minute) else { continue };
            if start > now {
                return Some(start);
            }
        }
        None
    }

    /// Vrai si la fenêtre est déjà entièrement derrière nous, et le silence purgeable.
    pub fn is_expired(&self, now: DateTime<Utc>) -> bool {
        match self {
            Self::Once { ends_at, .. } => now >= *ends_at,
            Self::Weekly { .. } | Self::Monthly { .. } => false,
        }
    }
}

/// Vrai si `date` est le n-ième jour de la semaine demandé, dans son mois.
fn nth_matches(date: NaiveDate, nth: NthWeekday) -> bool {
    if date.weekday().num_days_from_monday() as u8 != nth.weekday {
        return false;
    }
    if nth.nth > 0 {
        i8::try_from((date.day() - 1) / 7 + 1).is_ok_and(|rank| rank == nth.nth)
    } else {
        // « Le dernier » : il n'y a pas de même jour sept jours plus tard dans
        // le mois.
        date.checked_add_days(Days::new(7)).is_none_or(|next| next.month() != date.month())
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

    /// « Tous les dimanches de 2 h à 4 h », dans le fuseau donné.
    fn sunday_two_to_four(timezone: Option<&str>, offset: i32) -> Schedule {
        Schedule::Weekly {
            days: vec![6],
            start_minute: 120,
            end_minute: 240,
            utc_offset_minutes: offset,
            timezone: timezone.map(str::to_string),
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
        let schedule = sunday_two_to_four(None, 60);
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
            timezone: None,
        };
        // 2026-02-28 est un samedi.
        assert!(schedule.covers(at("2026-02-28T23:30:00Z")), "Saturday evening");
        assert!(schedule.covers(at("2026-03-01T00:30:00Z")), "early Sunday morning");
        assert!(!schedule.covers(at("2026-03-01T01:30:00Z")), "past the window");
        assert!(!schedule.covers(at("2026-02-28T22:30:00Z")), "before the window");
    }

    // ----------------------------------------------------------------------
    // Fuseaux nommés et changements d'heure
    // ----------------------------------------------------------------------

    #[test]
    fn une_fenetre_hebdomadaire_en_fuseau_nomme_reste_a_l_heure_locale() {
        let schedule = sunday_two_to_four(Some("Europe/Paris"), 0);

        // Heure d'hiver (UTC+1) : 2 h locales = 01:00 UTC.
        assert!(!schedule.covers(at("2026-01-04T00:59:00Z")));
        assert!(schedule.covers(at("2026-01-04T01:00:00Z")), "02:00 Paris, winter");
        assert!(schedule.covers(at("2026-01-04T02:59:00Z")));
        assert!(!schedule.covers(at("2026-01-04T03:00:00Z")), "04:00 Paris, winter");

        // Heure d'été (UTC+2) : 2 h locales = 00:00 UTC. Aucune dérive : c'est
        // toujours 2 h à l'horloge du mur.
        assert!(!schedule.covers(at("2026-06-06T23:59:00Z")), "01:59 Paris, just before");
        assert!(schedule.covers(at("2026-06-07T00:00:00Z")), "02:00 Paris, summer");
        assert!(schedule.covers(at("2026-06-07T01:59:00Z")));
        assert!(!schedule.covers(at("2026-06-07T02:00:00Z")), "04:00 Paris, summer");
    }

    #[test]
    fn un_decalage_fixe_derive_la_ou_un_fuseau_nomme_tient() {
        // Le même « dimanche 2 h–4 h », une fois en UTC+1 fixe, une fois à Paris.
        let fixe = sunday_two_to_four(None, 60);
        let nomme = sunday_two_to_four(Some("Europe/Paris"), 60);
        // 2026-06-07, 00:30 UTC : 02:30 à Paris (heure d'été), 01:30 pour le
        // décalage fixe. C'est exactement la dérive de deux fois par an que le
        // fuseau nommé supprime.
        let ete = at("2026-06-07T00:30:00Z");
        assert!(nomme.covers(ete), "02:30 in Paris: inside the window");
        assert!(!fixe.covers(ete), "a fixed offset drifts with the seasons");
    }

    #[test]
    fn la_nuit_du_passage_a_l_heure_d_ete_la_fenetre_ouvre_des_que_l_horloge_repart() {
        // 2026-03-29, Paris : à 02:00 locales l'horloge saute à 03:00. Une
        // fenêtre « 2 h, deux heures » n'a pas de 2 h ce jour-là.
        let schedule = sunday_two_to_four(Some("Europe/Paris"), 0);
        // 01:00 UTC = 03:00 locales : l'horloge vient de repartir.
        assert!(schedule.covers(at("2026-03-29T01:00:00Z")), "the clock jumped to 03:00");
        assert!(!schedule.covers(at("2026-03-29T00:59:00Z")), "still 01:59 local");
        // Et elle dure ses deux heures pleines, pas une de plus.
        assert!(schedule.covers(at("2026-03-29T02:59:00Z")));
        assert!(!schedule.covers(at("2026-03-29T03:00:00Z")));
    }

    #[test]
    fn la_nuit_du_passage_a_l_heure_d_hiver_la_fenetre_dure_deux_heures_pleines() {
        // 2026-10-25, Paris : à 03:00 locales l'horloge recule à 02:00, si bien
        // que « de 2 h à 4 h » à l'horloge murale durerait trois heures. La
        // fenêtre s'ouvre à la première occurrence de 2 h et dure deux heures.
        let schedule = sunday_two_to_four(Some("Europe/Paris"), 0);
        let start = at("2026-10-25T00:00:00Z"); // 02:00 CEST
        assert!(schedule.covers(start));
        assert!(schedule.covers(at("2026-10-25T01:59:00Z")));
        // 02:00 UTC = 03:00 CET : deux heures se sont écoulées, c'est fini.
        assert!(!schedule.covers(at("2026-10-25T02:00:00Z")), "exactly two hours, no drift");
        let (opened, closed) = schedule.window_containing(start).expect("window");
        assert_eq!(closed - opened, TimeDelta::hours(2));
    }

    #[test]
    fn un_fuseau_inconnu_retombe_sur_le_decalage_fixe() {
        // Une base restaurée d'ailleurs, un nom mal orthographié : la fenêtre
        // reste posée, elle ne devient pas permanente ni muette.
        let schedule = sunday_two_to_four(Some("Mars/Olympus"), 60);
        assert!(schedule.covers(at("2026-03-01T01:30:00Z")), "falls back to UTC+1");
    }

    // ----------------------------------------------------------------------
    // Récurrence mensuelle
    // ----------------------------------------------------------------------

    fn monthly(days: &[u8], nth: &[(i8, u8)], start: u32, duration: u32) -> Schedule {
        Schedule::Monthly {
            days: days.to_vec(),
            nth_weekdays: nth
                .iter()
                .map(|(nth, weekday)| NthWeekday { nth: *nth, weekday: *weekday })
                .collect(),
            start_minute: start,
            duration_minutes: duration,
            utc_offset_minutes: 0,
            timezone: None,
        }
    }

    #[test]
    fn une_fenetre_mensuelle_par_quantieme_ne_couvre_que_ce_jour() {
        // Le 1er de chaque mois, de 2 h à 4 h UTC.
        let schedule = monthly(&[1], &[], 120, 120);
        assert!(schedule.covers(at("2026-03-01T02:30:00Z")));
        assert!(schedule.covers(at("2026-04-01T02:30:00Z")));
        assert!(!schedule.covers(at("2026-03-02T02:30:00Z")), "the second is not the first");
        assert!(!schedule.covers(at("2026-03-01T04:30:00Z")), "past the window");
    }

    #[test]
    fn un_quantieme_absent_du_mois_saute_le_mois() {
        // Le 31, de 2 h à 3 h. Février n'en a pas.
        let schedule = monthly(&[31], &[], 120, 60);
        assert!(schedule.covers(at("2026-01-31T02:30:00Z")));
        assert!(!schedule.covers(at("2026-02-28T02:30:00Z")), "February has no 31st");
        assert!(schedule.covers(at("2026-03-31T02:30:00Z")));
    }

    #[test]
    fn le_premier_dimanche_du_mois_est_reconnu() {
        // Premier dimanche (nth 1, jour 6), 2 h → 4 h.
        let schedule = monthly(&[], &[(1, 6)], 120, 120);
        // 2026-03-01 est un dimanche, et c'est le premier du mois.
        assert!(schedule.covers(at("2026-03-01T02:30:00Z")));
        // 2026-03-08 est le deuxième.
        assert!(!schedule.covers(at("2026-03-08T02:30:00Z")));
        // 2026-04-05 est le premier dimanche d'avril.
        assert!(schedule.covers(at("2026-04-05T02:30:00Z")));
    }

    #[test]
    fn un_mois_sans_cinquieme_dimanche_est_saute() {
        let schedule = monthly(&[], &[(5, 6)], 120, 120);
        // Mars 2026 : dimanches les 1, 8, 15, 22 et 29 — il y en a cinq.
        assert!(schedule.covers(at("2026-03-29T02:30:00Z")), "March has a fifth Sunday");
        // Avril 2026 : dimanches les 5, 12, 19 et 26 — pas de cinquième.
        for day in ["05", "12", "19", "26"] {
            let instant = at(&format!("2026-04-{day}T02:30:00Z"));
            assert!(!schedule.covers(instant), "April has no fifth Sunday ({day})");
        }
        // Et la prochaine occurrence saute directement à mai.
        let next = schedule.next_start(at("2026-04-01T00:00:00Z")).expect("a next window");
        assert_eq!(next, at("2026-05-31T02:00:00Z"), "the fifth Sunday of May");
    }

    #[test]
    fn le_dernier_vendredi_du_mois_est_reconnu() {
        // Dernier vendredi (jour 4), 22 h, six heures : la fenêtre déborde sur
        // le samedi, ce qu'une heure de fin ne saurait dire.
        let schedule = monthly(&[], &[(LAST_OF_MONTH, 4)], 22 * 60, 6 * 60);
        // 2026-03-27 est le dernier vendredi de mars.
        assert!(schedule.covers(at("2026-03-27T23:00:00Z")));
        assert!(schedule.covers(at("2026-03-28T03:00:00Z")), "still inside, next day");
        assert!(!schedule.covers(at("2026-03-28T04:30:00Z")), "six hours and it is over");
        // 2026-03-20 est un vendredi, mais pas le dernier.
        assert!(!schedule.covers(at("2026-03-20T23:00:00Z")));
    }

    #[test]
    fn une_fenetre_mensuelle_longue_reste_ouverte_plusieurs_jours() {
        // Le 1er, pendant trois jours.
        let schedule = monthly(&[1], &[], 0, 3 * 24 * 60);
        assert!(schedule.covers(at("2026-03-01T00:00:00Z")));
        assert!(schedule.covers(at("2026-03-03T23:59:00Z")));
        assert!(!schedule.covers(at("2026-03-04T00:00:00Z")));
    }

    #[test]
    fn la_prochaine_occurrence_est_annoncee() {
        let schedule = sunday_two_to_four(Some("Europe/Paris"), 0);
        // Jeudi 2026-01-01 : le prochain dimanche est le 4, à 02:00 Paris = 01:00 UTC.
        let next = schedule.next_start(at("2026-01-01T12:00:00Z")).expect("a next window");
        assert_eq!(next, at("2026-01-04T01:00:00Z"));
        // Une fenêtre ponctuelle déjà passée n'en a plus.
        let past = Schedule::Once {
            starts_at: at("2020-01-01T00:00:00Z"),
            ends_at: at("2020-01-01T01:00:00Z"),
        };
        assert!(past.next_start(at("2026-01-01T00:00:00Z")).is_none());
    }

    // ----------------------------------------------------------------------
    // Compatibilité des lignes déjà en base
    // ----------------------------------------------------------------------

    #[test]
    fn une_fenetre_hebdomadaire_enregistree_avant_les_fuseaux_se_relit_telle_quelle() {
        // Exactement le JSON qu'écrivaient les versions précédentes.
        let raw = r#"{"kind":"weekly","days":[6],"start_minute":120,"end_minute":240,
                      "utc_offset_minutes":60}"#;
        let schedule: Schedule = serde_json::from_str(raw).expect("old rows must still load");
        assert_eq!(schedule, sunday_two_to_four(None, 60));
        assert!(schedule.covers(at("2026-03-01T01:30:00Z")), "same verdict as before");
        // Et se réécrit sans champ surnuméraire.
        let json = serde_json::to_string(&schedule).expect("serialisable");
        assert!(!json.contains("timezone"), "no empty timezone added: {json}");
    }

    #[test]
    fn une_fenetre_ponctuelle_enregistree_se_relit_telle_quelle() {
        let raw = r#"{"kind":"once","starts_at":"2026-03-01T22:00:00Z",
                      "ends_at":"2026-03-02T02:00:00Z"}"#;
        let schedule: Schedule = serde_json::from_str(raw).expect("old rows must still load");
        assert!(schedule.covers(at("2026-03-01T23:00:00Z")));
        assert!(schedule.is_expired(at("2026-03-03T00:00:00Z")));
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
        silence.matchers = labels(&[("tag_role", "lab"), ("__name__", "dumbmonit_up")]);
        let now = at("2026-03-01T12:00:00Z");
        assert!(silence.matches(
            now,
            None,
            &labels(&[("tag_role", "lab"), ("__name__", "dumbmonit_up"), ("host", "x")])
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
            timezone: None,
        };
        assert!(!recurrente.is_expired(at("2030-01-01T00:00:00Z")));
        assert!(!monthly(&[1], &[], 0, 60).is_expired(at("2030-01-01T00:00:00Z")));
    }
}
