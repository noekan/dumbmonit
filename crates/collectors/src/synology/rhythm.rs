//! Rythme des sauvegardes Active Backup for Business, appareil par appareil.
//!
//! Une règle fixe « sauvegarde plus vieille que 24 h » ne vaut rien pour un
//! portable : son propriétaire l'éteint le soir, ne l'allume pas le week-end, et
//! chaque lundi matin la règle sonnerait pour rien. Ce module apprend donc à
//! chaque appareil son propre rythme, à partir de ses trente derniers jours
//! d'exécutions, et n'en déduit une attente que relative à ce rythme.
//!
//! Tout est fonctionnel : [`assess`] reçoit les exécutions, l'heure et le décalage
//! horaire, et rend un verdict. Ni base, ni horloge, ni réseau — chaque cas se
//! teste avec un historique synthétique.
//!
//! # Ce qui est calculé
//!
//! * les **jours actifs** : les jours de la semaine où l'appareil a tenté au moins
//!   une sauvegarde, réussie ou non — un échec prouve autant qu'une réussite que
//!   la machine était allumée. Un jour sans aucune tentative sur au moins deux
//!   semaines d'historique est un **jour de repos** ;
//! * l'**intervalle habituel** entre deux réussites : la médiane et le neuvième
//!   décile des écarts, comptés en **temps actif** — les jours de repos entiers
//!   qu'un écart contient n'y figurent pas (le vendredi → lundi d'un portable de
//!   bureau vaut un jour, pas trois) ;
//! * l'**heure habituelle** : l'heure locale la plus fréquente des lancements ;
//! * la **tolérance** : `max(p90 × 1,5 ; 2 × médiane ; 36 h)`. Tant que le rythme
//!   est inconnu (moins de deux écarts exploitables), la tolérance est de trois
//!   jours, ce qui couvre un week-end ;
//! * le **temps actif écoulé** depuis la dernière réussite : le temps réel moins
//!   les jours de repos entiers qu'il contient.
//!
//! # L'état qui en découle
//!
//! Par ordre de priorité : `running` (une exécution est en cours), `failing`
//! (les [`FAILING_STREAK`] dernières tentatives ont échoué — une annulation ne
//! compte pas, un portable refermé en pleine sauvegarde n'est pas en panne),
//! `never` (aucune réussite connue), `overdue` (le temps actif écoulé dépasse la
//! tolérance), `idle` (jour de repos habituel : éteint, comme d'habitude),
//! `learning` (rythme pas encore connu), `ok`.

use serde::Serialize;

/// Fenêtre d'apprentissage, en jours.
pub const WINDOW_DAYS: i64 = 30;
const DAY: i64 = 86_400;
const HOUR: i64 = 3_600;
/// Plancher de la tolérance : une nuit ratée plus la marge d'une demi-journée.
pub const MIN_ALLOWANCE_S: i64 = 36 * HOUR;
/// Tolérance tant que le rythme est inconnu : trois jours, de quoi passer un
/// week-end sans alerter sur un appareil qu'on vient d'ajouter.
pub const LEARNING_ALLOWANCE_S: i64 = 72 * HOUR;
/// Échecs consécutifs à partir desquels un appareil est « en échec ».
pub const FAILING_STREAK: u32 = 2;
/// Historique minimal, en jours, pour oser déclarer un jour de repos.
const OFF_DAY_EVIDENCE_DAYS: i64 = 14;
/// Une exécution sans fin commencée depuis plus longtemps est réputée abandonnée.
const RUNNING_MAX_S: i64 = 24 * HOUR;

const WEEKDAYS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];

/// Une exécution pour un appareil, telle qu'ABB la rapporte.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    pub start_s: i64,
    /// 0 tant que l'exécution est en cours.
    pub end_s: i64,
    /// 2 réussite, 3 réussite partielle, 4 échec, 5 annulation, 6 sans sauvegarde.
    pub status: i64,
}

/// Lecture d'un code de résultat.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Success,
    Partial,
    Failed,
    Cancelled,
    NoBackup,
    Running,
    Unknown,
}

impl Outcome {
    pub fn label(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Partial => "partial_success",
            Self::Failed => "fail",
            Self::Cancelled => "cancel",
            Self::NoBackup => "no_backup",
            Self::Running => "running",
            Self::Unknown => "unknown",
        }
    }

    /// Une réussite partielle est un échec : une partie des données manque.
    fn is_failure(self) -> bool {
        matches!(self, Self::Partial | Self::Failed)
    }
}

