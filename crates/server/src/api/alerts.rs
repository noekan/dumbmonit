//! Routes HTTP du moteur d'alerting : règles, alertes actives, historique, silences.
//!
//! Le module n'abrite aucune logique d'alerting : il traduit du JSON en types du
//! domaine, valide ce que l'utilisateur a saisi, puis délègue à
//! [`crate::db::alerts`]. Les vues de sortie sont des types distincts des types
//! internes, afin qu'une évolution du moteur ne change pas silencieusement le
//! contrat de l'interface web.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::Duration;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use chrono::{DateTime, TimeDelta, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::Row;

use crate::alerting::machine::{EffectivePhase, Phase};
use crate::alerting::model::{
    AnomalyParams, Operator, RULE_HOST_DOWN, Rule, RuleKind, Severity, TargetSelector,
};
use crate::alerting::silence::{MINUTES_PER_DAY, Schedule, Silence};
use crate::api::channels;
use crate::api::{ApiError, ApiResult};
use crate::db;
use crate::state::AppState;

/// Longueur maximale d'un identifiant stable de règle.
///
/// L'`uid` sert de préfixe lisible dans les empreintes et dans les journaux : le
/// borner garde ces derniers exploitables à l'œil nu.
const MAX_UID_LEN: usize = 64;

/// Profondeur d'historique renvoyée quand l'appelant ne précise rien.
const DEFAULT_HISTORY_DAYS: i64 = 7;

/// Nombre maximal de transitions renvoyées en une fois.
///
/// Au-delà, la réponse pèse plus lourd que ce que l'interface sait afficher ; le
/// client affine avec `since` plutôt que de tout rapatrier.
const MAX_HISTORY_LIMIT: i64 = 5_000;
const DEFAULT_HISTORY_LIMIT: i64 = 500;

/// Décalage horaire acceptable pour un silence hebdomadaire, en minutes.
///
/// Les fuseaux réels vont de UTC−12 à UTC+14 ; hors de cette plage, c'est une
/// erreur de saisie et le silence couvrirait une autre tranche horaire que celle
/// que l'utilisateur croit avoir posée.
const MIN_UTC_OFFSET: i32 = -12 * 60;
const MAX_UTC_OFFSET: i32 = 14 * 60;

// --------------------------------------------------------------------------
// Vues
// --------------------------------------------------------------------------

/// Règle telle que l'API la renvoie.
///
/// Les durées sont exposées en secondes plutôt qu'en [`Duration`] : c'est ce que
/// manipulent les formulaires de l'interface, et cela évite une représentation
/// JSON `{secs, nanos}` que personne n'attend.
#[derive(Debug, Serialize)]
pub struct RuleView {
    pub id: i64,
    pub uid: String,
    pub name: String,
    pub description: String,
    pub kind: RuleKind,
    pub query: String,
    pub operator: Operator,
    pub threshold: f64,
    pub for_secs: u64,
    pub severity: Severity,
    pub selector: TargetSelector,
    pub channels: Vec<i64>,
    pub params: AnomalyParams,
    pub unit: String,
    pub repeat_secs: Option<u64>,
    pub escalate_after_secs: Option<u64>,
    pub enabled: bool,
    pub builtin: bool,
}

impl From<Rule> for RuleView {
    fn from(rule: Rule) -> Self {
        Self {
            id: rule.id,
            uid: rule.uid,
            name: rule.name,
            description: rule.description,
            kind: rule.kind,
            query: rule.query,
            operator: rule.operator,
            threshold: rule.threshold,
            for_secs: rule.for_duration.as_secs(),
            severity: rule.severity,
            selector: rule.selector,
            channels: rule.channels,
            params: rule.params,
            unit: rule.unit,
            repeat_secs: rule.repeat_interval.map(|d| d.as_secs()),
            escalate_after_secs: rule.escalate_after.map(|d| d.as_secs()),
            enabled: rule.enabled,
            builtin: rule.builtin,
        }
    }
}

/// Alerte active, telle que l'interface doit l'afficher.
///
/// `phase` et `effective_phase` sont toutes deux exposées : la première dit où en
/// est la machine à états, la seconde ce que l'utilisateur doit lire. Une alerte
/// `firing` mais `suppressed` reste `firing` pour le moteur — c'est bien ce qui
/// permet de ne pas la renotifier à la sortie de la suppression —, alors qu'elle
/// doit s'afficher comme supprimée, avec l'équipement responsable en regard.
#[derive(Debug, Serialize)]
pub struct ActiveAlertView {
    pub fingerprint: String,
    pub rule_uid: String,
    /// Nom courant de la règle, repris de sa définition. Vide si la règle a été
    /// supprimée entre-temps : l'état survit jusqu'à la purge du cycle suivant.
    pub rule_name: String,
    /// Sévérité déclarée par la règle. L'escalade éventuelle ne relève que la
    /// sévérité *notifiée* et n'est pas persistée ici.
    pub severity: Severity,
    pub target_id: Option<i64>,
    pub series_key: String,
    pub labels: BTreeMap<String, String>,
    pub phase: Phase,
    pub effective_phase: EffectivePhase,
    pub suppressed: bool,
    /// Identifiant de l'équipement injoignable qui masque cette alerte.
    pub suppressed_by: Option<i64>,
    pub silenced: bool,
    pub learning: bool,
    pub value: Option<f64>,
    pub score: Option<f64>,
    pub condition_since: Option<DateTime<Utc>>,
    pub firing_since: Option<DateTime<Utc>>,
    pub last_eval_at: Option<DateTime<Utc>>,
    pub last_notified_at: Option<DateTime<Utc>>,
    pub notify_count: u32,
}

