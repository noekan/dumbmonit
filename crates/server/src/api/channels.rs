//! Routes HTTP des canaux de notification.
//!
//! Une seule règle gouverne ce module : rien de ce qui est chiffré en base n'en
//! ressort. La lecture d'un canal ne déchiffre donc jamais ses secrets — elle se
//! contente de dire qu'il en existe un —, et seul l'envoi d'un message de test a
//! besoin du secret en clair, le temps d'une requête.

use std::collections::HashSet;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{Row, SqlitePool};

use crate::api::{ApiError, ApiResult};
use crate::db;
use crate::notify::{self, CHANNEL_KINDS, ChannelConfig, ChannelSummary, DeliveryReport};
use crate::state::AppState;

/// Clés qui désignent un secret quel que soit le type de canal.
///
/// Elles sont refusées dans `settings`, qui est renvoyé tel quel par l'API : une
/// valeur déposée là serait publiée à chaque lecture, alors que l'utilisateur croit
/// l'avoir confiée au serveur. Le message de refus lui indique où la mettre.
const SECRET_KEYS: [&str; 6] =
    ["webhook_url", "token", "bot_token", "password", "api_key", "secret"];

/// Clés supplémentaires à traiter comme secrètes, par type de canal.
///
/// Ce sont les secrets que le catalogue annonce à l'interface pour ce type — l'URL
/// d'un webhook générique, qui porte souvent le jeton dans son chemin, la clé
/// utilisateur de Pushover, les destinations d'Apprise… Les prendre à la source
/// garantit qu'aucun champ présenté comme secret ne peut être enregistré en clair.
fn extra_secret_keys(kind: &str) -> &'static [&'static str] {
    notify::catalog::secret_keys(kind)
}

/// Canal soumis par l'interface.
#[derive(Debug, Deserialize)]
pub struct ChannelPayload {
    pub name: String,
    pub kind: String,
    #[serde(default)]
    pub enabled: Option<bool>,
    /// Réglages non sensibles, renvoyés tels quels par l'API.
    #[serde(default)]
    pub settings: Option<Value>,
    /// Absent ou `null` lors d'une modification signifie « conserver les secrets
    /// enregistrés » — même sémantique que le `credential` d'un équipement, et
    /// c'est ce qui permet de renommer un canal sans ressaisir son jeton. Pour les
    /// effacer, envoyer explicitement un objet vide `{}`.
    #[serde(default)]
    pub secrets: Option<Value>,
}

/// Compte rendu d'un message de test.
#[derive(Debug, Serialize)]
pub struct TestReport {
    pub ok: bool,
    pub message: String,
}

// --------------------------------------------------------------------------
// Lecture
// --------------------------------------------------------------------------

/// Catalogue des types de canaux, avec les champs que chacun attend.
///
/// C'est ce qui permet à l'interface de proposer un formulaire par service sans
/// rien connaître d'eux : la liste des réglages, des secrets, et le lien vers la
/// documentation viennent d'ici.
pub async fn kinds() -> Json<Vec<notify::catalog::KindInfo>> {
    Json(notify::catalog::all())
}

pub async fn list(State(state): State<AppState>) -> ApiResult<Json<Vec<ChannelSummary>>> {
    Ok(Json(summaries(&state.pool, None).await?))
}

/// Identifiants des canaux existants, pour valider les destinataires d'une règle.
///
/// Volontairement sans déchiffrement : vérifier une référence n'exige pas de
/// manipuler le moindre secret.
pub(crate) async fn existing_ids(pool: &SqlitePool) -> ApiResult<HashSet<i64>> {
    let rows = sqlx::query("SELECT id FROM notification_channels").fetch_all(pool).await?;
    Ok(rows.iter().filter_map(|row| row.try_get("id").ok()).collect())
}

/// Résumés des canaux, éventuellement restreints à un identifiant.
///
/// `has_secret` est déduit de la présence de la colonne chiffrée et non d'un
/// déchiffrement réussi : le résumé reste exact même quand le secret d'instance a
/// changé et que le contenu est devenu illisible.
async fn summaries(pool: &SqlitePool, only: Option<i64>) -> ApiResult<Vec<ChannelSummary>> {
    let rows = sqlx::query(
        "SELECT id, name, kind, enabled, settings, secret_enc IS NOT NULL AS has_secret,
                last_error, last_sent_at
         FROM notification_channels
         WHERE (? IS NULL OR id = ?)
         ORDER BY name",
    )
    .bind(only)
    .bind(only)
    .fetch_all(pool)
    .await?;

    rows.iter()
        .map(|row| {
            let settings: String = row.try_get("settings")?;
            Ok(ChannelSummary {
                id: row.try_get("id")?,
                name: row.try_get("name")?,
                kind: row.try_get("kind")?,
                enabled: row.try_get::<i64, _>("enabled")? != 0,
                settings: serde_json::from_str(&settings).unwrap_or(Value::Null),
                has_secret: row.try_get::<i64, _>("has_secret")? != 0,
                last_error: row.try_get("last_error")?,
                last_sent_at: row.try_get("last_sent_at")?,
            })
        })
        .collect::<Result<Vec<_>, sqlx::Error>>()
        .map_err(ApiError::from)
}

