//! Baselines saisonnières pour la détection d'anomalie (jalon 5).
//!
//! Le trafic d'un homelab est saisonnier : le NAS travaille la nuit, la télévision
//! consomme le dimanche soir. Un seuil fixe est donc soit muet, soit bruyant. On
//! garde plutôt 168 seaux par série — un par couple (jour de la semaine, heure) —
//! et on compare chaque point à ce qu'on observe habituellement *à ce moment-là*.
//!
//! Le calcul est incrémental : on ne conserve jamais l'historique, seulement deux
//! nombres par seau. Un an de baselines pour mille séries tient dans quelques
//! mégaoctets de SQLite, et rien n'a besoin d'être recalculé au démarrage.

use chrono::{DateTime, Datelike, TimeDelta, Timelike, Utc};

use crate::alerting::model::AnomalyParams;

/// 7 jours × 24 heures.
pub const BUCKETS: usize = 168;

/// Constante de conversion entre déviation absolue médiane et écart-type pour une
/// loi normale. C'est elle qui rend le score comparable à un « nombre de sigmas ».
pub const MAD_TO_SIGMA: f64 = 1.4826;

/// Durée d'apprentissage avant qu'une anomalie ait le droit de notifier.
///
/// Deux semaines, parce qu'il faut avoir vu chaque seau au moins deux fois pour
/// distinguer « inhabituel » de « jamais observé ».
pub const LEARNING_PERIOD_DAYS: i64 = 14;

/// Index du seau saisonnier d'un instant : 0 = lundi 0 h, 167 = dimanche 23 h.
pub fn bucket_of(at: DateTime<Utc>) -> usize {
    let day = at.weekday().num_days_from_monday() as usize;
    day * 24 + at.hour() as usize
}

/// État persistant d'un seau.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Bucket {
    /// Moyenne mobile exponentielle de la valeur représentative du seau.
    pub ewma: f64,
    /// Déviation absolue lissée, estimateur en ligne de la MAD.
    pub mad: f64,
    pub samples: u32,
}

impl Bucket {
    /// Échelle robuste utilisée au dénominateur du score.
    ///
    /// Le plancher n'est pas une précaution cosmétique : une série parfaitement
    /// plate — un onduleur à 100 % de charge, un ventilateur à vitesse fixe — a une
    /// MAD strictement nulle. Sans plancher, le moindre bruit de mesure donnerait un
    /// score infini et l'alerte ne s'éteindrait jamais.
    pub fn scale(&self, params: &AnomalyParams) -> f64 {
        let floor = (params.mad_floor_rel * self.ewma.abs()).max(params.mad_floor_abs);
        (MAD_TO_SIGMA * self.mad).max(floor)
    }

    /// Score robuste `|x − ewma| / (1,4826 × MAD)`, planché.
    ///
    /// Renvoie `None` tant que le seau n'a pas assez d'observations : scorer sur une
    /// seule mesure reviendrait à comparer un point à lui-même.
    pub fn score(&self, value: f64, params: &AnomalyParams) -> Option<f64> {
        if self.samples < params.min_samples || !value.is_finite() {
            return None;
        }
        Some((value - self.ewma).abs() / self.scale(params))
    }

    /// Intègre une observation.
    ///
    /// La première initialise le seau : lisser à partir de zéro ferait converger la
    /// baseline pendant des semaines, et produirait des anomalies pour tout le début
    /// de vie de la série.
    ///
    /// L'appelant score *avant* d'appeler cette méthode ; l'ordre compte, sinon la
    /// valeur anormale se retrouverait dans la baseline qui vient de la juger.
    pub fn observe(&mut self, value: f64, params: &AnomalyParams) {
        if !value.is_finite() {
            return;
        }
        if self.samples == 0 {
            self.ewma = value;
            self.mad = 0.0;
        } else {
            let alpha = params.alpha.clamp(f64::EPSILON, 1.0);
            let deviation = (value - self.ewma).abs();
            // La MAD est mise à jour avant l'EWMA : la déviation doit se mesurer par
            // rapport à la baseline telle qu'elle était quand le point est arrivé.
            self.mad = alpha * deviation + (1.0 - alpha) * self.mad;
            self.ewma = alpha * value + (1.0 - alpha) * self.ewma;
        }
        self.samples = self.samples.saturating_add(1);
    }
}

/// Suivi d'une série : depuis quand on l'observe, et ses 168 seaux.
#[derive(Debug, Clone, PartialEq)]
pub struct SeriesBaseline {
    pub first_seen: DateTime<Utc>,
    pub updates: u64,
}

impl SeriesBaseline {
    pub fn new(first_seen: DateTime<Utc>) -> Self {
        Self { first_seen, updates: 0 }
    }

