//! Cœur d'un cycle d'évaluation, sans aucune entrée/sortie.
//!
//! [`plan_cycle`] reçoit tout ce dont il a besoin — les règles, ce que
//! VictoriaMetrics a répondu, l'état précédent, la topologie, les silences, les
//! baselines — et renvoie le nouvel état, les messages à envoyer et les
//! transitions à journaliser. Le moteur autour n'a plus qu'à faire des
//! entrées/sorties, ce qui rend l'ensemble du comportement testable sans réseau ni
//! base de données.

use std::collections::{BTreeMap, HashMap, HashSet};

use chrono::{DateTime, Utc};

use crate::alerting::baseline::{self, Bucket, SeriesBaseline};
use crate::alerting::group::{self, AlertGroup, AlertOutcome};
use crate::alerting::machine::{self, AlertState, Phase, Transition};
use crate::alerting::model::{
    self, Rule, TARGET_ADDRESS_LABELS, TARGET_ID_LABELS, TARGET_NAME_LABELS, TargetId, TargetNode,
};
use crate::alerting::overrides::{Effective, OverrideIndex, RuleOverride};
use crate::alerting::silence::Silence;
use crate::alerting::source::SeriesPoint;
use crate::alerting::suppress::{self, Topology};

/// Nom affiché quand une série n'a pu être rattachée à aucune cible.
pub const UNKNOWN_TARGET: &str = "no device";

/// Alerte telle qu'elle était en base au début du cycle.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredAlert {
    pub fingerprint: String,
    pub rule_uid: String,
    pub target_id: Option<TargetId>,
    pub series_key: String,
    pub labels: BTreeMap<String, String>,
    pub state: AlertState,
}

/// Réponse de VictoriaMetrics pour une règle.
#[derive(Debug, Clone)]
pub struct RuleObservations {
    pub rule: Rule,
    /// `None` quand la requête a échoué.
    ///
    /// La distinction avec `Some(vec![])` est essentielle : une réponse vide veut
    /// dire « plus aucune série ne viole la règle », donc résolution ; une erreur ne
    /// veut rien dire du tout, et résoudre sur une panne de VictoriaMetrics
    /// enverrait une salve de « tout va bien » parfaitement mensongère.
    pub series: Option<Vec<SeriesPoint>>,
}

/// Baselines saisonnières chargées en mémoire pour le cycle.
#[derive(Debug, Default)]
pub struct BaselineStore {
    series: HashMap<String, SeriesBaseline>,
    buckets: HashMap<(String, usize), Bucket>,
    dirty_series: HashSet<String>,
    dirty_buckets: HashSet<(String, usize)>,
}

impl BaselineStore {
    pub fn new(
        series: HashMap<String, SeriesBaseline>,
        buckets: HashMap<(String, usize), Bucket>,
    ) -> Self {
        Self { series, buckets, dirty_series: HashSet::new(), dirty_buckets: HashSet::new() }
    }

    pub fn series(&self, key: &str) -> Option<&SeriesBaseline> {
        self.series.get(key)
    }

    pub fn bucket(&self, key: &str, index: usize) -> Option<&Bucket> {
        self.buckets.get(&(key.to_string(), index))
    }

    /// Seaux modifiés pendant le cycle : seuls ceux-là sont réécrits en base.
    pub fn dirty(&self) -> impl Iterator<Item = (&str, usize, &Bucket)> {
        self.dirty_buckets
            .iter()
            .filter_map(|key| self.buckets.get(key).map(|bucket| (key.0.as_str(), key.1, bucket)))
    }

    pub fn dirty_series(&self) -> impl Iterator<Item = (&str, &SeriesBaseline)> {
        self.dirty_series
            .iter()
            .filter_map(|key| self.series.get(key).map(|series| (key.as_str(), series)))
    }
}

/// Tout ce qu'un cycle a besoin de connaître.
pub struct CycleInput {
    pub now: DateTime<Utc>,
    pub observations: Vec<RuleObservations>,
    pub targets: Vec<TargetNode>,
    pub previous: Vec<StoredAlert>,
    pub silences: Vec<Silence>,
    /// Surcharges par équipement (seuil, seuil de retour, désactivation).
    pub overrides: Vec<RuleOverride>,
}

/// Une transition à journaliser.
#[derive(Debug, Clone, PartialEq)]
pub struct HistoryEntry {
    pub fingerprint: String,
    pub rule_uid: String,
    pub target_id: Option<TargetId>,
    pub transition: Transition,
    pub severity: crate::alerting::model::Severity,
    pub value: Option<f64>,
    /// Explication du mutisme, vide quand la transition a bien notifié.
    pub reason: String,
    pub at: DateTime<Utc>,
}

