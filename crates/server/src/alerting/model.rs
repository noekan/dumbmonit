//! Types du domaine de l'alerting : règles, sévérités, phases, empreintes.
//!
//! Ce module est volontairement dépourvu d'entrées/sorties : il est partagé par le
//! moteur, la couche base de données et l'API, et reste testable sans réseau.

use std::collections::BTreeMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Identifiant d'une cible, repris de la table `targets`.
pub type TargetId = i64;

/// `uid` de la règle « équipement injoignable ».
///
/// Le moteur a besoin de la reconnaître nominativement : c'est elle qui définit
/// l'ensemble des cibles considérées comme hors ligne, donc la racine de la
/// suppression par dépendance.
pub const RULE_HOST_DOWN: &str = "host_down";

/// Étiquettes candidates pour rattacher une série à une cible, par ordre de
/// confiance décroissante.
///
/// Le collecteur pose les étiquettes d'identité via `Target::base_labels()`, dont
/// la composition exacte relève d'un autre jalon : accepter plusieurs conventions
/// évite de coupler le moteur d'alerting à ce détail.
pub const TARGET_ID_LABELS: [&str; 2] = ["target", "target_id"];
pub const TARGET_NAME_LABELS: [&str; 2] = ["host", "hostname"];
pub const TARGET_ADDRESS_LABELS: [&str; 2] = ["instance", "address"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Info,
    #[default]
    Warning,
    Critical,
}

impl Severity {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Warning => "warning",
            Self::Critical => "critical",
        }
    }

    /// Sévérité immédiatement supérieure, utilisée par l'escalade. `Critical` est un
    /// point fixe : au-delà, il n'y a plus rien à dire de plus fort.
    pub fn escalated(self) -> Self {
        match self {
            Self::Info => Self::Warning,
            Self::Warning | Self::Critical => Self::Critical,
        }
    }

    pub fn parse(raw: &str) -> Self {
        match raw {
            "info" => Self::Info,
            "critical" => Self::Critical,
            _ => Self::Warning,
        }
    }
}

impl std::fmt::Display for Severity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RuleKind {
    /// Comparaison directe du résultat de la requête à un seuil.
    Threshold,
    /// Écart à la baseline saisonnière, jalon 5.
    Anomaly,
    /// Extrapolation `predict_linear()`, entièrement calculée par VictoriaMetrics.
    Predict,
}

impl RuleKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Threshold => "threshold",
            Self::Anomaly => "anomaly",
            Self::Predict => "predict",
        }
    }

    pub fn parse(raw: &str) -> Self {
        match raw {
            "anomaly" => Self::Anomaly,
            "predict" => Self::Predict,
            _ => Self::Threshold,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Operator {
    #[serde(rename = ">")]
    Gt,
    #[serde(rename = ">=")]
    Ge,
    #[serde(rename = "<")]
    Lt,
    #[serde(rename = "<=")]
    Le,
}

impl Operator {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Gt => ">",
            Self::Ge => ">=",
            Self::Lt => "<",
            Self::Le => "<=",
        }
    }

    pub fn parse(raw: &str) -> Self {
        match raw {
            ">=" => Self::Ge,
            "<" => Self::Lt,
            "<=" => Self::Le,
            _ => Self::Gt,
        }
    }

    /// Applique l'opérateur. Une valeur non finie n'est jamais une violation : c'est
    /// un trou de collecte, pas un dépassement de seuil.
    pub fn test(self, value: f64, threshold: f64) -> bool {
        if !value.is_finite() {
            return false;
        }
        match self {
            Self::Gt => value > threshold,
            Self::Ge => value >= threshold,
            Self::Lt => value < threshold,
            Self::Le => value <= threshold,
        }
    }
}

impl std::fmt::Display for Operator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Portée d'une règle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum TargetSelector {
    /// Toutes les cibles. C'est le défaut des règles livrées : le produit doit
    /// alerter utilement sans que personne n'ait rien configuré.
    #[default]
    All,
    Ids {
        ids: Vec<TargetId>,
    },
    /// Correspondance exacte sur les étiquettes de la cible (ses `tags`).
    Labels {
        labels: BTreeMap<String, String>,
    },
}