/// Transition consignée dans l'historique.
#[derive(Debug, Serialize)]
pub struct HistoryEntryView {
    pub id: i64,
    pub fingerprint: String,
    pub rule_uid: String,
    pub target_id: Option<i64>,
    pub from_phase: Phase,
    pub to_phase: Phase,
    pub severity: Severity,
    pub value: Option<f64>,
    /// Faux quand la transition n'a donné lieu à aucun envoi ; `reason` dit alors
    /// pourquoi (apprentissage, suppression, fenêtre de maintenance).
    pub notified: bool,
    pub reason: String,
    /// Horodatage RFC 3339 en UTC, tel que consigné.
    pub at: String,
}

/// Fenêtre de maintenance telle que l'API la renvoie.
#[derive(Debug, Serialize)]
pub struct SilenceView {
    pub id: i64,
    pub name: String,
    pub comment: String,
    pub target_id: Option<i64>,
    pub matchers: BTreeMap<String, String>,
    pub schedule: Schedule,
    pub enabled: bool,
    /// Vrai si la fenêtre couvre l'instant présent. Calculé ici pour que
    /// l'interface n'ait pas à réimplémenter le calendrier hebdomadaire, dont les
    /// fenêtres à cheval sur minuit sont la partie délicate.
    pub active_now: bool,
}

impl SilenceView {
    fn new(silence: Silence, now: DateTime<Utc>) -> Self {
        let active_now = silence.enabled && silence.schedule.covers(now);
        Self {
            id: silence.id,
            name: silence.name,
            comment: silence.comment,
            target_id: silence.target_id,
            matchers: silence.matchers,
            schedule: silence.schedule,
            enabled: silence.enabled,
            active_now,
        }
    }
}

// --------------------------------------------------------------------------
// Entrées
// --------------------------------------------------------------------------

/// Règle soumise par l'interface.
///
/// Les énumérations arrivent en chaînes libres plutôt que typées par serde : les
/// `parse` du domaine retombent silencieusement sur une valeur par défaut, ce qui
/// est le bon comportement pour relire la base mais le pire pour un formulaire —
/// une sévérité mal orthographiée deviendrait `warning` sans que personne ne le
/// sache. La validation ci-dessous les refuse explicitement.
#[derive(Debug, Deserialize)]
pub struct RulePayload {
    /// Identifiant stable. Absent à la création, il est dérivé du nom.
    #[serde(default)]
    pub uid: Option<String>,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
    pub query: String,
    #[serde(default)]
    pub operator: Option<String>,
    #[serde(default)]
    pub threshold: Option<f64>,
    /// Signé volontairement : une valeur négative doit produire un message clair
    /// plutôt qu'un refus de désérialisation en anglais.
    #[serde(default)]
    pub for_secs: Option<i64>,
    #[serde(default)]
    pub severity: Option<String>,
    #[serde(default)]
    pub selector: Option<Value>,
    #[serde(default)]
    pub channels: Option<Vec<i64>>,
    #[serde(default)]
    pub params: Option<Value>,
    #[serde(default)]
    pub unit: Option<String>,
    /// Absent ou nul : aucun rappel. Zéro est traité de même.
    #[serde(default)]
    pub repeat_secs: Option<i64>,
    #[serde(default)]
    pub escalate_after_secs: Option<i64>,
    #[serde(default)]
    pub enabled: Option<bool>,
}

/// Réglages d'anomalie, chaque champ retombant sur la valeur du domaine.
///
/// L'interface n'expose qu'un ou deux de ces réglages selon l'écran ; exiger
/// l'objet complet la forcerait à recopier des constantes qui ne lui appartiennent
/// pas.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct AnomalyParamsPayload {
    k: Option<f64>,
    alpha: Option<f64>,
    mad_floor_abs: Option<f64>,
    mad_floor_rel: Option<f64>,
    min_samples: Option<u32>,
}

#[derive(Debug, Deserialize)]
pub struct EnablePayload {
    pub enabled: bool,
}