/// Résultat d'un cycle.
#[derive(Debug, Default)]
pub struct CycleOutcome {
    /// État de chaque empreinte évaluée, à persister tel quel.
    pub alerts: Vec<AlertOutcome>,
    /// Messages à envoyer, un par hôte au plus.
    pub groups: Vec<AlertGroup>,
    pub history: Vec<HistoryEntry>,
    /// Empreintes dont la règle n'a pas pu être évaluée : leur ligne est conservée
    /// en l'état, sans quoi la purge des empreintes obsolètes les effacerait.
    pub carried_over: Vec<String>,
}

/// Index de résolution série → cible.
struct TargetIndex {
    by_id: HashMap<TargetId, TargetNode>,
    by_name: HashMap<String, TargetId>,
    by_address: HashMap<String, TargetId>,
}

impl TargetIndex {
    fn new(targets: &[TargetNode]) -> Self {
        Self {
            by_id: targets.iter().map(|t| (t.id, t.clone())).collect(),
            by_name: targets.iter().map(|t| (t.name.clone(), t.id)).collect(),
            by_address: targets.iter().map(|t| (t.address.clone(), t.id)).collect(),
        }
    }

    /// Rattache une série à une cible.
    ///
    /// On essaie plusieurs conventions d'étiquettes plutôt qu'une seule : l'identité
    /// posée sur les échantillons relève des collecteurs, et une règle écrite à la
    /// main par l'utilisateur peut parfaitement agréger en ne gardant que `host`.
    fn resolve(&self, labels: &BTreeMap<String, String>) -> Option<&TargetNode> {
        for label in TARGET_ID_LABELS {
            if let Some(raw) = labels.get(label)
                && let Ok(id) = raw.parse::<TargetId>()
                && let Some(node) = self.by_id.get(&id)
            {
                return Some(node);
            }
        }
        for label in TARGET_NAME_LABELS {
            if let Some(name) = labels.get(label)
                && let Some(node) = self.by_name.get(name).and_then(|id| self.by_id.get(id))
            {
                return Some(node);
            }
        }
        for label in TARGET_ADDRESS_LABELS {
            if let Some(address) = labels.get(label) {
                // Une étiquette `instance` porte souvent un port : on compare aussi
                // sur l'hôte seul.
                let host = address.rsplit_once(':').map_or(address.as_str(), |(h, _)| h);
                if let Some(node) = self
                    .by_address
                    .get(address)
                    .or_else(|| self.by_address.get(host))
                    .and_then(|id| self.by_id.get(id))
                {
                    return Some(node);
                }
            }
        }
        None
    }
}

/// État intermédiaire d'une empreinte, avant application des surcouches.
struct Evaluated {
    fingerprint: String,
    rule_index: usize,
    target_id: Option<TargetId>,
    target_name: String,
    series_key: String,
    labels: BTreeMap<String, String>,
    state: AlertState,
    transition: Option<Transition>,
    value: Option<f64>,
    score: Option<f64>,
    /// Seuil réellement appliqué, surcharge comprise : c'est lui que le message
    /// doit citer, pas celui de la règle.
    threshold: f64,
}