impl TargetSelector {
    /// Une série sans cible identifiée n'est retenue que par le sélecteur `All` :
    /// on ne peut pas prétendre qu'elle correspond à une liste ou à des étiquettes
    /// qu'on est incapable de lire.
    pub fn matches(&self, target: Option<&TargetNode>) -> bool {
        match self {
            Self::All => true,
            Self::Ids { ids } => target.is_some_and(|t| ids.contains(&t.id)),
            Self::Labels { labels } => target
                .is_some_and(|t| labels.iter().all(|(key, value)| t.tags.get(key) == Some(value))),
        }
    }
}

/// Réglages de la détection d'anomalie, stockés dans `alert_rules.params`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct AnomalyParams {
    /// Seuil du score robuste au-delà duquel le point est jugé anormal.
    pub k: f64,
    /// Facteur de lissage de l'EWMA. 0,05 ≈ une constante de temps de vingt points
    /// dans le seau, soit une vingtaine de semaines : la baseline suit les dérives
    /// saisonnières sans se laisser réécrire par un incident isolé.
    pub alpha: f64,
    /// Plancher absolu de l'échelle. Obligatoire : sans lui, une série parfaitement
    /// plate a une MAD nulle, et tout écart, si minime soit-il, donne un score infini.
    pub mad_floor_abs: f64,
    /// Plancher relatif au niveau de la baseline, pour que le plancher garde un sens
    /// quelle que soit l'unité (des octets par seconde et des degrés Celsius
    /// n'ont pas la même échelle naturelle).
    pub mad_floor_rel: f64,
    /// Nombre minimal d'observations dans le seau avant de scorer.
    pub min_samples: u32,
}

impl Default for AnomalyParams {
    fn default() -> Self {
        Self { k: 3.5, alpha: 0.05, mad_floor_abs: 1e-6, mad_floor_rel: 0.01, min_samples: 3 }
    }
}

/// Règle d'alerte telle qu'utilisée par le moteur.
#[derive(Debug, Clone, PartialEq)]
pub struct Rule {
    pub id: i64,
    pub uid: String,
    pub name: String,
    pub description: String,
    pub kind: RuleKind,
    /// Expression MetricsQL évaluée en instantané. Pour une règle prédictive, c'est
    /// un `predict_linear(...)` complet : aucun calcul n'est refait côté Rust.
    pub query: String,
    pub operator: Operator,
    pub threshold: f64,
    /// Durée pendant laquelle la condition doit tenir avant de déclencher.
    pub for_duration: Duration,
    pub severity: Severity,
    pub selector: TargetSelector,
    pub channels: Vec<i64>,
    pub params: AnomalyParams,
    /// Suffixe d'affichage dans les messages : « % », « °C », « Go »…
    pub unit: String,
    /// Période de rappel tant que l'alerte reste active. `None` : pas de rappel.
    pub repeat_interval: Option<Duration>,
    /// Délai au-delà duquel la sévérité notifiée est relevée d'un cran.
    pub escalate_after: Option<Duration>,
    pub enabled: bool,
    pub builtin: bool,
}

impl Rule {
    /// La règle qui définit « cet équipement est hors ligne ».
    pub fn is_host_down(&self) -> bool {
        self.uid == RULE_HOST_DOWN
    }
}

/// Vue minimale d'une cible, suffisante pour l'alerting.
///
/// Le moteur ne dépend volontairement pas de `ezymonit_proto::Target` : il n'a
/// besoin que de la filiation et des étiquettes, et les charger lui-même évite de
/// déchiffrer des identifiants d'équipement qu'il n'utilisera jamais.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TargetNode {
    pub id: TargetId,
    pub name: String,
    pub address: String,
    pub parent_id: Option<TargetId>,
    pub tags: BTreeMap<String, String>,
}

/// Empreinte stable d'une alerte : règle + série.
///
/// Le préfixe reste lisible dans les journaux, le suffixe condense la série pour
/// que la clé primaire garde une longueur bornée quelles que soient les étiquettes.
pub fn fingerprint(rule_uid: &str, series_key: &str) -> String {
    format!("{rule_uid}@{:016x}", fnv1a64(series_key))
}