/// Fenêtre de maintenance soumise par l'interface.
#[derive(Debug, Deserialize)]
pub struct SilencePayload {
    pub name: String,
    #[serde(default)]
    pub comment: Option<String>,
    #[serde(default)]
    pub target_id: Option<i64>,
    #[serde(default)]
    pub matchers: BTreeMap<String, String>,
    /// Reçu en JSON brut pour que sa forme soit refusée en français plutôt que par
    /// le message de désérialisation de serde.
    pub schedule: Value,
    #[serde(default)]
    pub enabled: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct HistoryQuery {
    /// Borne basse, au format RFC 3339. Par défaut, les sept derniers jours.
    #[serde(default)]
    pub since: Option<String>,
    #[serde(default)]
    pub limit: Option<i64>,
}

// --------------------------------------------------------------------------
// Règles
// --------------------------------------------------------------------------

pub async fn list_rules(State(state): State<AppState>) -> ApiResult<Json<Vec<RuleView>>> {
    let rules = db::alerts::list_rules(&state.pool).await?;
    Ok(Json(rules.into_iter().map(RuleView::from).collect()))
}

pub async fn create_rule(
    State(state): State<AppState>,
    Json(payload): Json<RulePayload>,
) -> ApiResult<(StatusCode, Json<RuleView>)> {
    let existing = db::alerts::list_rules(&state.pool).await?;
    let mut rule = payload.validate(&state, None, &existing).await?;
    rule.id = db::alerts::upsert_rule(&state.pool, &rule).await?;
    Ok((StatusCode::CREATED, Json(RuleView::from(rule))))
}

pub async fn update_rule(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(payload): Json<RulePayload>,
) -> ApiResult<Json<RuleView>> {
    let existing = db::alerts::list_rules(&state.pool).await?;
    let current =
        existing.iter().find(|rule| rule.id == id).cloned().ok_or_else(|| rule_not_found(id))?;

    let mut rule = payload.validate(&state, Some(&current), &existing).await?;
    rule.id = db::alerts::upsert_rule(&state.pool, &rule).await?;
    Ok(Json(RuleView::from(rule)))
}

/// Supprime une règle créée par l'utilisateur.
///
/// Les règles livrées sont explicitement protégées : les effacer laisserait
/// l'instance sans aucune surveillance, et le seul retour en arrière possible
/// serait un redémarrage du serveur, qui les réinsère. La désactivation couvre le
/// besoin réel — « je ne veux plus de cette alerte » — sans cet effet de bord.
pub async fn delete_rule(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> ApiResult<StatusCode> {
    let rules = db::alerts::list_rules(&state.pool).await?;
    let rule = rules.iter().find(|rule| rule.id == id).ok_or_else(|| rule_not_found(id))?;

    if rule.builtin {
        return Err(ApiError::Conflict(format!(
            "The rule \"{}\" ships with DumbMonit and cannot be deleted. Disable it \
             (POST /api/alerts/rules/{id}/enable with {{\"enabled\": false}}) or adjust \
             its threshold.",
            rule.name
        )));
    }

    if db::alerts::delete_rule(&state.pool, id).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(rule_not_found(id))
    }
}

pub async fn set_rule_enabled(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(payload): Json<EnablePayload>,
) -> ApiResult<Json<RuleView>> {
    if !db::alerts::set_rule_enabled(&state.pool, id, payload.enabled).await? {
        return Err(rule_not_found(id));
    }
    let rules = db::alerts::list_rules(&state.pool).await?;
    let rule = rules.into_iter().find(|rule| rule.id == id).ok_or_else(|| rule_not_found(id))?;
    Ok(Json(RuleView::from(rule)))
}

impl RulePayload {
    /// Traduit la saisie en règle du domaine, ou explique ce qui cloche.
    ///
    /// `current` porte la règle existante lors d'une modification : c'est d'elle que
    /// proviennent l'`uid` et le drapeau `builtin`, jamais du corps de la requête.
    async fn validate(
        self,
        state: &AppState,
        current: Option<&Rule>,
        existing: &[Rule],
    ) -> ApiResult<Rule> {
        let name = self.name.trim().to_string();
        if name.is_empty() {
            return Err(ApiError::BadRequest("Rule name is required.".into()));
        }

        let query = self.query.trim().to_string();
        if query.is_empty() {
            return Err(ApiError::BadRequest(
                "The MetricsQL query is required: without it, the rule watches nothing.".into(),
            ));
        }

        let kind = parse_kind(self.kind.as_deref())?;
        let operator = parse_operator(self.operator.as_deref())?;
        let severity = parse_severity(self.severity.as_deref())?;

        let threshold = self.threshold.unwrap_or(0.0);
        if !threshold.is_finite() {
            return Err(ApiError::BadRequest(
                "The threshold must be a finite number: neither NaN nor infinity can be \
                 compared to a measurement."
                    .into(),
            ));
        }

        let for_duration = parse_duration(self.for_secs, "The hold duration \"for_secs\"")?
            .unwrap_or(Duration::ZERO);
        let repeat_interval =
            parse_duration(self.repeat_secs, "The reminder interval \"repeat_secs\"")?;
        let escalate_after = parse_duration(
            self.escalate_after_secs,
            "The escalation delay \"escalate_after_secs\"",
        )?;

        let selector = parse_selector(self.selector)?;
        let params = parse_params(self.params)?;
        let channels = validate_channels(state, self.channels.unwrap_or_default()).await?;
        let uid = resolve_uid(self.uid.as_deref(), &name, current, existing)?;

        Ok(Rule {
            id: current.map_or(0, |rule| rule.id),
            uid,
            name,
            description: self.description.unwrap_or_default(),
            kind,
            query,
            operator,
            threshold,
            for_duration,
            severity,
            selector,
            channels,
            params,
            unit: self.unit.unwrap_or_default(),
            repeat_interval,
            escalate_after,
            enabled: self.enabled.or(current.map(|rule| rule.enabled)).unwrap_or(true),
            // Le drapeau ne peut pas être posé depuis l'API : il désigne ce que le
            // produit livre, pas ce que l'utilisateur écrit.
            builtin: current.is_some_and(|rule| rule.builtin),
        })
    }
}

fn parse_kind(raw: Option<&str>) -> ApiResult<RuleKind> {
    match raw.map(str::trim) {
        None | Some("") | Some("threshold") => Ok(RuleKind::Threshold),
        Some("anomaly") => Ok(RuleKind::Anomaly),
        Some("predict") => Ok(RuleKind::Predict),
        Some(other) => Err(ApiError::BadRequest(format!(
            "Unknown rule type \"{other}\" (expected: threshold, anomaly, predict)."
        ))),
    }
}

fn parse_operator(raw: Option<&str>) -> ApiResult<Operator> {
    match raw.map(str::trim) {
        None | Some("") | Some(">") => Ok(Operator::Gt),
        Some(">=") => Ok(Operator::Ge),
        Some("<") => Ok(Operator::Lt),
        Some("<=") => Ok(Operator::Le),
        Some(other) => Err(ApiError::BadRequest(format!(
            "Invalid operator \"{other}\" (expected: >, >=, <, <=)."
        ))),
    }
}

fn parse_severity(raw: Option<&str>) -> ApiResult<Severity> {
    match raw.map(str::trim) {
        None | Some("") | Some("warning") => Ok(Severity::Warning),
        Some("info") => Ok(Severity::Info),
        Some("critical") => Ok(Severity::Critical),
        Some(other) => Err(ApiError::BadRequest(format!(
            "Unknown severity \"{other}\" (expected: info, warning, critical)."
        ))),
    }
}

/// Durée en secondes, refusée si négative. Zéro équivaut à « pas de durée ».
fn parse_duration(secs: Option<i64>, label: &str) -> ApiResult<Option<Duration>> {
    match secs {
        None => Ok(None),
        Some(value) if value < 0 => {
            Err(ApiError::BadRequest(format!("{label} cannot be negative.")))
        }
        Some(0) => Ok(None),
        Some(value) => Ok(Some(Duration::from_secs(value as u64))),
    }
}

fn parse_selector(raw: Option<Value>) -> ApiResult<TargetSelector> {
    let Some(value) = raw.filter(|value| !value.is_null()) else {
        return Ok(TargetSelector::All);
    };
    // Le détail de serde est en anglais et cite des noms de variantes Rust : on lui
    // substitue les trois formes que l'utilisateur a effectivement le droit d'écrire.
    serde_json::from_value(value).map_err(|_| {
        ApiError::BadRequest(
            "Invalid device selector. Accepted forms: {\"kind\":\"all\"}, \
             {\"kind\":\"ids\",\"ids\":[1,2]} or \
             {\"kind\":\"labels\",\"labels\":{\"role\":\"nas\"}}."
                .into(),
        )
    })
}

fn parse_params(raw: Option<Value>) -> ApiResult<AnomalyParams> {
    let Some(value) = raw.filter(|value| !value.is_null()) else {
        return Ok(AnomalyParams::default());
    };

    let payload: AnomalyParamsPayload = serde_json::from_value(value).map_err(|_| {
        ApiError::BadRequest(
            "Invalid anomaly settings. \"k\", \"alpha\", \"mad_floor_abs\" and \
             \"mad_floor_rel\" must be numbers, \"min_samples\" a positive integer."
                .into(),
        )
    })?;

    let defaults = AnomalyParams::default();
    let params = AnomalyParams {
        k: payload.k.unwrap_or(defaults.k),
        alpha: payload.alpha.unwrap_or(defaults.alpha),
        mad_floor_abs: payload.mad_floor_abs.unwrap_or(defaults.mad_floor_abs),
        mad_floor_rel: payload.mad_floor_rel.unwrap_or(defaults.mad_floor_rel),
        min_samples: payload.min_samples.unwrap_or(defaults.min_samples),
    };

    if !(params.k.is_finite() && params.k > 0.0) {
        return Err(ApiError::BadRequest(
            "\"k\" must be a strictly positive number: it is the number of robust \
             deviations beyond which a point is considered abnormal."
                .into(),
        ));
    }
    if !(params.alpha.is_finite() && params.alpha > 0.0 && params.alpha <= 1.0) {
        return Err(ApiError::BadRequest(
            "\"alpha\" must be between 0 (exclusive) and 1 (inclusive).".into(),
        ));
    }
    if !(params.mad_floor_abs.is_finite() && params.mad_floor_abs > 0.0) {
        return Err(ApiError::BadRequest(
            "\"mad_floor_abs\" must be strictly positive: without a floor, a perfectly \
             flat series would score any deviation as infinite."
                .into(),
        ));
    }
    if !(params.mad_floor_rel.is_finite() && params.mad_floor_rel >= 0.0) {
        return Err(ApiError::BadRequest("\"mad_floor_rel\" cannot be negative.".into()));
    }
    if params.min_samples == 0 {
        return Err(ApiError::BadRequest(
            "\"min_samples\" must be at least 1: scoring an empty bucket makes no sense.".into(),
        ));
    }
    Ok(params)
}

/// Vérifie que chaque canal destinataire existe, et retire les doublons.
///
/// Un identifiant fantôme rendrait la règle muette sans le moindre message : le
/// moteur n'enverrait rien puisqu'aucun canal ne correspondrait, et l'utilisateur
/// conclurait que l'alerting est en panne.
async fn validate_channels(state: &AppState, wanted: Vec<i64>) -> ApiResult<Vec<i64>> {
    if wanted.is_empty() {
        return Ok(Vec::new());
    }

    let known = channels::existing_ids(&state.pool).await?;
    let mut seen = HashSet::new();
    let mut channels = Vec::with_capacity(wanted.len());
    for id in wanted {
        if !known.contains(&id) {
            return Err(ApiError::BadRequest(format!(
                "Notification channel {id} does not exist. Create it first, or leave \
                 \"channels\" empty to notify every enabled channel."
            )));
        }
        if seen.insert(id) {
            channels.push(id);
        }
    }
    Ok(channels)
}

/// Détermine l'identifiant stable d'une règle.
///
/// Il est immuable après création : c'est lui qui relie une règle à ses empreintes
/// et à son historique. Le renommer résoudrait toutes ses alertes en cours puis les
/// ferait renaître, ce que personne ne demande en corrigeant un libellé.
fn resolve_uid(
    submitted: Option<&str>,
    name: &str,
    current: Option<&Rule>,
    existing: &[Rule],
) -> ApiResult<String> {
    if let Some(current) = current {
        return match submitted.map(str::trim).filter(|uid| !uid.is_empty()) {
            Some(uid) if uid != current.uid => Err(ApiError::BadRequest(format!(
                "The stable identifier of a rule cannot be changed (\"{}\" here): it links \
                 the rule to its history and its active alerts.",
                current.uid
            ))),
            _ => Ok(current.uid.clone()),
        };
    }

    let taken: HashSet<&str> = existing.iter().map(|rule| rule.uid.as_str()).collect();

    match submitted.map(str::trim).filter(|uid| !uid.is_empty()) {
        Some(uid) => {
            check_uid_shape(uid)?;
            if taken.contains(uid) {
                return Err(ApiError::Conflict(format!(
                    "A rule already uses the identifier \"{uid}\"."
                )));
            }
            Ok(uid.to_string())
        }
        None => {
            let base = slugify(name);
            if base.is_empty() {
                return Err(ApiError::BadRequest(
                    "Cannot derive an identifier from the name: add at least one letter \
                     or digit, or provide \"uid\" explicitly."
                        .into(),
                ));
            }
            check_uid_shape(&base)?;
            // Un suffixe numérique plutôt qu'un refus : l'utilisateur n'a pas choisi
            // cet identifiant, le lui reprocher n'aurait aucun sens.
            if !taken.contains(base.as_str()) {
                return Ok(base);
            }
            (2..1000)
                .map(|suffix| format!("{base}_{suffix}"))
                .find(|candidate| !taken.contains(candidate.as_str()))
                .ok_or_else(|| {
                    ApiError::Conflict(format!(
                        "Too many rules have a name close to \"{name}\". Provide \"uid\" \
                         explicitly."
                    ))
                })
        }
    }
}

fn check_uid_shape(uid: &str) -> ApiResult<()> {
    if uid.len() > MAX_UID_LEN {
        return Err(ApiError::BadRequest(format!(
            "A rule identifier is limited to {MAX_UID_LEN} characters."
        )));
    }
    if !uid.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-') {
        return Err(ApiError::BadRequest(format!(
            "The identifier \"{uid}\" must only contain unaccented lowercase letters, \
             digits, hyphens and underscores."
        )));
    }
    // `host_down` désigne nommément la règle qui définit « équipement injoignable »,
    // racine de la suppression par dépendance : la laisser porter autre chose
    // couperait toutes les alertes des équipements situés derrière.
    if uid == RULE_HOST_DOWN {
        return Err(ApiError::BadRequest(format!(
            "The identifier \"{RULE_HOST_DOWN}\" is reserved for the \"Device unreachable\" \
             rule, on which dependency suppression relies."
        )));
    }
    Ok(())
}

