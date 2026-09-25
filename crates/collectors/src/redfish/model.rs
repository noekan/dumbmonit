//! Lecture tolérante des ressources Redfish.
//!
//! Le schéma DMTF est commun, ses implémentations beaucoup moins : un nombre
//! annoncé `null` (Supermicro, capteur absent), une chaîne là où l'on attend un
//! nombre, une propriété d'une version de schéma que le contrôleur ne connaît pas.
//! Plutôt que des structures `serde` qui échoueraient en bloc sur le premier
//! écart, on lit le JSON champ par champ : une propriété illisible donne `None`
//! et la ressource reste exploitable pour tout le reste.

use serde_json::Value;

/// `Status.Health` et `Status.HealthRollup` (Resource.Health).
///
/// Encodé en jauge 0 / 1 / 2 : c'est l'ordre de gravité du schéma, ce qui permet
/// aux règles d'écrire « ≥ 1 » pour « pas OK » et « ≥ 2 » pour « en panne ».
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Health {
    Ok,
    Warning,
    Critical,
}

impl Health {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "OK" => Some(Self::Ok),
            "Warning" => Some(Self::Warning),
            "Critical" => Some(Self::Critical),
            _ => None,
        }
    }

    pub fn value(self) -> f64 {
        match self {
            Self::Ok => 0.0,
            Self::Warning => 1.0,
            Self::Critical => 2.0,
        }
    }
}

/// Vrai si l'élément doit être surveillé d'après `Status.State`.
///
/// `Absent` : l'emplacement existe mais rien n'y est branché — la deuxième baie
/// d'alimentation d'un serveur livré avec une seule, les emplacements DIMM vides.
/// `Disabled` : désactivé volontairement, comme un groupe de redondance sans objet.
/// Ni l'un ni l'autre ne doit produire de série de santé, sans quoi un serveur
/// sain à moitié peuplé s'afficherait en panne. Tous les autres états
/// (`Enabled`, `StandbyOffline`, `UnavailableOffline`, `Deferring`, `Updating`…)
/// désignent un élément présent, dont la santé compte.
pub fn is_monitored(resource: &Value) -> bool {
    !matches!(
        resource.pointer("/Status/State").and_then(Value::as_str),
        Some("Absent") | Some("Disabled")
    )
}

/// `Status.Health` d'une ressource, si elle est surveillée et la publie.
pub fn health(resource: &Value) -> Option<Health> {
    if !is_monitored(resource) {
        return None;
    }
    resource.pointer("/Status/Health").and_then(Value::as_str).and_then(Health::parse)
}

/// `Status.HealthRollup` : la santé de la ressource et de tout ce qu'elle contient.
pub fn health_rollup(resource: &Value) -> Option<Health> {
    if !is_monitored(resource) {
        return None;
    }
    resource.pointer("/Status/HealthRollup").and_then(Value::as_str).and_then(Health::parse)
}

/// Un nombre, qu'il soit écrit comme tel ou — écart fréquent — comme une chaîne.
pub fn number(resource: &Value, pointer: &str) -> Option<f64> {
    let value = resource.pointer(pointer)?;
    let parsed = match value {
        Value::Number(number) => number.as_f64(),
        Value::String(text) => text.trim().parse().ok(),
        _ => None,
    }?;
    parsed.is_finite().then_some(parsed)
}

/// Une chaîne non vide.
pub fn text<'a>(resource: &'a Value, pointer: &str) -> Option<&'a str> {
    resource.pointer(pointer).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty())
}

/// Le chemin d'un lien `{"@odata.id": …}`.
pub fn link(resource: &Value, pointer: &str) -> Option<String> {
    text(resource, &format!("{pointer}/@odata.id")).map(str::to_string)
}

/// Les chemins d'un tableau de liens : `Members` d'une collection, `Drives`
/// d'un contrôleur de stockage.
pub fn links(resource: &Value, pointer: &str) -> Vec<String> {
    resource
        .pointer(pointer)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.get("@odata.id").and_then(Value::as_str))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Un nom d'élément lisible et stable : `Name`, sinon `Id`, sinon `MemberId`.
///
/// Les capteurs de l'ancien schéma `Thermal` n'ont qu'un `MemberId` souvent
/// numérique ; les contrôleurs modernes ont un `Id` parlant (« CPU1Temp »).
pub fn display_name(resource: &Value) -> String {
    text(resource, "/Name")
        .or_else(|| text(resource, "/Id"))
        .or_else(|| text(resource, "/MemberId"))
        .unwrap_or("unnamed")
        .to_string()
}

/// L'identifiant court d'une ressource : `Id`, sinon le dernier segment du chemin.
pub fn short_id(resource: &Value, path: &str) -> String {
    text(resource, "/Id").map(str::to_string).unwrap_or_else(|| {
        path.trim_end_matches('/').rsplit('/').next().unwrap_or(path).to_string()
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn un_emplacement_absent_na_pas_de_sante() {
        let slot = json!({"Status": {"State": "Absent", "Health": "Critical"}});
        assert_eq!(health(&slot), None, "une baie vide n'est pas une alimentation en panne");
        let disabled = json!({"Status": {"State": "Disabled", "Health": "Warning"}});
        assert_eq!(health(&disabled), None);
    }

    #[test]
    fn un_element_present_garde_sa_sante_quel_que_soit_son_etat() {
        let offline = json!({"Status": {"State": "UnavailableOffline", "Health": "Critical"}});
        assert_eq!(health(&offline), Some(Health::Critical));
        let no_state = json!({"Status": {"Health": "Warning"}});
        assert_eq!(health(&no_state), Some(Health::Warning));
        let enabled =
            json!({"Status": {"State": "Enabled", "Health": "OK", "HealthRollup": "Critical"}});
        assert_eq!(health(&enabled), Some(Health::Ok));
        assert_eq!(health_rollup(&enabled), Some(Health::Critical));
    }

    #[test]
    fn une_sante_inconnue_ou_nulle_ne_produit_rien() {
        assert_eq!(health(&json!({"Status": {"Health": null}})), None);
        assert_eq!(health(&json!({"Status": {"Health": "Unknown"}})), None);
        assert_eq!(health(&json!({})), None);
    }

    #[test]
    fn la_gravite_suit_l_ordre_du_schema() {
        assert!(Health::Ok.value() < Health::Warning.value());
        assert!(Health::Warning.value() < Health::Critical.value());
    }

    #[test]
    fn les_nombres_ecrits_en_chaine_ou_nuls_sont_toleres() {
        let sensor = json!({"Reading": "41.5", "Null": null, "Bad": "n/a", "Ok": 12});
        assert_eq!(number(&sensor, "/Reading"), Some(41.5));
        assert_eq!(number(&sensor, "/Null"), None);
        assert_eq!(number(&sensor, "/Bad"), None);
        assert_eq!(number(&sensor, "/Ok"), Some(12.0));
    }

    #[test]
    fn les_liens_de_collection_sont_extraits() {
        let collection = json!({"Members": [
            {"@odata.id": "/redfish/v1/Chassis/1U"},
            {"@odata.id": "/redfish/v1/Chassis/Blade1"},
            {"pas": "un lien"}
        ]});
        assert_eq!(
            links(&collection, "/Members"),
            vec!["/redfish/v1/Chassis/1U", "/redfish/v1/Chassis/Blade1"]
        );
        assert_eq!(short_id(&json!({}), "/redfish/v1/Chassis/1U"), "1U");
    }
}