/// Évalue un cycle complet.
///
/// `baselines` est modifié au passage — les baselines d'anomalie sont incrémentales
/// par construction. L'appelant persiste ensuite les seuls seaux marqués modifiés.
pub fn plan_cycle(input: CycleInput, baselines: &mut BaselineStore) -> CycleOutcome {
    let now = input.now;
    let index = TargetIndex::new(&input.targets);
    let topology = Topology::new(&input.targets);
    let overrides = OverrideIndex::new(&input.overrides);

    let previous: HashMap<&str, &StoredAlert> =
        input.previous.iter().map(|alert| (alert.fingerprint.as_str(), alert)).collect();

    let mut evaluated: Vec<Evaluated> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut carried_over: Vec<String> = Vec::new();

    for (rule_index, observation) in input.observations.iter().enumerate() {
        let rule = &observation.rule;

        let Some(series) = &observation.series else {
            // Règle non évaluable ce cycle : on gèle ses empreintes.
            carried_over.extend(
                input
                    .previous
                    .iter()
                    .filter(|a| a.rule_uid == rule.uid)
                    .map(|a| a.fingerprint.clone()),
            );
            continue;
        };

        for point in series {
            let target = index.resolve(&point.labels);
            if !rule.selector.matches(target) {
                continue;
            }
            let effective = overrides.effective(rule, target.map(|t| t.id));
            // Règle retirée pour cet équipement : la série est ignorée, ce qui
            // résout proprement une alerte en cours par le passage « séries
            // disparues » plus bas.
            if !effective.enabled {
                continue;
            }

            let series_key = model::series_key(&point.labels);
            let fingerprint = model::fingerprint(&rule.uid, &series_key);
            // Deux séries distinctes qui se réduisent à la même clé — cela arrive
            // avec une agrégation mal écrite — ne doivent pas se battre pour la même
            // ligne d'état : la première rencontrée gagne, la seconde est ignorée.
            if !seen.insert(fingerprint.clone()) {
                continue;
            }

            let stored = previous.get(fingerprint.as_str());
            let mut before = stored.map(|a| a.state.clone()).unwrap_or_default();
            // Les surcouches du cycle précédent ne doivent pas se propager : elles
            // sont recalculées intégralement plus bas.
            before.suppressed = false;
            before.suppressed_by = None;
            before.silenced = false;

            // Hystérésis : la condition était vraie au cycle précédent si elle est
            // datée, que l'alerte soit encore en attente ou déjà partie.
            let active = before.condition_since.is_some();
            let (condition, score, learning) =
                evaluate_point(rule, &effective, active, &series_key, point, now, baselines);
            let (mut state, transition) =
                machine::advance(&before, condition, now, rule.for_duration);
            state.value = Some(point.value);
            state.score = score;
            state.learning = learning;

            let target_name = target
                .map(|t| t.name.clone())
                .or_else(|| TARGET_NAME_LABELS.iter().find_map(|l| point.labels.get(*l).cloned()))
                .unwrap_or_else(|| UNKNOWN_TARGET.to_string());

            evaluated.push(Evaluated {
                fingerprint,
                rule_index,
                target_id: target.map(|t| t.id),
                target_name,
                series_key,
                labels: point.labels.clone(),
                state,
                transition,
                value: Some(point.value),
                score,
                threshold: effective.threshold,
            });
        }

        // Séries disparues : sans ce passage, une alerte dont la série cesse
        // d'exister resterait `firing` éternellement.
        for stored in input.previous.iter().filter(|a| a.rule_uid == rule.uid) {
            if seen.contains(&stored.fingerprint) {
                continue;
            }
            let mut before = stored.state.clone();
            before.suppressed = false;
            before.suppressed_by = None;
            before.silenced = false;
            let (state, transition) = machine::advance(&before, false, now, rule.for_duration);

            // Une empreinte revenue au repos et jamais notifiée n'a plus rien à dire :
            // on la laisse sortir de la table plutôt que d'y accumuler du passé.
            if state.phase == Phase::Ok && transition.is_none() {
                continue;
            }
            seen.insert(stored.fingerprint.clone());
            evaluated.push(Evaluated {
                fingerprint: stored.fingerprint.clone(),
                rule_index,
                target_id: stored.target_id,
                target_name: stored
                    .target_id
                    .and_then(|id| index.by_id.get(&id))
                    .map(|t| t.name.clone())
                    .unwrap_or_else(|| UNKNOWN_TARGET.to_string()),
                series_key: stored.series_key.clone(),
                labels: stored.labels.clone(),
                state,
                transition,
                value: None,
                score: None,
                threshold: overrides.effective(rule, stored.target_id).threshold,
            });
        }
    }

    // Ensemble des cibles hors ligne, déduit du cycle courant : la suppression est
    // ainsi cohérente avec ce qu'on s'apprête à notifier, et non avec le cycle
    // précédent.
    let down: HashSet<TargetId> = evaluated
        .iter()
        .filter(|entry| {
            input.observations[entry.rule_index].rule.is_host_down()
                && entry.state.phase == Phase::Firing
        })
        .filter_map(|entry| entry.target_id)
        .collect();

    let mut alerts = Vec::with_capacity(evaluated.len());
    let mut history = Vec::new();

    for mut entry in evaluated {
        let rule = &input.observations[entry.rule_index].rule;

        if let Some(suppression) =
            suppress::suppression_for(&topology, entry.target_id, rule.is_host_down(), &down)
        {
            entry.state.suppressed = true;
            entry.state.suppressed_by = entry.target_id.map(|target| suppression.culprit(target));
        }

        if crate::alerting::silence::first_match(
            &input.silences,
            now,
            entry.target_id,
            &entry.labels,
        )
        .is_some()
        {
            entry.state.silenced = true;
        }

        if let Some(transition) = &entry.transition {
            history.push(HistoryEntry {
                fingerprint: entry.fingerprint.clone(),
                rule_uid: rule.uid.clone(),
                target_id: entry.target_id,
                transition: transition.clone(),
                severity: rule.severity,
                value: entry.value,
                reason: history_reason(&entry.state),
                at: now,
            });
        }

        alerts.push(AlertOutcome {
            fingerprint: entry.fingerprint,
            rule_uid: rule.uid.clone(),
            rule_name: rule.name.clone(),
            target_id: entry.target_id,
            target_name: entry.target_name,
            series_key: entry.series_key,
            labels: entry.labels,
            state: entry.state,
            severity: rule.severity,
            value: entry.value,
            score: entry.score,
            unit: rule.unit.clone(),
            operator: rule.operator.as_str().to_string(),
            threshold: entry.threshold,
            channels: rule.channels.clone(),
            repeat_interval: rule.repeat_interval,
            escalate_after: rule.escalate_after,
            just_transitioned: entry.transition.is_some(),
        });
    }

    let groups = group::group(&alerts, now);
    CycleOutcome { alerts, groups, history, carried_over }
}