/// Identifiant lisible dérivé d'un nom saisi en français.
///
/// Les accents sont repliés plutôt que remplacés par un tiret bas : « Mémoire
/// saturée » doit donner `memoire_saturee`, pas `m_moire_satur_e`.
fn slugify(name: &str) -> String {
    let mut slug = String::with_capacity(name.len());
    for raw in name.chars() {
        let folded = fold_accent(raw);
        if folded.is_ascii_alphanumeric() {
            slug.push(folded.to_ascii_lowercase());
        } else if !slug.ends_with('_') {
            slug.push('_');
        }
    }
    let slug = slug.trim_matches('_').to_string();
    slug.chars().take(MAX_UID_LEN).collect()
}

fn fold_accent(c: char) -> char {
    match c {
        'à' | 'â' | 'ä' | 'á' | 'ã' | 'å' => 'a',
        'ç' => 'c',
        'è' | 'é' | 'ê' | 'ë' => 'e',
        'ì' | 'í' | 'î' | 'ï' => 'i',
        'ñ' => 'n',
        'ò' | 'ó' | 'ô' | 'ö' | 'õ' => 'o',
        'ù' | 'ú' | 'û' | 'ü' => 'u',
        'ý' | 'ÿ' => 'y',
        'À' | 'Â' | 'Ä' | 'Á' | 'Ã' | 'Å' => 'A',
        'Ç' => 'C',
        'È' | 'É' | 'Ê' | 'Ë' => 'E',
        'Ì' | 'Í' | 'Î' | 'Ï' => 'I',
        'Ñ' => 'N',
        'Ò' | 'Ó' | 'Ô' | 'Ö' | 'Õ' => 'O',
        'Ù' | 'Ú' | 'Û' | 'Ü' => 'U',
        'Ý' => 'Y',
        other => other,
    }
}