async fn summary(pool: &SqlitePool, id: i64) -> ApiResult<ChannelSummary> {
    summaries(pool, Some(id)).await?.into_iter().next().ok_or_else(|| not_found(id))
}

// --------------------------------------------------------------------------
// Écriture
// --------------------------------------------------------------------------

pub async fn create(
    State(state): State<AppState>,
    Json(payload): Json<ChannelPayload>,
) -> ApiResult<(StatusCode, Json<ChannelSummary>)> {
    let draft = payload.validate(None)?;
    let id = db::alerts::upsert_channel(&state.pool, &state.cipher, &draft)
        .await
        .map_err(duplicate_name_to_conflict)?;
    Ok((StatusCode::CREATED, Json(summary(&state.pool, id).await?)))
}

pub async fn update(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(payload): Json<ChannelPayload>,
) -> ApiResult<Json<ChannelSummary>> {
    // Le canal est relu avec ses secrets : sans eux, on ne saurait pas valider la
    // configuration résultante quand la requête n'en fournit pas de nouveaux.
    let current = db::alerts::get_channel(&state.pool, &state.cipher, id)
        .await?
        .ok_or_else(|| not_found(id))?;

    let mut draft = payload.validate(Some(&current))?;
    draft.id = Some(id);

    db::alerts::upsert_channel(&state.pool, &state.cipher, &draft)
        .await
        .map_err(duplicate_name_to_conflict)?;
    Ok(Json(summary(&state.pool, id).await?))
}

/// Supprime un canal, sauf s'il reste destinataire d'une règle.
///
/// Une règle qui pointe vers un canal disparu ne notifie plus rien du tout, et rien
/// ne le signale : mieux vaut refuser ici, en nommant les règles à corriger.
pub async fn delete(State(state): State<AppState>, Path(id): Path<i64>) -> ApiResult<StatusCode> {
    let rules = db::alerts::list_rules(&state.pool).await?;
    let users: Vec<&str> = rules
        .iter()
        .filter(|rule| rule.channels.contains(&id))
        .map(|rule| rule.name.as_str())
        .collect();

    if !users.is_empty() {
        return Err(ApiError::Conflict(format!(
            "This channel is still used by {}. Remove it from those rules before deleting it.",
            users.join(", ")
        )));
    }

    if db::alerts::delete_channel(&state.pool, id).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(not_found(id))
    }
}