    /// Vrai tant que la série n'a pas assez d'ancienneté pour notifier.
    ///
    /// C'est l'ancienneté et non le nombre de points qui décide : une série relevée
    /// toutes les cinq minutes accumule dix mille points en un mois sans pour autant
    /// avoir vu un seul dimanche de plus qu'une série relevée toutes les heures.
    pub fn is_learning(&self, now: DateTime<Utc>) -> bool {
        now.signed_duration_since(self.first_seen) < TimeDelta::days(LEARNING_PERIOD_DAYS)
    }
}

/// Médiane d'un échantillon, utilisée quand un cycle apporte plusieurs points pour
/// un même seau : la médiane absorbe les pics isolés, contrairement à la moyenne,
/// et c'est bien elle qu'on veut lisser.
pub fn median(values: &[f64]) -> Option<f64> {
    let mut finite: Vec<f64> = values.iter().copied().filter(|v| v.is_finite()).collect();
    if finite.is_empty() {
        return None;
    }
    finite.sort_by(|a, b| a.partial_cmp(b).expect("finite values, total ordering"));
    let middle = finite.len() / 2;
    Some(if finite.len().is_multiple_of(2) {
        (finite[middle - 1] + finite[middle]) / 2.0
    } else {
        finite[middle]
    })
}

/// Verdict d'un point face à sa baseline.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AnomalyVerdict {
    pub score: Option<f64>,
    /// Vrai si le score dépasse `k`, apprentissage ou non. C'est cette valeur qui
    /// pilote la machine à états : pendant l'apprentissage l'alerte progresse donc
    /// normalement, elle est simplement empêchée de notifier. L'interface peut ainsi
    /// montrer ce que la détection *aurait* déclenché.
    pub exceeds: bool,
    /// Vrai si le score dépasse `k` et que la série a passé l'apprentissage.
    pub anomalous: bool,
    pub learning: bool,
}