fn rule_not_found(id: i64) -> ApiError {
    ApiError::NotFound(format!("Alert rule {id} not found."))
}

// --------------------------------------------------------------------------
// Alertes actives et historique
// --------------------------------------------------------------------------

pub async fn list_active(State(state): State<AppState>) -> ApiResult<Json<Vec<ActiveAlertView>>> {
    let alerts = db::alerts::list_active(&state.pool).await?;
    let rules: HashMap<String, Rule> = db::alerts::list_rules(&state.pool)
        .await?
        .into_iter()
        .map(|rule| (rule.uid.clone(), rule))
        .collect();

    Ok(Json(
        alerts
            .into_iter()
            .map(|alert| {
                let rule = rules.get(&alert.rule_uid);
                ActiveAlertView {
                    effective_phase: alert.state.effective_phase(),
                    phase: alert.state.phase,
                    rule_name: rule.map(|rule| rule.name.clone()).unwrap_or_default(),
                    severity: rule.map_or(Severity::Warning, |rule| rule.severity),
                    fingerprint: alert.fingerprint,
                    rule_uid: alert.rule_uid,
                    target_id: alert.target_id,
                    series_key: alert.series_key,
                    labels: alert.labels,
                    suppressed: alert.state.suppressed,
                    suppressed_by: alert.state.suppressed_by,
                    silenced: alert.state.silenced,
                    learning: alert.state.learning,
                    value: alert.state.value,
                    score: alert.state.score,
                    condition_since: alert.state.condition_since,
                    firing_since: alert.state.firing_since,
                    last_eval_at: alert.state.last_eval_at,
                    last_notified_at: alert.state.last_notified_at,
                    notify_count: alert.state.notify_count,
                }
            })
            .collect(),
    ))
}