impl Run {
    pub fn outcome(&self) -> Outcome {
        if self.end_s <= 0 {
            return Outcome::Running;
        }
        match self.status {
            2 => Outcome::Success,
            3 => Outcome::Partial,
            4 => Outcome::Failed,
            5 => Outcome::Cancelled,
            6 => Outcome::NoBackup,
            _ => Outcome::Unknown,
        }
    }

    /// L'instant qui date l'exécution : sa fin, ou son début tant qu'elle dure.
    fn at(&self) -> i64 {
        if self.end_s > 0 { self.end_s } else { self.start_s }
    }
}

/// Ce que l'utilisateur lit ; le code numérique est ce que la métrique porte.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Ok,
    Idle,
    Learning,
    Running,
    Overdue,
    Failing,
    Never,
}

impl State {
    pub fn label(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Idle => "idle",
            Self::Learning => "learning",
            Self::Running => "running",
            Self::Overdue => "overdue",
            Self::Failing => "failing",
            Self::Never => "never",
        }
    }

    /// Valeur de `abb_device_state` : croissante avec la gravité.
    pub fn code(self) -> f64 {
        match self {
            Self::Ok => 0.0,
            Self::Idle => 1.0,
            Self::Learning => 2.0,
            Self::Running => 3.0,
            Self::Overdue => 4.0,
            Self::Failing => 5.0,
            Self::Never => 6.0,
        }
    }
}

/// Une case du calendrier des trente derniers jours.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DayCell {
    /// Jour local, `AAAA-MM-JJ`.
    pub day: String,
    /// `success`, `failure`, `cancelled` ou `none` — la meilleure issue du jour.
    pub outcome: &'static str,
    pub runs: u32,
}

/// Le verdict pour un appareil.
#[derive(Debug, Clone, Serialize)]
pub struct Assessment {
    pub state: State,
    pub last_success_s: Option<i64>,
    pub last_run_s: Option<i64>,
    pub last_outcome: Option<&'static str>,
    pub runs_30d: u32,
    pub successes_30d: u32,
    pub failures_30d: u32,
    pub consecutive_failures: u32,
    /// Médiane des écarts entre réussites, en temps actif (jours de repos exclus).
    pub typical_interval_s: Option<i64>,
    pub p90_gap_s: Option<i64>,
    /// Temps actif toléré depuis la dernière réussite avant « en retard ».
    pub allowance_s: i64,
    /// Temps actif écoulé depuis la dernière réussite.
    pub active_elapsed_s: Option<i64>,
    /// Lundi en premier.
    pub active_weekdays: [bool; 7],
    pub usual_hour: Option<u32>,
    /// Le rythme en mots : « weekdays, around 20:00 ».
    pub rhythm: String,
    /// Trente cases, de la plus ancienne à aujourd'hui.
    pub calendar: Vec<DayCell>,
}

fn local_day(ts: i64, offset_s: i64) -> i64 {
    (ts + offset_s).div_euclid(DAY)
}

/// Jour de la semaine d'un jour local, lundi = 0. Le 1er janvier 1970 était un
/// jeudi.
fn weekday(day: i64) -> usize {
    (day + 3).rem_euclid(7) as usize
}

fn local_hour(ts: i64, offset_s: i64) -> u32 {
    ((ts + offset_s).rem_euclid(DAY) / HOUR) as u32
}

fn day_label(day: i64) -> String {
    chrono::DateTime::from_timestamp(day * DAY, 0)
        .map(|date| date.format("%Y-%m-%d").to_string())
        .unwrap_or_default()
}

fn median(sorted: &[i64]) -> i64 {
    let n = sorted.len();
    if n % 2 == 1 { sorted[n / 2] } else { (sorted[n / 2 - 1] + sorted[n / 2]) / 2 }
}

/// Neuvième décile, par rang le plus proche.
fn p90(sorted: &[i64]) -> i64 {
    let rank = ((sorted.len() as f64) * 0.9).ceil() as usize;
    sorted[rank.clamp(1, sorted.len()) - 1]
}

