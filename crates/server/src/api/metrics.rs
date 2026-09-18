//! Lecture des séries temporelles.
//!
//! Ces routes relaient les requêtes vers VictoriaMetrics plutôt que de l'exposer
//! directement : l'interface n'a ainsi qu'une seule origine à contacter, et le jour
//! où l'authentification sera en place, elle protégera aussi l'accès aux données.

use axum::Json;
use axum::extract::{Query, State};
use serde::{Deserialize, Serialize};

use crate::api::{ApiError, ApiResult};
use crate::state::AppState;

/// Nombre maximal de points par série renvoyés à l'interface.
///
/// Au-delà, le navigateur peine autant que le réseau, et l'œil ne distingue plus
/// rien : on élargit le pas plutôt que de tout transmettre.
const MAX_POINTS: i64 = 2_000;

/// Longueur maximale d'une expression MetricsQL, en octets.
///
/// Les requêtes de l'interface tiennent en quelques centaines d'octets ; au-delà
/// de quelques kilo-octets, c'est une expression construite pour occuper
/// VictoriaMetrics, pas pour lire un graphe.
const MAX_QUERY_LEN: usize = 4 * 1024;

/// Plage maximale d'une requête sur intervalle : un peu plus d'un an, la
/// rétention par défaut. Plus large, la requête parcourrait des séries qui
/// n'existent pas et le pas serait de toute façon élargi jusqu'à l'absurde.
const MAX_RANGE_MS: i64 = 400 * 24 * 3_600_000;

fn validate_query(query: &str) -> ApiResult<()> {
    if query.trim().is_empty() {
        return Err(ApiError::BadRequest("The query is empty.".into()));
    }
    if query.len() > MAX_QUERY_LEN {
        return Err(ApiError::BadRequest(format!(
            "The query is too long ({} bytes, maximum {MAX_QUERY_LEN}).",
            query.len()
        )));
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
pub struct RangeQuery {
    /// Expression MetricsQL.
    pub query: String,
    /// Bornes en millisecondes depuis l'époque Unix.
    pub start: i64,
    pub end: i64,
    /// Pas d'échantillonnage en secondes. Ajusté si la plage est trop large.
    #[serde(default)]
    pub step: Option<u64>,
}

#[derive(Debug, Serialize)]
pub struct SeriesView {
    pub metric: std::collections::BTreeMap<String, String>,
    /// Couples `[horodatage_secondes, "valeur"]`, au format de l'API Prometheus.
    pub values: Vec<(f64, String)>,
}

pub async fn query_range(
    State(state): State<AppState>,
    Query(params): Query<RangeQuery>,
) -> ApiResult<Json<Vec<SeriesView>>> {
    validate_query(&params.query)?;
    if params.end <= params.start {
        return Err(ApiError::BadRequest("The end of the range must be after its start.".into()));
    }
    if params.end - params.start > MAX_RANGE_MS {
        return Err(ApiError::BadRequest(
            "The range is too wide: query at most 400 days at a time.".into(),
        ));
    }

    let step = resolve_step(params.start, params.end, params.step);

    let series = state
        .victoria
        .query_range(&params.query, params.start, params.end, step)
        .await
        .map_err(|error| ApiError::BadRequest(format!("{error:#}")))?;

    Ok(Json(
        series.into_iter().map(|s| SeriesView { metric: s.metric, values: s.values }).collect(),
    ))
}

#[derive(Debug, Deserialize)]
pub struct InstantQuery {
    pub query: String,
}

pub async fn query(
    State(state): State<AppState>,
    Query(params): Query<InstantQuery>,
) -> ApiResult<Json<Vec<SeriesView>>> {
    validate_query(&params.query)?;

    let series = state
        .victoria
        .query(&params.query)
        .await
        .map_err(|error| ApiError::BadRequest(format!("{error:#}")))?;

    Ok(Json(
        series
            .into_iter()
            .map(|s| SeriesView { metric: s.metric, values: vec![s.value] })
            .collect(),
    ))
}

/// Choisit un pas qui garde le nombre de points sous [`MAX_POINTS`].
///
/// Le pas demandé par l'interface est respecté tant qu'il reste raisonnable ; sur
/// une plage d'un an, il est élargi silencieusement plutôt que de renvoyer une
/// erreur que l'utilisateur ne saurait pas corriger.
fn resolve_step(start_ms: i64, end_ms: i64, requested: Option<u64>) -> u64 {
    let span_secs = ((end_ms - start_ms) / 1000).max(1);
    let minimum = (span_secs / MAX_POINTS).max(1) as u64;
    match requested {
        Some(step) if step >= minimum => step,
        _ => minimum,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOUR: i64 = 3_600_000;

    #[test]
    fn honours_a_reasonable_requested_step() {
        assert_eq!(resolve_step(0, HOUR, Some(60)), 60);
    }

    #[test]
    fn widens_a_step_that_would_return_too_many_points() {
        // Un an à un pas de 10 secondes représenterait plus de trois millions de
        // points ; le pas est élargi pour rester sous la limite.
        let year = 365 * 24 * HOUR;
        let step = resolve_step(0, year, Some(10));
        assert!(step > 10);
        assert!((year / 1000) / step as i64 <= MAX_POINTS);
    }

    #[test]
    fn picks_a_step_on_its_own_when_none_is_given() {
        assert_eq!(resolve_step(0, HOUR, None), 1);
    }

    #[test]
    fn an_oversized_query_is_refused() {
        assert!(validate_query("up").is_ok());
        assert!(validate_query("   ").is_err());
        assert!(validate_query(&"a".repeat(MAX_QUERY_LEN + 1)).is_err());
    }

    #[test]
    fn never_returns_a_zero_step() {
        assert_eq!(resolve_step(0, 1, None), 1);
    }
}