/// Historique des transitions, de la plus récente à la plus ancienne.
///
/// La lecture est écrite ici et non dans [`crate::db::alerts`] : ce module n'expose
/// que les accès dont le moteur a besoin, et l'historique n'est lu que par l'API.
pub async fn history(
    State(state): State<AppState>,
    Query(params): Query<HistoryQuery>,
) -> ApiResult<Json<Vec<HistoryEntryView>>> {
    let since = match params.since.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(raw) => {
            DateTime::parse_from_rfc3339(raw).map(|at| at.with_timezone(&Utc)).map_err(|_| {
                ApiError::BadRequest(format!(
                    "\"since\" must be an RFC 3339 date, for example \
                     2026-08-31T12:00:00Z (received: \"{raw}\")."
                ))
            })?
        }
        None => Utc::now() - TimeDelta::days(DEFAULT_HISTORY_DAYS),
    };

    let limit = match params.limit {
        Some(value) if value <= 0 => {
            return Err(ApiError::BadRequest(
                "\"limit\" must be a strictly positive integer.".into(),
            ));
        }
        Some(value) => value.min(MAX_HISTORY_LIMIT),
        None => DEFAULT_HISTORY_LIMIT,
    };

    let rows = sqlx::query(
        "SELECT id, fingerprint, rule_uid, target_id, from_phase, to_phase, severity, value,
                notified, reason, at
         FROM alert_history
         WHERE at >= ?
         ORDER BY at DESC, id DESC
         LIMIT ?",
    )
    .bind(since.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
    .bind(limit)
    .fetch_all(&state.pool)
    .await
    .map_err(anyhow::Error::from)?;

    let entries = rows
        .iter()
        .map(|row| {
            let from_phase: String = row.try_get("from_phase")?;
            let to_phase: String = row.try_get("to_phase")?;
            let severity: String = row.try_get("severity")?;
            Ok(HistoryEntryView {
                id: row.try_get("id")?,
                fingerprint: row.try_get("fingerprint")?,
                rule_uid: row.try_get("rule_uid")?,
                target_id: row.try_get("target_id")?,
                from_phase: Phase::parse(&from_phase),
                to_phase: Phase::parse(&to_phase),
                severity: Severity::parse(&severity),
                value: row.try_get("value")?,
                notified: row.try_get::<i64, _>("notified")? != 0,
                reason: row.try_get("reason")?,
                at: row.try_get("at")?,
            })
        })
        .collect::<Result<Vec<_>, sqlx::Error>>()
        .map_err(anyhow::Error::from)?;

    Ok(Json(entries))
}

