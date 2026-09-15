//! Surcharges d'une règle par équipement.
//!
//! Une règle livrée vaut pour tout le parc ; un NAS de sauvegarde qui tourne à
//! 95 % de disque en permanence n'a pas besoin d'une règle à part, seulement
//! d'un seuil à lui. Chaque champ d'une surcharge est facultatif : `None` laisse
//! la valeur de la règle s'appliquer.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::alerting::model::{Rule, TargetId};

/// Surcharge d'une règle pour un équipement donné.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RuleOverride {
    pub rule_uid: String,
    pub target_id: TargetId,
    pub threshold: Option<f64>,
    pub clear_threshold: Option<f64>,
    /// `Some(false)` retire l'équipement de la règle ; `Some(true)` est accepté
    /// pour la symétrie mais ne change rien.
    pub enabled: Option<bool>,
}

impl RuleOverride {
    /// Vrai si la surcharge ne change rien : autant ne pas la stocker.
    pub fn is_empty(&self) -> bool {
        self.threshold.is_none() && self.clear_threshold.is_none() && self.enabled.is_none()
    }
}

/// Seuils effectifs d'une règle pour un équipement, surcharge appliquée.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Effective {
    pub threshold: f64,
    pub clear_threshold: Option<f64>,
    pub enabled: bool,
}

/// Index (règle, équipement) → surcharge, construit une fois par cycle.
#[derive(Debug, Default)]
pub struct OverrideIndex {
    by_key: HashMap<(String, TargetId), RuleOverride>,
}

impl OverrideIndex {
    pub fn new(overrides: &[RuleOverride]) -> Self {
        Self {
            by_key: overrides
                .iter()
                .map(|o| ((o.rule_uid.clone(), o.target_id), o.clone()))
                .collect(),
        }
    }

    pub fn get(&self, rule_uid: &str, target: TargetId) -> Option<&RuleOverride> {
        self.by_key.get(&(rule_uid.to_string(), target))
    }

    /// Seuils à appliquer pour cette série. Sans équipement identifié, il n'y a
    /// rien à surcharger : la règle s'applique telle quelle.
    pub fn effective(&self, rule: &Rule, target: Option<TargetId>) -> Effective {
        let base = Effective {
            threshold: rule.threshold,
            clear_threshold: rule.clear_threshold,
            enabled: true,
        };
        let Some(over) = target.and_then(|id| self.get(&rule.uid, id)) else { return base };
        Effective {
            threshold: over.threshold.unwrap_or(base.threshold),
            // Un seuil surchargé sans seuil de retour explicite garde celui de la
            // règle seulement s'il reste cohérent ; sinon `Operator::holds` l'ignore.
            clear_threshold: over.clear_threshold.or(base.clear_threshold),
            enabled: over.enabled.unwrap_or(true),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::alerting::model::{AnomalyParams, Operator, RuleKind, Severity, TargetSelector};

    fn rule() -> Rule {
        Rule {
            id: 1,
            uid: "disk".to_string(),
            name: "Disk".to_string(),
            description: String::new(),
            kind: RuleKind::Threshold,
            query: "q".to_string(),
            operator: Operator::Ge,
            threshold: 90.0,
            clear_threshold: Some(88.0),
            for_duration: Duration::ZERO,
            severity: Severity::Warning,
            selector: TargetSelector::All,
            channels: Vec::new(),
            params: AnomalyParams::default(),
            unit: "%".to_string(),
            repeat_interval: None,
            escalate_after: None,
            enabled: true,
            builtin: true,
        }
    }

    #[test]
    fn une_surcharge_ne_touche_que_l_equipement_vise() {
        let index = OverrideIndex::new(&[RuleOverride {
            rule_uid: "disk".to_string(),
            target_id: 7,
            threshold: Some(97.0),
            clear_threshold: None,
            enabled: None,
        }]);
        let rule = rule();
        assert_eq!(index.effective(&rule, Some(7)).threshold, 97.0);
        assert_eq!(index.effective(&rule, Some(7)).clear_threshold, Some(88.0), "inherited");
        assert_eq!(index.effective(&rule, Some(8)).threshold, 90.0);
        assert_eq!(index.effective(&rule, None).threshold, 90.0);
    }

    #[test]
    fn une_surcharge_peut_desactiver_la_regle_pour_un_equipement() {
        let index = OverrideIndex::new(&[RuleOverride {
            rule_uid: "disk".to_string(),
            target_id: 7,
            threshold: None,
            clear_threshold: None,
            enabled: Some(false),
        }]);
        assert!(!index.effective(&rule(), Some(7)).enabled);
        assert!(index.effective(&rule(), Some(1)).enabled);
    }

    #[test]
    fn une_surcharge_vide_est_reconnue() {
        let empty = RuleOverride {
            rule_uid: "disk".to_string(),
            target_id: 1,
            threshold: None,
            clear_threshold: None,
            enabled: None,
        };
        assert!(empty.is_empty());
    }
}