/// Envoie un message de test, par le chemin exact de la production.
pub async fn test(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> ApiResult<Json<TestReport>> {
    let config = db::alerts::get_channel(&state.pool, &state.cipher, id)
        .await?
        .ok_or_else(|| not_found(id))?;

    // Un client par appel : l'état partagé n'en porte pas, et un test reste un
    // geste manuel et isolé. Le coût — une poignée de millisecondes pour monter le
    // contexte TLS — est sans conséquence à cette fréquence.
    let http = notify::http_client();
    let result = notify::test_channel(&http, &config).await;
    let error = result.as_ref().err().map(ToString::to_string);

    // Le résultat est consigné comme celui d'un envoi réel : c'est ce que
    // l'interface affiche à côté du canal, et un test doit y laisser sa trace.
    let report =
        DeliveryReport { channel_id: id, channel_name: config.name.clone(), error: error.clone() };
    db::alerts::record_delivery(&state.pool, &report, Utc::now()).await?;

    match error {
        None => Ok(Json(TestReport {
            ok: true,
            message: format!("Test message sent to \"{}\".", config.name),
        })),
        // Déjà expurgée par la couche notification : aucune interpolation de secret
        // n'est possible ici.
        Some(error) => {
            Err(ApiError::BadRequest(format!("Sending to \"{}\" failed: {error}", config.name)))
        }
    }
}

// --------------------------------------------------------------------------
// Validation
// --------------------------------------------------------------------------

impl ChannelPayload {
    /// Traduit la saisie en brouillon prêt à enregistrer.
    ///
    /// `current` porte le canal existant lors d'une modification, secrets compris :
    /// la configuration résultante est éprouvée par le constructeur du notificateur
    /// lui-même, ce qui évite de réécrire — et de laisser diverger — la liste des
    /// réglages obligatoires de chaque service.
    fn validate(self, current: Option<&ChannelConfig>) -> ApiResult<db::alerts::ChannelDraft> {
        let name = self.name.trim().to_string();
        if name.is_empty() {
            return Err(ApiError::BadRequest("Channel name is required.".into()));
        }

        let kind = self.kind.trim().to_string();
        if !CHANNEL_KINDS.contains(&kind.as_str()) {
            return Err(ApiError::BadRequest(format!(
                "Unknown channel type \"{kind}\" (available: {})",
                CHANNEL_KINDS.join(", ")
            )));
        }

        let settings = match self.settings {
            None | Some(Value::Null) => json!({}),
            Some(value) if value.is_object() => value,
            Some(_) => {
                return Err(ApiError::BadRequest("\"settings\" must be a JSON object.".into()));
            }
        };
        reject_secrets_in_settings(&kind, &settings)?;

        let secrets = match self.secrets {
            None => None,
            Some(value) if value.is_object() => Some(value),
            Some(_) => {
                return Err(ApiError::BadRequest(
                    "\"secrets\" must be a JSON object. Omit the field to keep the stored secrets."
                        .into(),
                ));
            }
        };

        // Configuration telle qu'elle vivra après enregistrement, secrets conservés
        // compris : c'est elle qu'il faut éprouver, pas seulement ce qui a été soumis.
        let effective = ChannelConfig {
            id: current.map_or(0, |channel| channel.id),
            name: name.clone(),
            kind: kind.clone(),
            enabled: self.enabled.or(current.map(|c| c.enabled)).unwrap_or(true),
            settings: settings.clone(),
            secrets: secrets
                .clone()
                .or_else(|| current.map(|channel| channel.secrets.clone()))
                .unwrap_or(Value::Null),
        };
        let http = notify::http_client();
        notify::build(&http, &effective)
            .map_err(|error| ApiError::BadRequest(error.to_string()))?;

        Ok(db::alerts::ChannelDraft {
            id: None,
            name,
            kind,
            enabled: effective.enabled,
            settings,
            secrets,
        })
    }
}

fn reject_secrets_in_settings(kind: &str, settings: &Value) -> ApiResult<()> {
    let Some(object) = settings.as_object() else { return Ok(()) };
    for key in SECRET_KEYS.iter().chain(extra_secret_keys(kind)) {
        if object.contains_key(*key) {
            return Err(ApiError::BadRequest(format!(
                "\"{key}\" is a secret: put it in \"secrets\", not in \"settings\", which the API returns in clear text."
            )));
        }
    }
    Ok(())
}

fn not_found(id: i64) -> ApiError {
    ApiError::NotFound(format!("Notification channel {id} not found."))
}

/// `name` est unique en base : le doublon vient de l'utilisateur, pas d'une panne.
fn duplicate_name_to_conflict(error: anyhow::Error) -> ApiError {
    if format!("{error:#}").contains("UNIQUE constraint failed") {
        ApiError::Conflict("A channel with this name already exists.".into())
    } else {
        ApiError::Internal(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload(kind: &str, settings: Value, secrets: Option<Value>) -> ChannelPayload {
        ChannelPayload {
            name: "canal".to_string(),
            kind: kind.to_string(),
            enabled: None,
            settings: Some(settings),
            secrets,
        }
    }

    /// Message d'un refus de validation. `ApiError` n'implémente pas `Debug`, on
    /// l'inspecte donc par filtrage plutôt que par `unwrap_err`.
    fn refus<T>(result: ApiResult<T>) -> String {
        match result {
            Err(ApiError::BadRequest(message) | ApiError::Conflict(message)) => message,
            Err(_) => panic!("une erreur d'utilisateur était attendue"),
            Ok(_) => panic!("la validation aurait dû échouer"),
        }
    }

    #[test]
    fn un_secret_depose_dans_les_reglages_est_refuse() {
        let message = refus(
            payload("discord", json!({ "webhook_url": "https://exemple/x" }), None).validate(None),
        );
        assert!(message.contains("secrets"), "message inattendu : {message}");
        assert!(!message.contains("exemple"), "l'URL a fuité : {message}");
    }

    #[test]
    fn l_url_d_un_webhook_generique_ne_peut_pas_vivre_dans_les_reglages() {
        let message =
            refus(payload("webhook", json!({ "url": "https://exemple/x" }), None).validate(None));
        assert!(message.contains("secrets"), "message inattendu : {message}");
    }

    #[test]
    fn un_type_de_canal_inconnu_est_refuse_avec_la_liste() {
        let message = refus(payload("pigeon-voyageur", json!({}), None).validate(None));
        assert!(message.contains("discord"), "message inattendu : {message}");
    }

    #[test]
    fn une_configuration_incomplete_est_refusee_avant_enregistrement() {
        // ntfy sans sujet : le notificateur lui-même dit ce qui manque.
        let message = refus(payload("ntfy", json!({}), None).validate(None));
        assert!(message.contains("topic"), "message inattendu : {message}");
    }

    #[test]
    fn les_secrets_conserves_valident_la_configuration() {
        let current = ChannelConfig {
            id: 1,
            name: "ancien".to_string(),
            kind: "discord".to_string(),
            enabled: true,
            settings: json!({}),
            secrets: json!({ "webhook_url": "https://exemple.test/hook" }),
        };
        // Renommage sans secrets : la validation s'appuie sur ceux déjà en base, et
        // le brouillon n'en porte aucun, ce qui préserve la colonne chiffrée.
        let Ok(draft) = payload("discord", json!({}), None).validate(Some(&current)) else {
            panic!("le renommage aurait dû être accepté");
        };
        assert!(draft.secrets.is_none(), "les secrets doivent rester en base");
    }
}