// --------------------------------------------------------------------------
// Silences
// --------------------------------------------------------------------------

pub async fn list_silences(State(state): State<AppState>) -> ApiResult<Json<Vec<SilenceView>>> {
    let now = Utc::now();
    let silences = db::alerts::list_silences(&state.pool).await?;
    Ok(Json(silences.into_iter().map(|s| SilenceView::new(s, now)).collect()))
}

pub async fn create_silence(
    State(state): State<AppState>,
    Json(payload): Json<SilencePayload>,
) -> ApiResult<(StatusCode, Json<SilenceView>)> {
    let mut silence = payload.validate(&state).await?;
    silence.id = db::alerts::create_silence(&state.pool, &silence).await?;
    Ok((StatusCode::CREATED, Json(SilenceView::new(silence, Utc::now()))))
}

pub async fn delete_silence(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> ApiResult<StatusCode> {
    if db::alerts::delete_silence(&state.pool, id).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::NotFound(format!("Maintenance window {id} not found.")))
    }
}

impl SilencePayload {
    async fn validate(self, state: &AppState) -> ApiResult<Silence> {
        let name = self.name.trim().to_string();
        if name.is_empty() {
            return Err(ApiError::BadRequest(
                "Maintenance window name is required: six months from now, it is what \
                 explains why these alerts are silent."
                    .into(),
            ));
        }

        if let Some(target_id) = self.target_id {
            let exists = sqlx::query("SELECT 1 FROM targets WHERE id = ?")
                .bind(target_id)
                .fetch_optional(&state.pool)
                .await
                .map_err(anyhow::Error::from)?
                .is_some();
            if !exists {
                return Err(ApiError::BadRequest(format!("Device {target_id} does not exist.")));
            }
        }

        Ok(Silence {
            id: 0,
            name,
            comment: self.comment.unwrap_or_default(),
            target_id: self.target_id,
            matchers: self.matchers,
            schedule: parse_schedule(self.schedule)?,
            enabled: self.enabled.unwrap_or(true),
        })
    }
}