/// Juge un appareil d'après ses exécutions (dans n'importe quel ordre ; les plus
/// anciennes peuvent dépasser la fenêtre, elles ne servent alors qu'à dater la
/// dernière réussite).
pub fn assess(runs: &[Run], now_s: i64, utc_offset_s: i64) -> Assessment {
    let today = local_day(now_s, utc_offset_s);
    let window_start = now_s - WINDOW_DAYS * DAY;

    let mut recent: Vec<&Run> =
        runs.iter().filter(|run| run.at() >= window_start && run.start_s <= now_s).collect();
    recent.sort_by_key(|run| run.at());

    let running = recent
        .iter()
        .any(|run| run.outcome() == Outcome::Running && now_s - run.start_s < RUNNING_MAX_S);

    let last_success_s =
        runs.iter().filter(|run| run.outcome() == Outcome::Success).map(|run| run.end_s).max();
    let last_run = recent.iter().rev().find(|run| run.outcome() != Outcome::Running).copied();

    // Jours actifs et jours de repos.
    let mut active_weekdays = [false; 7];
    let mut first_day = today;
    for run in &recent {
        let day = local_day(run.start_s, utc_offset_s);
        active_weekdays[weekday(day)] = true;
        first_day = first_day.min(day);
    }
    let evidence = today - first_day >= OFF_DAY_EVIDENCE_DAYS;
    let off_day = |day: i64| evidence && !active_weekdays[weekday(day)];

    // Temps actif entre deux instants : le temps réel moins les jours de repos
    // entiers qui les séparent. Le même compte sert aux écarts et au retard, pour
    // que les deux se comparent.
    let active_seconds = |from_s: i64, to_s: i64| -> i64 {
        let (from, to) = (local_day(from_s, utc_offset_s), local_day(to_s, utc_offset_s));
        let off_days = (from + 1..to).filter(|day| off_day(*day)).count() as i64;
        (to_s - from_s - off_days * DAY).max(0)
    };

    // Écarts entre réussites, en temps actif.
    let successes: Vec<i64> = recent
        .iter()
        .filter(|run| run.outcome() == Outcome::Success)
        .map(|run| run.end_s)
        .collect();
    let mut gaps: Vec<i64> = successes
        .windows(2)
        .filter(|pair| pair[1] > pair[0])
        .map(|pair| active_seconds(pair[0], pair[1]))
        .collect();
    gaps.sort_unstable();

    let learning = gaps.len() < 2;
    let (typical_interval_s, p90_gap_s) =
        if gaps.is_empty() { (None, None) } else { (Some(median(&gaps)), Some(p90(&gaps))) };
    let floor = if learning { LEARNING_ALLOWANCE_S } else { MIN_ALLOWANCE_S };
    let allowance_s = [
        p90_gap_s.map(|gap| gap * 3 / 2),
        typical_interval_s.map(|interval| interval * 2),
        Some(floor),
    ]
    .into_iter()
    .flatten()
    .max()
    .unwrap_or(floor);

    let active_elapsed_s = last_success_s.map(|last| active_seconds(last, now_s));

    // Série d'échecs en cours : une annulation ne la rompt ni ne l'allonge.
    let mut consecutive_failures = 0;
    for run in recent.iter().rev() {
        match run.outcome() {
            Outcome::Success => break,
            outcome if outcome.is_failure() => consecutive_failures += 1,
            _ => {}
        }
    }

    let state = if running {
        State::Running
    } else if consecutive_failures >= FAILING_STREAK {
        State::Failing
    } else if last_success_s.is_none() {
        State::Never
    } else if active_elapsed_s.is_some_and(|elapsed| elapsed > allowance_s) {
        State::Overdue
    } else if off_day(today) {
        State::Idle
    } else if learning {
        State::Learning
    } else {
        State::Ok
    };

    // Heure habituelle : celle des lancements réussis, sinon de tous.
    let usual_hour = {
        let mut counts = [0u32; 24];
        let mut any = false;
        for run in recent.iter().filter(|run| run.outcome() == Outcome::Success) {
            counts[local_hour(run.start_s, utc_offset_s) as usize] += 1;
            any = true;
        }
        if !any {
            for run in &recent {
                counts[local_hour(run.start_s, utc_offset_s) as usize] += 1;
            }
        }
        let best = counts.iter().copied().max().unwrap_or(0);
        (best > 0).then(|| counts.iter().position(|c| *c == best).unwrap_or(0) as u32)
    };

    // Calendrier : la meilleure issue de chaque jour.
    let mut calendar: Vec<DayCell> = (today - WINDOW_DAYS + 1..=today)
        .map(|day| DayCell { day: day_label(day), outcome: "none", runs: 0 })
        .collect();
    for run in &recent {
        let index = local_day(run.at(), utc_offset_s) - (today - WINDOW_DAYS + 1);
        let Some(cell) = usize::try_from(index).ok().and_then(|i| calendar.get_mut(i)) else {
            continue;
        };
        let word = match run.outcome() {
            Outcome::Success => "success",
            Outcome::Running => "running",
            outcome if outcome.is_failure() => "failure",
            Outcome::Cancelled => "cancelled",
            _ => continue,
        };
        cell.runs += 1;
        let rank = |w: &str| match w {
            "success" => 4,
            "running" => 3,
            "failure" => 2,
            "cancelled" => 1,
            _ => 0,
        };
        if rank(word) > rank(cell.outcome) {
            cell.outcome = word;
        }
    }

    let runs_30d = recent.iter().filter(|run| run.outcome() != Outcome::Running).count() as u32;
    let successes_30d = successes.len() as u32;
    let failures_30d = recent.iter().filter(|run| run.outcome().is_failure()).count() as u32;

    let rhythm = rhythm_words(runs_30d, learning, typical_interval_s, &active_weekdays, usual_hour);

    Assessment {
        state,
        last_success_s,
        last_run_s: last_run.map(Run::at),
        last_outcome: last_run.map(|run| run.outcome().label()),
        runs_30d,
        successes_30d,
        failures_30d,
        consecutive_failures,
        typical_interval_s,
        p90_gap_s,
        allowance_s,
        active_elapsed_s,
        active_weekdays,
        usual_hour,
        rhythm,
        calendar,
    }
}