/// Évalue un point sans modifier la baseline.
pub fn evaluate(
    series: &SeriesBaseline,
    bucket: &Bucket,
    value: f64,
    now: DateTime<Utc>,
    params: &AnomalyParams,
) -> AnomalyVerdict {
    let learning = series.is_learning(now);
    let score = bucket.score(value, params);
    // En apprentissage on calcule quand même le score : l'interface affiche ce que
    // l'alerte *aurait* déclenché, ce qui donne à l'utilisateur de quoi juger le
    // réglage de `k` avant que la détection ne prenne la parole.
    let exceeds = score.is_some_and(|s| s >= params.k);
    AnomalyVerdict { score, exceeds, anomalous: exceeds && !learning, learning }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(iso: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(iso).expect("valid ISO timestamp").with_timezone(&Utc)
    }

    fn params() -> AnomalyParams {
        AnomalyParams::default()
    }

    #[test]
    fn les_seaux_couvrent_la_semaine_entiere() {
        // 2026-03-02 est un lundi.
        assert_eq!(bucket_of(at("2026-03-02T00:30:00Z")), 0);
        assert_eq!(bucket_of(at("2026-03-02T13:59:00Z")), 13);
        // 2026-03-08 est un dimanche.
        assert_eq!(bucket_of(at("2026-03-08T23:00:00Z")), BUCKETS - 1);
        for jour in 2..=8 {
            for heure in 0..24 {
                let index = bucket_of(at(&format!("2026-03-{jour:02}T{heure:02}:00:00Z")));
                assert!(index < BUCKETS);
            }
        }
    }

    #[test]
    fn la_premiere_observation_initialise_le_seau() {
        let mut bucket = Bucket::default();
        bucket.observe(42.0, &params());
        assert_eq!(bucket.ewma, 42.0);
        assert_eq!(bucket.mad, 0.0);
        assert_eq!(bucket.samples, 1);
    }

    #[test]
    fn l_ewma_converge_vers_le_niveau_observe() {
        let mut bucket = Bucket::default();
        bucket.observe(0.0, &params());
        for _ in 0..500 {
            bucket.observe(100.0, &params());
        }
        assert!((bucket.ewma - 100.0).abs() < 0.1, "ewma = {}", bucket.ewma);
        assert!(bucket.mad < 0.5, "the MAD drops when the series settles");
    }

    #[test]
    fn la_mad_croit_avec_la_dispersion() {
        let params = params();
        let mut calme = Bucket::default();
        let mut agite = Bucket::default();
        for i in 0..400 {
            let oscillation = if i % 2 == 0 { 1.0 } else { -1.0 };
            calme.observe(50.0 + 0.1 * oscillation, &params);
            agite.observe(50.0 + 20.0 * oscillation, &params);
        }
        assert!(agite.mad > calme.mad * 10.0, "calm={} agitated={}", calme.mad, agite.mad);
    }

    #[test]
    fn le_score_signale_un_ecart_franc_et_ignore_le_bruit() {
        let params = params();
        let mut bucket = Bucket::default();
        for i in 0..400 {
            bucket.observe(50.0 + if i % 2 == 0 { 2.0 } else { -2.0 }, &params);
        }
        let normal = bucket.score(51.0, &params).expect("bucket fed enough");
        assert!(normal < params.k, "an ordinary point must not alert: {normal}");

        let extreme = bucket.score(500.0, &params).expect("bucket fed enough");
        assert!(extreme > params.k, "a tenfold increase must alert: {extreme}");
    }

    #[test]
    fn une_serie_parfaitement_plate_ne_divise_jamais_par_zero() {
        let params = params();
        let mut bucket = Bucket::default();
        for _ in 0..100 {
            bucket.observe(100.0, &params);
        }
        assert_eq!(bucket.mad, 0.0, "no dispersion observed");

        let score = bucket.score(100.5, &params).expect("fed bucket");
        assert!(score.is_finite(), "the floor avoids division by zero");
        // Plancher relatif : 1 % de 100 = 1,0 ; l'écart de 0,5 vaut donc 0,5 sigma.
        assert!((score - 0.5).abs() < 1e-9, "score = {score}");
        assert!(score < params.k, "half a percent of drift must not alert");

        // Un écart réellement massif reste détecté malgré le plancher.
        let massif = bucket.score(1000.0, &params).expect("fed bucket");
        assert!(massif > params.k, "score = {massif}");
    }

    #[test]
    fn une_serie_plate_a_zero_reste_finie() {
        let params = params();
        let mut bucket = Bucket::default();
        for _ in 0..50 {
            bucket.observe(0.0, &params);
        }
        // Ici le plancher relatif vaut zéro : seul le plancher absolu protège.
        let score = bucket.score(0.0, &params).expect("fed bucket");
        assert!(score.is_finite() && score == 0.0);
        assert!(bucket.score(1.0, &params).expect("fed bucket").is_finite());
    }

    #[test]
    fn un_seau_trop_neuf_ne_score_pas() {
        let params = params();
        let mut bucket = Bucket::default();
        assert_eq!(bucket.score(10.0, &params), None);
        bucket.observe(10.0, &params);
        bucket.observe(10.0, &params);
        assert_eq!(bucket.score(10.0, &params), None, "min_samples = 3");
        bucket.observe(10.0, &params);
        assert!(bucket.score(10.0, &params).is_some());
    }

    #[test]
    fn une_valeur_non_finie_ne_corrompt_pas_la_baseline() {
        let params = params();
        let mut bucket = Bucket::default();
        bucket.observe(10.0, &params);
        bucket.observe(f64::NAN, &params);
        bucket.observe(f64::INFINITY, &params);
        assert_eq!(bucket.samples, 1);
        assert_eq!(bucket.ewma, 10.0);
    }

    #[test]
    fn l_apprentissage_dure_exactement_quatorze_jours() {
        let series = SeriesBaseline::new(at("2026-03-01T00:00:00Z"));
        assert!(series.is_learning(at("2026-03-01T00:00:00Z")));
        assert!(series.is_learning(at("2026-03-14T23:59:59Z")));
        assert!(!series.is_learning(at("2026-03-15T00:00:00Z")), "exactly fourteen days");
        assert!(!series.is_learning(at("2026-04-01T00:00:00Z")));
    }

    #[test]
    fn l_apprentissage_calcule_le_score_mais_ne_declenche_pas() {
        let params = params();
        let mut bucket = Bucket::default();
        for _ in 0..50 {
            bucket.observe(10.0, &params);
        }
        let series = SeriesBaseline::new(at("2026-03-01T00:00:00Z"));

        let pendant = evaluate(&series, &bucket, 10_000.0, at("2026-03-10T00:00:00Z"), &params);
        assert!(pendant.learning);
        assert!(!pendant.anomalous, "no notification while learning");
        assert!(pendant.score.is_some_and(|s| s > params.k), "but the score is recorded");
        assert!(pendant.exceeds, "and the excess stays visible to the UI");

        let apres = evaluate(&series, &bucket, 10_000.0, at("2026-03-20T00:00:00Z"), &params);
        assert!(!apres.learning);
        assert!(apres.anomalous, "detection speaks up after learning");
    }

    #[test]
    fn la_mediane_absorbe_les_pics_isoles() {
        assert_eq!(median(&[1.0, 2.0, 3.0]), Some(2.0));
        assert_eq!(median(&[1.0, 2.0, 3.0, 4.0]), Some(2.5));
        assert_eq!(median(&[1.0, 1.0, 1.0, 1.0, 9_000.0]), Some(1.0));
        assert_eq!(median(&[f64::NAN, 5.0]), Some(5.0));
        assert_eq!(median(&[]), None);
        assert_eq!(median(&[f64::NAN]), None);
    }
}