fn parse_schedule(raw: Value) -> ApiResult<Schedule> {
    let schedule: Schedule = serde_json::from_value(raw).map_err(|_| {
        ApiError::BadRequest(
            "Invalid schedule. Accepted forms: \
             {\"kind\":\"once\",\"starts_at\":\"2026-01-01T00:00:00Z\",\
             \"ends_at\":\"2026-01-01T02:00:00Z\"} or \
             {\"kind\":\"weekly\",\"days\":[6],\"start_minute\":120,\"end_minute\":240,\
             \"utc_offset_minutes\":60}"
                .into(),
        )
    })?;

    match &schedule {
        Schedule::Once { starts_at, ends_at } => {
            if ends_at <= starts_at {
                return Err(ApiError::BadRequest(
                    "The end of the window must be after its start.".into(),
                ));
            }
        }
        Schedule::Weekly { days, start_minute, end_minute, utc_offset_minutes } => {
            if days.is_empty() {
                return Err(ApiError::BadRequest(
                    "Select at least one day of the week (0 = Monday … 6 = Sunday).".into(),
                ));
            }
            if let Some(day) = days.iter().find(|day| **day > 6) {
                return Err(ApiError::BadRequest(format!(
                    "Invalid day of the week \"{day}\": days go from 0 (Monday) to 6 (Sunday)."
                )));
            }
            for (label, minute) in [("start_minute", *start_minute), ("end_minute", *end_minute)] {
                if minute >= MINUTES_PER_DAY {
                    return Err(ApiError::BadRequest(format!(
                        "\"{label}\" must be between 0 and {} (minutes since midnight).",
                        MINUTES_PER_DAY - 1
                    )));
                }
            }
            // Début et fin confondus feraient une fenêtre couvrant la journée
            // entière — un silence permanent posé par accident, exactement ce qu'un
            // outil de supervision ne doit jamais faire en silence.
            if start_minute == end_minute {
                return Err(ApiError::BadRequest(
                    "The start and end of the window must differ. To silence an alert \
                     permanently, disable the rule instead."
                        .into(),
                ));
            }
            if !(MIN_UTC_OFFSET..=MAX_UTC_OFFSET).contains(utc_offset_minutes) {
                return Err(ApiError::BadRequest(format!(
                    "\"utc_offset_minutes\" must be between {MIN_UTC_OFFSET} and \
                     {MAX_UTC_OFFSET}."
                )));
            }
        }
    }

    Ok(schedule)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Message d'un refus de validation.
    ///
    /// `ApiError` n'implémente pas `Debug` — c'est voulu, une erreur d'API n'a pas à
    /// être imprimable telle quelle : on l'inspecte donc par filtrage.
    fn refus<T>(result: ApiResult<T>) -> String {
        match result {
            Err(ApiError::BadRequest(message) | ApiError::Conflict(message)) => message,
            Err(_) => panic!("une erreur d'utilisateur était attendue"),
            Ok(_) => panic!("la validation aurait dû échouer"),
        }
    }

    fn accepte<T>(result: ApiResult<T>) -> T {
        match result {
            Ok(value) => value,
            Err(
                ApiError::BadRequest(message)
                | ApiError::Conflict(message)
                | ApiError::NotFound(message),
            ) => panic!("refus inattendu : {message}"),
            Err(_) => panic!("erreur interne inattendue"),
        }
    }

    #[test]
    fn un_nom_francais_donne_un_identifiant_lisible() {
        assert_eq!(slugify("Mémoire saturée"), "memoire_saturee");
        assert_eq!(slugify("CPU > 90 %"), "cpu_90");
        assert_eq!(slugify("  ---  "), "");
    }

    #[test]
    fn les_enumerations_mal_orthographiees_sont_refusees() {
        assert!(refus(parse_severity(Some("warnning"))).contains("severity"));
        assert!(refus(parse_operator(Some("=>"))).contains("operator"));
        assert!(refus(parse_kind(Some("seuil"))).contains("rule type"));
        // L'absence de valeur reste acceptée : elle vaut le défaut du domaine.
        assert_eq!(accepte(parse_severity(None)), Severity::Warning);
        assert_eq!(accepte(parse_operator(None)), Operator::Gt);
        assert_eq!(accepte(parse_kind(None)), RuleKind::Threshold);
    }

    #[test]
    fn une_duree_negative_est_refusee() {
        assert!(refus(parse_duration(Some(-1), "The duration")).contains("negative"));
        assert_eq!(accepte(parse_duration(Some(0), "The duration")), None);
        assert_eq!(
            accepte(parse_duration(Some(30), "The duration")),
            Some(Duration::from_secs(30))
        );
    }

    #[test]
    fn l_identifiant_reserve_de_la_regle_injoignable_est_protege() {
        assert!(refus(check_uid_shape(RULE_HOST_DOWN)).contains("reserved"));
        assert!(refus(check_uid_shape("Accentué")).contains("lowercase"));
    }

    #[test]
    fn une_fenetre_hebdomadaire_de_duree_nulle_est_refusee() {
        let raw = serde_json::json!({
            "kind": "weekly", "days": [0], "start_minute": 120, "end_minute": 120
        });
        assert!(refus(parse_schedule(raw)).contains("must differ"));
    }

    #[test]
    fn une_fenetre_ponctuelle_inversee_est_refusee() {
        let raw = serde_json::json!({
            "kind": "once",
            "starts_at": "2026-01-01T02:00:00Z",
            "ends_at": "2026-01-01T01:00:00Z"
        });
        assert!(refus(parse_schedule(raw)).contains("after its start"));
    }

    #[test]
    fn les_reglages_d_anomalie_incoherents_sont_refuses() {
        assert!(refus(parse_params(Some(serde_json::json!({"alpha": 0})))).contains("alpha"));
        assert!(refus(parse_params(Some(serde_json::json!({"k": -1})))).contains("k"));
        assert!(
            refus(parse_params(Some(serde_json::json!({"min_samples": 0}))))
                .contains("min_samples")
        );
        // Un objet partiel complète avec les valeurs du domaine.
        let params = accepte(parse_params(Some(serde_json::json!({"k": 4.0}))));
        assert_eq!(params.k, 4.0);
        assert_eq!(params.alpha, AnomalyParams::default().alpha);
    }
}