/// Les jours actifs en mots : « every day », « weekdays », « weekends », ou la
/// liste des jours.
fn days_words(active: &[bool; 7]) -> String {
    let count = active.iter().filter(|a| **a).count();
    let weekdays_only = active[..5].iter().all(|a| *a) && !active[5] && !active[6];
    let weekend_only = !active[..5].iter().any(|a| *a) && active[5] && active[6];
    if count == 7 {
        "every day".to_string()
    } else if weekdays_only {
        "weekdays".to_string()
    } else if weekend_only {
        "weekends".to_string()
    } else {
        WEEKDAYS
            .iter()
            .zip(active)
            .filter(|(_, on)| **on)
            .map(|(name, _)| *name)
            .collect::<Vec<_>>()
            .join(", ")
    }
}

fn rhythm_words(
    runs_30d: u32,
    learning: bool,
    typical_interval_s: Option<i64>,
    active: &[bool; 7],
    usual_hour: Option<u32>,
) -> String {
    if runs_30d == 0 {
        return "no run in the last 30 days".to_string();
    }
    if learning {
        let plural = if runs_30d == 1 { "" } else { "s" };
        return format!("still learning ({runs_30d} run{plural} in 30 days)");
    }
    let days = days_words(active);
    let mut words = match typical_interval_s.unwrap_or(DAY) {
        interval if interval < 6 * HOUR => {
            if days == "every day" {
                "several times a day".to_string()
            } else {
                format!("several times a day on {days}")
            }
        }
        interval if interval < 3 * DAY / 2 => days,
        interval => {
            let every = (interval as f64 / DAY as f64).round().max(2.0) as i64;
            if days == "every day" {
                format!("every ~{every} days")
            } else {
                format!("every ~{every} days, on {days}")
            }
        }
    };
    if let Some(hour) = usual_hour {
        words.push_str(&format!(", around {hour:02}:00"));
    }
    words
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Lundi 14 septembre 2026, 00:00 UTC.
    const MONDAY: i64 = 1_789_344_000;

    fn at(day: i64, hour: i64) -> i64 {
        MONDAY + day * DAY + hour * HOUR
    }

    fn run(start: i64, status: i64) -> Run {
        Run { start_s: start, end_s: start + 20 * 60, status }
    }

    /// Un portable de bureau : une sauvegarde chaque jour ouvré vers 20 h, rien
    /// le week-end, sur six semaines jusqu'au vendredi précédant `MONDAY`.
    fn laptop() -> Vec<Run> {
        (-42..0)
            .filter(|day| weekday(local_day(at(*day, 0), 0)) < 5)
            .map(|day| run(at(day, 20), 2))
            .collect()
    }

    /// Un poste fixe : une sauvegarde chaque nuit à 3 h.
    fn desktop(until_day: i64) -> Vec<Run> {
        (-40..=until_day).map(|day| run(at(day, 3), 2)).collect()
    }

    #[test]
    fn un_portable_de_bureau_nest_pas_en_retard_le_lundi_matin() {
        let verdict = assess(&laptop(), at(0, 8), 0);
        assert_eq!(verdict.state, State::Ok, "{verdict:?}");
        assert_eq!(verdict.active_weekdays, [true, true, true, true, true, false, false]);
        // Vendredi 20 h 20 → lundi 8 h : 60 h, dont deux jours de repos entiers.
        assert_eq!(verdict.active_elapsed_s, Some(12 * HOUR - 20 * 60));
        // Un écart vendredi → lundi vaut un jour de temps actif, pas trois.
        assert_eq!(verdict.typical_interval_s, Some(DAY));
        assert_eq!(verdict.p90_gap_s, Some(DAY));
        assert_eq!(verdict.allowance_s, 2 * DAY);
        assert_eq!(verdict.rhythm, "weekdays, around 20:00");
    }

    #[test]
    fn un_portable_de_bureau_est_au_repos_le_week_end() {
        let verdict = assess(&laptop(), at(-2, 12), 0);
        assert_eq!(verdict.state, State::Idle);
        let verdict = assess(&laptop(), at(-1, 23), 0);
        assert_eq!(verdict.state, State::Idle);
    }

    #[test]
    fn un_portable_de_bureau_devient_en_retard_apres_deux_jours_actifs_manques() {
        // Lundi soir : un jour actif manqué, encore dans la tolérance.
        assert_eq!(assess(&laptop(), at(0, 23), 0).state, State::Ok);
        // Mardi 22 h : deux jours actifs sans réussite, tolérance de 48 h dépassée.
        let verdict = assess(&laptop(), at(1, 22), 0);
        assert_eq!(verdict.state, State::Overdue, "{verdict:?}");
        assert!(verdict.active_elapsed_s.unwrap() > verdict.allowance_s);
    }

    #[test]
    fn un_poste_fixe_quotidien_suit_le_plancher_de_trente_six_heures() {
        let runs = desktop(0);
        let verdict = assess(&runs, at(0, 12), 0);
        assert_eq!(verdict.state, State::Ok);
        assert_eq!(verdict.active_weekdays, [true; 7]);
        assert_eq!(verdict.allowance_s, 2 * DAY);
        assert_eq!(verdict.rhythm, "every day, around 03:00");
        assert_eq!(verdict.calendar.len(), 30);
        assert!(verdict.calendar.iter().all(|cell| cell.outcome == "success"));

        // Deux nuits manquées : en retard, quel que soit le jour de la semaine.
        let verdict = assess(&runs, at(2, 4), 0);
        assert_eq!(verdict.state, State::Overdue);
        assert_eq!(verdict.last_success_s, Some(at(0, 3) + 20 * 60));
    }

    #[test]
    fn un_appareil_dont_les_dernieres_tentatives_echouent_est_en_echec() {
        let mut runs = desktop(-3);
        runs.push(run(at(-2, 3), 4));
        runs.push(run(at(-1, 3), 5)); // une annulation ne compte pas…
        runs.push(run(at(0, 3), 3)); // …mais une réussite partielle est un échec.
        let verdict = assess(&runs, at(0, 12), 0);
        assert_eq!(verdict.state, State::Failing);
        assert_eq!(verdict.consecutive_failures, 2);
        assert_eq!(verdict.failures_30d, 2);
        assert_eq!(verdict.last_outcome, Some("partial_success"));
        let today = verdict.calendar.last().unwrap();
        assert_eq!(today.outcome, "failure");
        assert_eq!(verdict.calendar[verdict.calendar.len() - 2].outcome, "cancelled");

        // Un seul échec après une réussite : pas encore en échec.
        let mut runs = desktop(-1);
        runs.push(run(at(0, 3), 4));
        assert_eq!(assess(&runs, at(0, 12), 0).state, State::Ok);
    }

    #[test]
    fn un_appareil_tout_neuf_apprend_encore_et_tolere_trois_jours() {
        let runs = vec![run(at(-1, 9), 2), run(at(0, 9), 2)];
        let verdict = assess(&runs, at(0, 18), 0);
        assert_eq!(verdict.state, State::Learning);
        assert_eq!(verdict.typical_interval_s, Some(DAY));
        assert_eq!(verdict.allowance_s, LEARNING_ALLOWANCE_S);
        assert_eq!(verdict.rhythm, "still learning (2 runs in 30 days)");
        // Sans deux semaines d'historique, aucun jour n'est réputé de repos.
        assert_ne!(assess(&runs, at(5, 12), 0).state, State::Idle);

        let verdict = assess(&runs, at(3, 12), 0);
        assert_eq!(verdict.state, State::Overdue, "{verdict:?}");
    }

    #[test]
    fn un_appareil_jamais_sauvegarde_est_signale_sans_etre_en_retard() {
        assert_eq!(assess(&[], at(0, 12), 0).state, State::Never);
        let verdict = assess(&[run(at(0, 3), 4)], at(0, 12), 0);
        assert_eq!(verdict.state, State::Never);
        assert_eq!(verdict.rhythm, "still learning (1 run in 30 days)");
        assert_eq!(verdict.last_success_s, None);
    }

    #[test]
    fn une_execution_en_cours_prime_sur_le_reste() {
        let mut runs = desktop(-1);
        runs.push(Run { start_s: at(0, 3), end_s: 0, status: 0 });
        assert_eq!(assess(&runs, at(0, 4), 0).state, State::Running);
        // Une exécution « en cours » depuis deux jours est un vestige, pas un état.
        assert_ne!(assess(&runs, at(2, 4), 0).state, State::Running);
    }

    #[test]
    fn une_reussite_hors_fenetre_date_encore_la_derniere_sauvegarde() {
        let runs = vec![run(at(-45, 3), 2)];
        let verdict = assess(&runs, at(0, 12), 0);
        assert_eq!(verdict.last_success_s, Some(at(-45, 3) + 20 * 60));
        assert_eq!(verdict.state, State::Overdue);
        assert_eq!(verdict.runs_30d, 0);
        assert_eq!(verdict.rhythm, "no run in the last 30 days");
    }

    #[test]
    fn le_decalage_horaire_deplace_les_jours_et_les_heures() {
        // 23 h UTC le dimanche est déjà lundi 1 h à Paris (UTC+2 en été).
        let runs: Vec<Run> = (-6..=0).map(|week| run(at(week * 7 - 1, 23), 2)).collect();
        let utc = assess(&runs, at(0, 12), 0);
        assert_eq!(utc.usual_hour, Some(23));
        assert!(utc.active_weekdays[6], "dimanche en UTC");
        let paris = assess(&runs, at(0, 12), 2 * HOUR);
        assert_eq!(paris.usual_hour, Some(1));
        assert!(paris.active_weekdays[0], "lundi à Paris");
        // Un jour actif par semaine : l'écart hebdomadaire vaut un jour actif.
        assert_eq!(paris.typical_interval_s, Some(DAY));
        assert_eq!(paris.rhythm, "Mon, around 01:00");
        assert_eq!(utc.rhythm, "Sun, around 23:00");
    }

    #[test]
    fn les_mots_du_rythme_couvrent_les_cadences_courantes() {
        let all = [true; 7];
        assert_eq!(
            rhythm_words(60, false, Some(HOUR), &all, Some(9)),
            "several times a day, around 09:00"
        );
        let weekend = [false, false, false, false, false, true, true];
        assert_eq!(rhythm_words(8, false, Some(DAY), &weekend, None), "weekends");
        let some = [true, false, true, false, true, false, false];
        assert_eq!(
            rhythm_words(12, false, Some(2 * DAY), &some, Some(20)),
            "every ~2 days, on Mon, Wed, Fri, around 20:00"
        );
    }

    #[test]
    fn la_mediane_et_le_decile_suivent_les_definitions_usuelles() {
        assert_eq!(median(&[1, 2, 3]), 2);
        assert_eq!(median(&[1, 2, 3, 4]), 2);
        assert_eq!(p90(&[1, 2, 3, 4, 5, 6, 7, 8, 9, 10]), 9);
        assert_eq!(p90(&[5]), 5);
        assert_eq!(weekday(local_day(MONDAY, 0)), 0);
    }
}