/// FNV-1a 64 bits. Une empreinte n'a aucune exigence cryptographique ici : elle doit
/// seulement être stable entre deux démarrages, ce que `DefaultHasher` ne garantit pas.
fn fnv1a64(input: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in input.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Clé canonique d'une série, au format Prometheus, à partir de ses étiquettes.
///
/// L'étiquette `__name__` est extraite comme nom de métrique afin que la clé soit
/// identique à celle qu'aurait produite `Sample::series_key`.
pub fn series_key(labels: &BTreeMap<String, String>) -> String {
    let name = labels.get("__name__").map(String::as_str).unwrap_or("");
    let mut key = String::with_capacity(name.len() + 16 * labels.len());
    key.push_str(name);
    let mut first = true;
    for (label, value) in labels.iter().filter(|(k, _)| k.as_str() != "__name__") {
        key.push(if first { '{' } else { ',' });
        first = false;
        key.push_str(label);
        key.push_str("=\"");
        key.push_str(value);
        key.push('"');
    }
    if !first {
        key.push('}');
    }
    key
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: TargetId, tags: &[(&str, &str)]) -> TargetNode {
        TargetNode {
            id,
            name: format!("device-{id}"),
            address: format!("10.0.0.{id}"),
            parent_id: None,
            tags: tags.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect(),
        }
    }

    #[test]
    fn les_operateurs_comparent_comme_attendu() {
        assert!(Operator::Gt.test(91.0, 90.0));
        assert!(!Operator::Gt.test(90.0, 90.0));
        assert!(Operator::Ge.test(90.0, 90.0));
        assert!(Operator::Lt.test(0.0, 1.0));
        assert!(Operator::Le.test(1.0, 1.0));
    }

    #[test]
    fn une_valeur_non_finie_ne_declenche_jamais() {
        for operator in [Operator::Gt, Operator::Ge, Operator::Lt, Operator::Le] {
            assert!(!operator.test(f64::NAN, 0.0));
            assert!(!operator.test(f64::INFINITY, 0.0));
            assert!(!operator.test(f64::NEG_INFINITY, 0.0));
        }
    }

    #[test]
    fn l_escalade_plafonne_a_critical() {
        assert_eq!(Severity::Info.escalated(), Severity::Warning);
        assert_eq!(Severity::Warning.escalated(), Severity::Critical);
        assert_eq!(Severity::Critical.escalated(), Severity::Critical);
    }

    #[test]
    fn le_selecteur_par_etiquettes_exige_toutes_les_correspondances() {
        let selector = TargetSelector::Labels {
            labels: [("role".to_string(), "nas".to_string())].into_iter().collect(),
        };
        assert!(selector.matches(Some(&node(1, &[("role", "nas"), ("site", "home")]))));
        assert!(!selector.matches(Some(&node(2, &[("role", "switch")]))));
        assert!(!selector.matches(None));
    }

    #[test]
    fn le_selecteur_all_accepte_une_serie_sans_cible() {
        assert!(TargetSelector::All.matches(None));
        assert!(!TargetSelector::Ids { ids: vec![1] }.matches(None));
        assert!(TargetSelector::Ids { ids: vec![1] }.matches(Some(&node(1, &[]))));
    }

    #[test]
    fn les_empreintes_sont_stables_et_distinctes() {
        let a = fingerprint("cpu_high", "cpu{host=\"nas\"}");
        assert_eq!(a, fingerprint("cpu_high", "cpu{host=\"nas\"}"));
        assert_ne!(a, fingerprint("cpu_high", "cpu{host=\"router\"}"));
        assert_ne!(a, fingerprint("cpu_low", "cpu{host=\"nas\"}"));
        assert!(a.starts_with("cpu_high@"));
    }

    #[test]
    fn la_cle_de_serie_reprend_le_format_prometheus() {
        let labels: BTreeMap<String, String> =
            [("__name__", "ezymonit_cpu_usage_percent"), ("host", "nas"), ("core", "0")]
                .iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                .collect();
        assert_eq!(series_key(&labels), "ezymonit_cpu_usage_percent{core=\"0\",host=\"nas\"}");
    }

    #[test]
    fn une_serie_sans_etiquette_garde_son_nom_seul() {
        let labels: BTreeMap<String, String> =
            [("__name__".to_string(), "ezymonit_up".to_string())].into_iter().collect();
        assert_eq!(series_key(&labels), "ezymonit_up");
    }

    #[test]
    fn le_selecteur_se_serialise_en_json_stable() {
        let json = serde_json::to_string(&TargetSelector::Ids { ids: vec![3, 7] }).unwrap();
        assert_eq!(json, r#"{"kind":"ids","ids":[3,7]}"#);
        assert_eq!(
            serde_json::from_str::<TargetSelector>(r#"{"kind":"all"}"#).unwrap(),
            TargetSelector::All
        );
    }
}