/// Explique en une phrase pourquoi cette transition n'a pas notifié, le cas échéant.
fn history_reason(state: &AlertState) -> String {
    if state.learning {
        "learning: would have fired".to_string()
    } else if state.suppressed {
        match state.suppressed_by {
            Some(culprit) => format!("suppressed: device {culprit} unreachable"),
            None => "suppressed by dependency".to_string(),
        }
    } else if state.silenced {
        "maintenance window".to_string()
    } else {
        String::new()
    }
}

/// Évalue un point selon le type de la règle.
///
/// Renvoie `(condition_remplie, score_d_anomalie, en_apprentissage)`.
fn evaluate_point(
    rule: &Rule,
    effective: &Effective,
    active: bool,
    series_key: &str,
    point: &SeriesPoint,
    now: DateTime<Utc>,
    baselines: &mut BaselineStore,
) -> (bool, Option<f64>, bool) {
    use crate::alerting::model::RuleKind;

    match rule.kind {
        // Une règle prédictive n'est qu'un seuil sur une requête `predict_linear` :
        // c'est VictoriaMetrics qui extrapole, pas nous.
        RuleKind::Threshold | RuleKind::Predict => (
            rule.operator.holds(
                point.value,
                effective.threshold,
                effective.clear_threshold,
                active,
            ),
            None,
            false,
        ),
        RuleKind::Anomaly => {
            let bucket_index = baseline::bucket_of(now);
            let series = baselines
                .series
                .entry(series_key.to_string())
                .or_insert_with(|| SeriesBaseline::new(now));
            let is_first_sight = series.updates == 0;
            let series_snapshot = series.clone();

            let bucket =
                baselines.buckets.entry((series_key.to_string(), bucket_index)).or_default();
            // On score avant d'intégrer le point : sinon la valeur anormale
            // participerait à la baseline qui doit la juger, et se noierait elle-même.
            let verdict =
                baseline::evaluate(&series_snapshot, bucket, point.value, now, &rule.params);
            bucket.observe(point.value, &rule.params);

            baselines.dirty_buckets.insert((series_key.to_string(), bucket_index));
            if let Some(series) = baselines.series.get_mut(series_key) {
                series.updates = series.updates.saturating_add(1);
            }
            // La ligne de série n'est réécrite qu'à la première observation : son
            // `first_seen` ne bouge plus ensuite, et le compteur de mises à jour ne
            // vaut pas une écriture SQLite à chaque cycle.
            if is_first_sight {
                baselines.dirty_series.insert(series_key.to_string());
            }

            (verdict.exceeds, verdict.score, verdict.learning)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::alerting::group::NotifyReason;
    use crate::alerting::machine::EffectivePhase;
    use crate::alerting::model::{AnomalyParams, Operator, RuleKind, Severity, TargetSelector};
    use crate::alerting::silence::Schedule;

    fn at(seconds: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_700_000_000 + seconds, 0).expect("valid timestamp")
    }

    fn rule(uid: &str, kind: RuleKind, threshold: f64, for_secs: u64) -> Rule {
        Rule {
            id: 1,
            uid: uid.to_string(),
            name: format!("rule {uid}"),
            description: String::new(),
            kind,
            query: "q".to_string(),
            operator: Operator::Gt,
            threshold,
            clear_threshold: None,
            for_duration: Duration::from_secs(for_secs),
            severity: Severity::Warning,
            selector: TargetSelector::All,
            channels: Vec::new(),
            params: AnomalyParams::default(),
            unit: String::new(),
            repeat_interval: None,
            escalate_after: None,
            enabled: true,
            builtin: false,
        }
    }

    fn point(target: TargetId, value: f64) -> SeriesPoint {
        SeriesPoint {
            labels: [
                ("__name__".to_string(), "dumbmonit_x".to_string()),
                ("target".to_string(), target.to_string()),
                ("host".to_string(), format!("device-{target}")),
            ]
            .into_iter()
            .collect(),
            value,
            ts_ms: 0,
        }
    }

    fn node(id: TargetId, parent: Option<TargetId>) -> TargetNode {
        TargetNode {
            id,
            name: format!("device-{id}"),
            address: format!("10.0.0.{id}"),
            parent_id: parent,
            tags: BTreeMap::new(),
        }
    }

    /// Convertit les alertes d'un cycle en état stocké, pour enchaîner les cycles.
    fn to_stored(outcome: &CycleOutcome) -> Vec<StoredAlert> {
        outcome
            .alerts
            .iter()
            .map(|alert| StoredAlert {
                fingerprint: alert.fingerprint.clone(),
                rule_uid: alert.rule_uid.clone(),
                target_id: alert.target_id,
                series_key: alert.series_key.clone(),
                labels: alert.labels.clone(),
                state: alert.state.clone(),
            })
            .collect()
    }

    /// Marque comme notifiées les alertes que le cycle a effectivement groupées,
    /// exactement comme le fait le moteur après un envoi réussi.
    fn mark_notified(stored: &mut [StoredAlert], outcome: &CycleOutcome, now: DateTime<Utc>) {
        let notified: HashSet<&str> = outcome
            .groups
            .iter()
            .flat_map(|group| group.items.iter().map(|item| item.fingerprint.as_str()))
            .collect();
        for alert in stored.iter_mut() {
            if notified.contains(alert.fingerprint.as_str()) {
                alert.state.last_notified_at = Some(now);
                alert.state.notify_count += 1;
            }
        }
    }

    fn input(
        now: DateTime<Utc>,
        observations: Vec<RuleObservations>,
        targets: Vec<TargetNode>,
        previous: Vec<StoredAlert>,
    ) -> CycleInput {
        CycleInput {
            now,
            observations,
            targets,
            previous,
            silences: Vec::new(),
            overrides: Vec::new(),
        }
    }

    #[test]
    fn un_depassement_maintenu_finit_par_notifier_une_fois() {
        let cpu = rule("cpu_high", RuleKind::Threshold, 90.0, 300);
        let mut baselines = BaselineStore::default();

        let cycle1 = plan_cycle(
            input(
                at(0),
                vec![RuleObservations { rule: cpu.clone(), series: Some(vec![point(1, 95.0)]) }],
                vec![node(1, None)],
                Vec::new(),
            ),
            &mut baselines,
        );
        assert_eq!(cycle1.alerts[0].effective_phase(), EffectivePhase::Pending);
        assert!(cycle1.groups.is_empty(), "`for` has not elapsed");

        let mut stored = to_stored(&cycle1);
        let cycle2 = plan_cycle(
            input(
                at(300),
                vec![RuleObservations { rule: cpu.clone(), series: Some(vec![point(1, 95.0)]) }],
                vec![node(1, None)],
                stored.clone(),
            ),
            &mut baselines,
        );
        assert_eq!(cycle2.groups.len(), 1);
        assert_eq!(cycle2.groups[0].items[0].reason, NotifyReason::Firing);
        assert_eq!(cycle2.groups[0].target_name, "device-1");

        // Cycle suivant, toujours en dépassement : sans rappel configuré, silence.
        stored = to_stored(&cycle2);
        mark_notified(&mut stored, &cycle2, at(300));
        let cycle3 = plan_cycle(
            input(
                at(360),
                vec![RuleObservations { rule: cpu, series: Some(vec![point(1, 95.0)]) }],
                vec![node(1, None)],
                stored,
            ),
            &mut baselines,
        );
        assert!(cycle3.groups.is_empty(), "a known alert does not repeat");
    }

    #[test]
    fn la_disparition_d_une_serie_resout_l_alerte() {
        let cpu = rule("cpu_high", RuleKind::Threshold, 90.0, 0);
        let mut baselines = BaselineStore::default();

        let cycle1 = plan_cycle(
            input(
                at(0),
                vec![RuleObservations { rule: cpu.clone(), series: Some(vec![point(1, 95.0)]) }],
                vec![node(1, None)],
                Vec::new(),
            ),
            &mut baselines,
        );
        let mut stored = to_stored(&cycle1);
        mark_notified(&mut stored, &cycle1, at(0));

        let cycle2 = plan_cycle(
            input(
                at(60),
                vec![RuleObservations { rule: cpu, series: Some(Vec::new()) }],
                vec![node(1, None)],
                stored,
            ),
            &mut baselines,
        );
        assert_eq!(cycle2.alerts[0].state.phase, Phase::Resolved);
        assert_eq!(cycle2.groups[0].items[0].reason, NotifyReason::Resolved);
    }

    #[test]
    fn une_panne_de_victoriametrics_ne_resout_rien() {
        let cpu = rule("cpu_high", RuleKind::Threshold, 90.0, 0);
        let mut baselines = BaselineStore::default();

        let cycle1 = plan_cycle(
            input(
                at(0),
                vec![RuleObservations { rule: cpu.clone(), series: Some(vec![point(1, 95.0)]) }],
                vec![node(1, None)],
                Vec::new(),
            ),
            &mut baselines,
        );
        let stored = to_stored(&cycle1);

        let cycle2 = plan_cycle(
            input(
                at(60),
                vec![RuleObservations { rule: cpu, series: None }],
                vec![node(1, None)],
                stored.clone(),
            ),
            &mut baselines,
        );
        assert!(cycle2.alerts.is_empty(), "no evaluation");
        assert!(cycle2.groups.is_empty(), "no misleading \"all clear\"");
        assert_eq!(cycle2.carried_over, vec![stored[0].fingerprint.clone()]);
    }

    #[test]
    fn la_panne_d_un_grand_parent_supprime_toute_la_descendance() {
        // box(1) → switch(2) → nas(3)
        let targets = vec![node(1, None), node(2, Some(1)), node(3, Some(2))];
        let host_down = rule(model::RULE_HOST_DOWN, RuleKind::Threshold, 180.0, 0);
        let cpu = rule("cpu_high", RuleKind::Threshold, 90.0, 0);
        let mut baselines = BaselineStore::default();

        let outcome = plan_cycle(
            input(
                at(0),
                vec![
                    RuleObservations {
                        rule: host_down,
                        // Les trois équipements sont muets, mais un seul est en cause.
                        series: Some(vec![point(1, 600.0), point(2, 600.0), point(3, 600.0)]),
                    },
                    RuleObservations { rule: cpu, series: Some(vec![point(3, 99.0)]) },
                ],
                targets,
                Vec::new(),
            ),
            &mut baselines,
        );

        assert_eq!(outcome.groups.len(), 1, "a single notification for the whole cascade");
        assert_eq!(outcome.groups[0].target_name, "device-1", "the root cause");

        let phases: BTreeMap<&str, EffectivePhase> =
            outcome.alerts.iter().map(|a| (a.fingerprint.as_str(), a.effective_phase())).collect();
        assert_eq!(phases.len(), 4);
        let suppressed = phases.values().filter(|p| **p == EffectivePhase::Suppressed).count();
        assert_eq!(suppressed, 3, "switch, nas, and the nas CPU");
    }

    #[test]
    fn un_cycle_de_parente_n_empeche_pas_le_cycle_d_aboutir() {
        // 1 → 2 → 1 : la base l'autorise, le moteur ne doit pas s'y perdre.
        let targets = vec![node(1, Some(2)), node(2, Some(1))];
        let host_down = rule(model::RULE_HOST_DOWN, RuleKind::Threshold, 180.0, 0);
        let mut baselines = BaselineStore::default();

        let outcome = plan_cycle(
            input(
                at(0),
                vec![RuleObservations {
                    rule: host_down,
                    series: Some(vec![point(1, 600.0), point(2, 600.0)]),
                }],
                targets,
                Vec::new(),
            ),
            &mut baselines,
        );
        assert_eq!(outcome.alerts.len(), 2);
        // Chacun est le parent de l'autre et tous deux sont tombés : ils se
        // suppriment mutuellement, mais le cycle se termine — c'est l'essentiel.
        assert!(outcome.alerts.iter().all(|a| a.state.suppressed));
    }

    #[test]
    fn une_fenetre_de_maintenance_fait_taire_sans_arreter_le_suivi() {
        let cpu = rule("cpu_high", RuleKind::Threshold, 90.0, 0);
        let silence = Silence {
            id: 1,
            name: "maintenance".to_string(),
            comment: String::new(),
            target_id: Some(1),
            matchers: BTreeMap::new(),
            schedule: Schedule::Once { starts_at: at(-60), ends_at: at(600) },
            enabled: true,
        };
        let mut baselines = BaselineStore::default();

        let mut cycle_input = input(
            at(0),
            vec![RuleObservations { rule: cpu.clone(), series: Some(vec![point(1, 95.0)]) }],
            vec![node(1, None)],
            Vec::new(),
        );
        cycle_input.silences = vec![silence];
        let pendant = plan_cycle(cycle_input, &mut baselines);

        assert!(pendant.groups.is_empty(), "silent during maintenance");
        assert_eq!(pendant.alerts[0].state.phase, Phase::Firing, "but tracking continues");
        assert!(pendant.alerts[0].state.silenced);

        // Fin de maintenance : l'alerte est notifiée une fois, sans repartir de zéro.
        let apres = plan_cycle(
            input(
                at(700),
                vec![RuleObservations { rule: cpu, series: Some(vec![point(1, 95.0)]) }],
                vec![node(1, None)],
                to_stored(&pendant),
            ),
            &mut baselines,
        );
        assert_eq!(apres.groups.len(), 1);
        assert_eq!(apres.alerts[0].state.firing_since, Some(at(0)), "the age is kept");
    }

    #[test]
    fn une_anomalie_notifie_une_fois_l_apprentissage_termine() {
        let mut anomaly = rule("cpu_anomaly", RuleKind::Anomaly, 0.0, 0);
        // Un seul échantillon suffit à nourrir le seau : le test porte sur la sortie
        // d'apprentissage, pas sur la maturité des seaux.
        anomaly.params.min_samples = 1;
        let mut baselines = BaselineStore::default();
        let start = at(0);

        let premier = plan_cycle(
            input(
                start,
                vec![RuleObservations {
                    rule: anomaly.clone(),
                    series: Some(vec![point(1, 50.0)]),
                }],
                vec![node(1, None)],
                Vec::new(),
            ),
            &mut baselines,
        );
        assert!(premier.alerts[0].state.learning);

        // Quatorze jours plus tard, exactement deux semaines : même seau saisonnier,
        // et l'apprentissage vient de s'achever.
        let now = start + chrono::TimeDelta::days(14);
        let outcome = plan_cycle(
            input(
                now,
                vec![RuleObservations { rule: anomaly, series: Some(vec![point(1, 5_000.0)]) }],
                vec![node(1, None)],
                to_stored(&premier),
            ),
            &mut baselines,
        );
        assert!(!outcome.alerts[0].state.learning, "learning finished");
        assert_eq!(outcome.alerts[0].state.phase, Phase::Firing);
        assert_eq!(outcome.groups.len(), 1, "detection finally speaks up");
        assert!(outcome.alerts[0].score.is_some_and(|s| s > 3.5));
    }

    #[test]
    fn une_anomalie_en_apprentissage_progresse_mais_ne_notifie_pas() {
        let mut anomaly = rule("cpu_anomaly", RuleKind::Anomaly, 0.0, 0);
        anomaly.params.min_samples = 1;
        let mut baselines = BaselineStore::default();
        let start = at(0);

        let premier = plan_cycle(
            input(
                start,
                vec![RuleObservations {
                    rule: anomaly.clone(),
                    series: Some(vec![point(1, 50.0)]),
                }],
                vec![node(1, None)],
                Vec::new(),
            ),
            &mut baselines,
        );

        // Sept jours plus tard : même seau, mais l'apprentissage court toujours.
        let outcome = plan_cycle(
            input(
                start + chrono::TimeDelta::days(7),
                vec![RuleObservations { rule: anomaly, series: Some(vec![point(1, 5_000.0)]) }],
                vec![node(1, None)],
                to_stored(&premier),
            ),
            &mut baselines,
        );
        assert!(outcome.alerts[0].state.learning);
        assert_eq!(
            outcome.alerts[0].state.phase,
            Phase::Firing,
            "the machine advances so the UI shows what would have fired"
        );
        assert!(outcome.groups.is_empty(), "but nothing is notified while learning");
        assert!(outcome.alerts[0].score.is_some_and(|s| s > 3.5));
    }

    #[test]
    fn une_serie_hors_selecteur_est_ignoree() {
        let mut cpu = rule("cpu_high", RuleKind::Threshold, 90.0, 0);
        cpu.selector = TargetSelector::Ids { ids: vec![2] };
        let mut baselines = BaselineStore::default();

        let outcome = plan_cycle(
            input(
                at(0),
                vec![RuleObservations { rule: cpu, series: Some(vec![point(1, 99.0)]) }],
                vec![node(1, None), node(2, None)],
                Vec::new(),
            ),
            &mut baselines,
        );
        assert!(outcome.alerts.is_empty());
    }

    #[test]
    fn une_serie_sans_cible_connue_reste_evaluee_et_notifiee() {
        let cpu = rule("cpu_high", RuleKind::Threshold, 90.0, 0);
        let mut baselines = BaselineStore::default();
        let mut orpheline = point(9, 99.0);
        orpheline.labels.remove("target");
        orpheline.labels.insert("host".to_string(), "unknown".to_string());

        let outcome = plan_cycle(
            input(
                at(0),
                vec![RuleObservations { rule: cpu, series: Some(vec![orpheline]) }],
                Vec::new(),
                Vec::new(),
            ),
            &mut baselines,
        );
        assert_eq!(outcome.alerts.len(), 1);
        assert_eq!(outcome.alerts[0].target_id, None);
        assert_eq!(outcome.groups[0].target_name, "unknown");
    }

    #[test]
    fn les_alertes_d_un_meme_hote_sont_regroupees_en_un_message() {
        let cpu = rule("cpu_high", RuleKind::Threshold, 90.0, 0);
        let disque = rule("disk_almost_full", RuleKind::Threshold, 90.0, 0);
        let mut baselines = BaselineStore::default();

        let outcome = plan_cycle(
            input(
                at(0),
                vec![
                    RuleObservations { rule: cpu, series: Some(vec![point(1, 99.0)]) },
                    RuleObservations { rule: disque, series: Some(vec![point(1, 95.0)]) },
                ],
                vec![node(1, None)],
                Vec::new(),
            ),
            &mut baselines,
        );
        assert_eq!(outcome.groups.len(), 1);
        assert_eq!(outcome.groups[0].items.len(), 2);
    }

    #[test]
    fn une_serie_resolue_par_une_etiquette_instance_avec_port() {
        let cpu = rule("cpu_high", RuleKind::Threshold, 90.0, 0);
        let mut baselines = BaselineStore::default();
        let mut p = point(1, 99.0);
        p.labels.remove("target");
        p.labels.remove("host");
        p.labels.insert("instance".to_string(), "10.0.0.1:161".to_string());

        let outcome = plan_cycle(
            input(
                at(0),
                vec![RuleObservations { rule: cpu, series: Some(vec![p]) }],
                vec![node(1, None)],
                Vec::new(),
            ),
            &mut baselines,
        );
        assert_eq!(outcome.alerts[0].target_id, Some(1));
    }

    #[test]
    fn l_historique_conserve_la_raison_du_mutisme() {
        let targets = vec![node(1, None), node(2, Some(1))];
        let host_down = rule(model::RULE_HOST_DOWN, RuleKind::Threshold, 180.0, 0);
        let mut baselines = BaselineStore::default();

        let outcome = plan_cycle(
            input(
                at(0),
                vec![RuleObservations {
                    rule: host_down,
                    series: Some(vec![point(1, 600.0), point(2, 600.0)]),
                }],
                targets,
                Vec::new(),
            ),
            &mut baselines,
        );
        let supprimee = outcome
            .history
            .iter()
            .find(|entry| entry.target_id == Some(2))
            .expect("logged transition");
        assert!(supprimee.reason.contains("unreachable"), "reason: {}", supprimee.reason);
    }

    #[test]
    fn l_hysteresis_garde_l_alerte_entre_le_seuil_et_le_seuil_de_retour() {
        let mut cpu = rule("cpu_high", RuleKind::Threshold, 90.0, 0);
        cpu.clear_threshold = Some(80.0);
        let mut baselines = BaselineStore::default();

        let obs = |value: f64| RuleObservations {
            rule: cpu.clone(),
            series: Some(vec![point(1, value)]),
        };

        let cycle1 = plan_cycle(
            input(at(0), vec![obs(95.0)], vec![node(1, None)], Vec::new()),
            &mut baselines,
        );
        assert_eq!(cycle1.alerts[0].state.phase, Phase::Firing);

        // 85 % : sous le seuil de déclenchement, au-dessus du seuil de retour →
        // l'alerte tient, aucune résolution.
        let mut stored = to_stored(&cycle1);
        mark_notified(&mut stored, &cycle1, at(0));
        let cycle2 =
            plan_cycle(input(at(30), vec![obs(85.0)], vec![node(1, None)], stored), &mut baselines);
        assert_eq!(cycle2.alerts[0].state.phase, Phase::Firing, "hovering: still firing");
        assert!(cycle2.groups.is_empty());

        // 79 % : sous le seuil de retour → résolution.
        let stored = to_stored(&cycle2);
        let cycle3 =
            plan_cycle(input(at(60), vec![obs(79.0)], vec![node(1, None)], stored), &mut baselines);
        assert_eq!(cycle3.alerts[0].state.phase, Phase::Resolved);
        assert_eq!(cycle3.groups[0].items[0].reason, NotifyReason::Resolved);

        // 85 % après résolution : la condition n'est pas active, pas de redéclenchement.
        let stored = to_stored(&cycle3);
        let cycle4 =
            plan_cycle(input(at(90), vec![obs(85.0)], vec![node(1, None)], stored), &mut baselines);
        assert_eq!(cycle4.alerts[0].state.phase, Phase::Ok);
    }

    #[test]
    fn une_surcharge_par_equipement_change_le_seuil_cite_dans_le_message() {
        let disk = rule("disk", RuleKind::Threshold, 90.0, 0);
        let mut baselines = BaselineStore::default();
        let mut cycle_input = input(
            at(0),
            vec![RuleObservations {
                rule: disk.clone(),
                series: Some(vec![point(1, 93.0), point(2, 93.0)]),
            }],
            vec![node(1, None), node(2, None)],
            Vec::new(),
        );
        cycle_input.overrides = vec![RuleOverride {
            rule_uid: "disk".to_string(),
            target_id: 2,
            threshold: Some(97.0),
            clear_threshold: None,
            enabled: None,
        }];
        let outcome = plan_cycle(cycle_input, &mut baselines);

        let device1 = outcome.alerts.iter().find(|a| a.target_id == Some(1)).unwrap();
        let device2 = outcome.alerts.iter().find(|a| a.target_id == Some(2)).unwrap();
        assert_eq!(device1.state.phase, Phase::Firing);
        assert_eq!(device1.threshold, 90.0);
        assert_eq!(device2.state.phase, Phase::Ok, "97 % override: 93 % is fine");
        assert_eq!(device2.threshold, 97.0, "the override is what the message cites");
    }

    #[test]
    fn une_surcharge_desactivee_resout_l_alerte_en_cours() {
        let disk = rule("disk", RuleKind::Threshold, 90.0, 0);
        let mut baselines = BaselineStore::default();
        let obs = || RuleObservations { rule: disk.clone(), series: Some(vec![point(1, 95.0)]) };

        let cycle1 =
            plan_cycle(input(at(0), vec![obs()], vec![node(1, None)], Vec::new()), &mut baselines);
        assert_eq!(cycle1.alerts[0].state.phase, Phase::Firing);

        let mut stored = to_stored(&cycle1);
        mark_notified(&mut stored, &cycle1, at(0));
        let mut cycle_input = input(at(30), vec![obs()], vec![node(1, None)], stored);
        cycle_input.overrides = vec![RuleOverride {
            rule_uid: "disk".to_string(),
            target_id: 1,
            threshold: None,
            clear_threshold: None,
            enabled: Some(false),
        }];
        let cycle2 = plan_cycle(cycle_input, &mut baselines);
        assert_eq!(cycle2.alerts[0].state.phase, Phase::Resolved, "the series is ignored");
    }
}
