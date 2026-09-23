//! Regroupement, déduplication, rappels et escalade.
//!
//! Sans regroupement, un NAS dont le RAID se dégrade envoie un message par disque,
//! par cycle. L'utilisateur coupe alors les notifications, et le produit ne sert
//! plus à rien. On envoie donc au plus un message par hôte et par cycle, quitte à
//! ce qu'il contienne cinq lignes.

use std::collections::BTreeMap;
use std::time::Duration;

use chrono::{DateTime, TimeDelta, Utc};
use serde::{Deserialize, Serialize};

use crate::alerting::machine::{AlertState, EffectivePhase, Phase};
use crate::alerting::model::{Severity, TargetId};

/// Pourquoi une alerte prend la parole à ce cycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NotifyReason {
    /// Première notification depuis le déclenchement.
    Firing,
    /// Retour à la normale, uniquement si le déclenchement avait été annoncé.
    Resolved,
    /// Rappel périodique tant que l'alerte dure.
    Reminder,
    /// La sévérité est relevée d'un cran, l'alerte s'éternisant.
    Escalation,
    /// Personne n'a acquitté au bout du délai d'escalade : l'alerte part aussi
    /// sur le canal d'escalade de la politique, et seulement sur lui. Un seul
    /// relais, jamais deux : au-delà, ce n'est plus une notification, c'est une
    /// astreinte, et ce n'est pas ce que fait ce produit.
    Unacked,
    /// L'empreinte bat (déclenche et se résout en boucle) : un seul avis part,
    /// puis plus rien pendant la durée de retenue. Produit par la politique de
    /// notification, jamais par [`decide`].
    Flapping,
}

impl NotifyReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Firing => "firing",
            Self::Resolved => "resolved",
            Self::Reminder => "reminder",
            Self::Escalation => "escalation",
            Self::Unacked => "unacked",
            Self::Flapping => "flapping",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "firing" => Some(Self::Firing),
            "resolved" => Some(Self::Resolved),
            "reminder" => Some(Self::Reminder),
            "escalation" => Some(Self::Escalation),
            "unacked" => Some(Self::Unacked),
            "flapping" => Some(Self::Flapping),
            _ => None,
        }
    }

    /// Vrai pour tout ce qui annonce un problème en cours, par opposition à une
    /// résolution.
    pub fn is_firing_like(self) -> bool {
        !matches!(self, Self::Resolved)
    }
}

/// Résultat d'évaluation d'une empreinte, prêt à être notifié, persisté ou ignoré.
#[derive(Debug, Clone, PartialEq)]
pub struct AlertOutcome {
    pub fingerprint: String,
    pub rule_uid: String,
    pub rule_name: String,
    pub target_id: Option<TargetId>,
    pub target_name: String,
    pub series_key: String,
    pub labels: BTreeMap<String, String>,
    pub state: AlertState,
    /// Sévérité de base de la règle ; l'escalade la relève au moment de notifier.
    pub severity: Severity,
    pub value: Option<f64>,
    pub score: Option<f64>,
    pub unit: String,
    pub operator: String,
    pub threshold: f64,
    /// Canaux visés par la règle. Vide signifie « tous les canaux actifs ».
    pub channels: Vec<i64>,
    pub repeat_interval: Option<Duration>,
    pub escalate_after: Option<Duration>,
    /// Délai après lequel une alerte que personne n'a acquittée part aussi sur
    /// le canal d'escalade. Vient de la politique globale, pas de la règle :
    /// c'est un réglage d'instance (« si personne ne répond en un quart
    /// d'heure, réveille-moi »), pas une propriété de la condition mesurée.
    pub unacked_after: Option<Duration>,
    /// Vrai quand la transition vient d'être franchie à ce cycle.
    pub just_transitioned: bool,
}

impl AlertOutcome {
    pub fn effective_phase(&self) -> EffectivePhase {
        self.state.effective_phase()
    }

    /// Sévérité réellement notifiée, escalade comprise.
    pub fn effective_severity(&self, now: DateTime<Utc>) -> Severity {
        if self.escalated_at().is_some_and(|at| now >= at) {
            self.severity.escalated()
        } else {
            self.severity
        }
    }

    /// Instant auquel l'escalade prend effet, si la règle en prévoit une.
    fn escalated_at(&self) -> Option<DateTime<Utc>> {
        let after = TimeDelta::from_std(self.escalate_after?).ok()?;
        Some(self.state.firing_since? + after)
    }

    /// Instant auquel le relais vers le canal d'escalade devient dû.
    ///
    /// Compté depuis le déclenchement, comme l'escalade de sévérité : c'est la
    /// même horloge, et la seule que l'utilisateur voit dans l'interface
    /// (« déclenchée il y a 20 min »). Le délai réel avant que le canal
    /// d'escalade reçoive quoi que ce soit est repoussé par la politique tant
    /// que la première annonce n'est pas effectivement partie.
    fn unacked_at(&self) -> Option<DateTime<Utc>> {
        let after = TimeDelta::from_std(self.unacked_after?).ok()?;
        Some(self.state.firing_since? + after)
    }
}

/// Décide si l'alerte doit notifier à ce cycle, et pourquoi.
///
/// Les trois causes de mutisme — suppression par dépendance, silence de
/// maintenance, apprentissage — sont vérifiées avant toute autre considération :
/// elles priment sur les rappels comme sur l'escalade. L'acquittement vient
/// ensuite : il ne fait taire que ce qui annonce un problème en cours (première
/// annonce, rappel, escalade), jamais la résolution — qui, elle, ne passe pas
/// par ici puisque la machine efface l'acquittement en résolvant.
pub fn decide(outcome: &AlertOutcome, now: DateTime<Utc>) -> Option<NotifyReason> {
    let state = &outcome.state;

    if state.suppressed || state.silenced || state.learning {
        return None;
    }

    match state.phase {
        Phase::Firing => {
            if state.is_acked(now) {
                return None;
            }
            if state.notify_count == 0 {
                return Some(NotifyReason::Firing);
            }
            let last = state.last_notified_at?;

            // L'escalade se déduit des horodatages plutôt que d'un drapeau
            // persistant : c'est une donnée de moins à maintenir cohérente, et le
            // franchissement reste détecté une seule fois.
            if let Some(escalated_at) = outcome.escalated_at()
                && last < escalated_at
                && now >= escalated_at
            {
                return Some(NotifyReason::Escalation);
            }

            // Relais vers le canal d'escalade : même lecture des horodatages,
            // donc même garantie qu'il ne part qu'une fois.
            if let Some(unacked_at) = outcome.unacked_at()
                && last < unacked_at
                && now >= unacked_at
            {
                return Some(NotifyReason::Unacked);
            }

            let repeat = TimeDelta::from_std(outcome.repeat_interval?).ok()?;
            (now.signed_duration_since(last) >= repeat).then_some(NotifyReason::Reminder)
        }
        // On n'annonce une résolution que si le déclenchement a été annoncé : sinon
        // l'utilisateur reçoit « tout va bien » pour un problème qu'il ignorait.
        //
        // La comparaison des horodatages, plutôt qu'un simple « la transition vient
        // d'avoir lieu », rend l'annonce rejouable : si tous les canaux étaient en
        // panne au moment de la résolution, le cycle suivant la renvoie.
        Phase::Resolved if state.notify_count > 0 => {
            let resolved_at = state.resolved_at?;
            let last = state.last_notified_at?;
            (last < resolved_at).then_some(NotifyReason::Resolved)
        }
        _ => None,
    }
}

/// Une ligne d'un message groupé.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GroupItem {
    pub fingerprint: String,
    /// `uid` de la règle, que le filtre par étiquettes d'un canal peut viser.
    /// Par défaut vide : les lignes déjà en file avant cette version se relisent.
    #[serde(default)]
    pub rule_uid: String,
    pub rule_name: String,
    pub severity: Severity,
    pub reason: NotifyReason,
    pub value: Option<f64>,
    pub score: Option<f64>,
    pub unit: String,
    pub operator: String,
    pub threshold: f64,
    pub series_key: String,
    pub since: Option<DateTime<Utc>>,
    pub phase: EffectivePhase,
    /// Précision ajoutée par la politique de notification (« flapping: 4 changes
    /// in 30 min »), affichée en fin de ligne.
    #[serde(default)]
    pub note: Option<String>,
}

/// Un message : un hôte, ses alertes du cycle.
#[derive(Debug, Clone, PartialEq)]
pub struct AlertGroup {
    pub target_id: Option<TargetId>,
    pub target_name: String,
    /// Collecteur de l'équipement et étiquettes de sa fiche, renseignés par le
    /// cycle après le regroupement : c'est ce que lit le filtre d'un canal.
    /// Vides pour le groupe des séries sans équipement identifié.
    pub target_kind: String,
    pub target_tags: BTreeMap<String, String>,
    /// Sévérité la plus élevée du groupe : c'est elle qui donne le ton du message.
    pub severity: Severity,
    pub items: Vec<GroupItem>,
    /// Union des canaux demandés par les règles du groupe. Vide = tous les canaux.
    pub channels: Vec<i64>,
    pub at: DateTime<Utc>,
}

impl AlertGroup {
    /// Vrai si toutes les lignes du groupe sont des résolutions.
    pub fn is_resolution(&self) -> bool {
        self.items.iter().all(|item| item.reason == NotifyReason::Resolved)
    }
}

/// Construit les messages du cycle.
///
/// Le regroupement se fait par cible : c'est l'unité que l'utilisateur a en tête
/// (« mon NAS va mal »), pas la série ni la règle. Les séries sans cible identifiée
/// tombent dans un groupe commun plutôt que d'être perdues.
pub fn group(outcomes: &[AlertOutcome], now: DateTime<Utc>) -> Vec<AlertGroup> {
    // `BTreeMap` plutôt que `HashMap` : l'ordre des messages doit être reproductible,
    // en production comme dans les tests.
    let mut groups: BTreeMap<Option<TargetId>, AlertGroup> = BTreeMap::new();
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    // Cibles dont au moins une règle vise « tous les canaux » : leur message part
    // partout, sinon rattacher une seule règle à un canal précis restreindrait
    // silencieusement toutes les autres alertes du même hôte.
    let mut broadcast: std::collections::HashSet<Option<TargetId>> =
        std::collections::HashSet::new();

    for outcome in outcomes {
        // Déduplication : une empreinte présente deux fois — deux séries résolues
        // vers la même clé, un rechargement concurrent — ne produit qu'une ligne.
        if !seen.insert(outcome.fingerprint.as_str()) {
            continue;
        }
        let Some(reason) = decide(outcome, now) else { continue };

        let severity = outcome.effective_severity(now);
        let item = GroupItem {
            fingerprint: outcome.fingerprint.clone(),
            rule_uid: outcome.rule_uid.clone(),
            rule_name: outcome.rule_name.clone(),
            severity,
            reason,
            value: outcome.value,
            score: outcome.score,
            unit: outcome.unit.clone(),
            operator: outcome.operator.clone(),
            threshold: outcome.threshold,
            series_key: outcome.series_key.clone(),
            since: outcome.state.firing_since,
            phase: outcome.effective_phase(),
            note: None,
        };

        let group = groups.entry(outcome.target_id).or_insert_with(|| AlertGroup {
            target_id: outcome.target_id,
            target_name: outcome.target_name.clone(),
            target_kind: String::new(),
            target_tags: BTreeMap::new(),
            severity: Severity::Info,
            items: Vec::new(),
            channels: Vec::new(),
            at: now,
        });
        group.severity = group.severity.max(severity);
        if outcome.channels.is_empty() {
            broadcast.insert(outcome.target_id);
        }
        for channel in &outcome.channels {
            if !group.channels.contains(channel) {
                group.channels.push(*channel);
            }
        }
        group.items.push(item);
    }

    // Les lignes les plus graves en tête : un message tronqué par le service de
    // notification doit rester informatif.
    let mut result: Vec<AlertGroup> = groups.into_values().collect();
    for group in &mut result {
        if broadcast.contains(&group.target_id) {
            group.channels.clear();
        }
        group.items.sort_by(|a, b| {
            b.severity.cmp(&a.severity).then_with(|| a.rule_name.cmp(&b.rule_name))
        });
    }
    result.sort_by(|a, b| {
        b.severity.cmp(&a.severity).then_with(|| a.target_name.cmp(&b.target_name))
    });
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(seconds: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_700_000_000 + seconds, 0).expect("valid timestamp")
    }

    fn outcome(fingerprint: &str, target: TargetId, name: &str) -> AlertOutcome {
        AlertOutcome {
            fingerprint: fingerprint.to_string(),
            rule_uid: "cpu_high".to_string(),
            rule_name: "High CPU".to_string(),
            target_id: Some(target),
            target_name: name.to_string(),
            series_key: format!("s-{fingerprint}"),
            labels: BTreeMap::new(),
            state: AlertState {
                phase: Phase::Firing,
                firing_since: Some(at(0)),
                ..Default::default()
            },
            severity: Severity::Warning,
            value: Some(95.0),
            score: None,
            unit: "%".to_string(),
            operator: ">".to_string(),
            threshold: 90.0,
            channels: vec![1],
            repeat_interval: Some(Duration::from_secs(3600)),
            escalate_after: None,
            unacked_after: None,
            just_transitioned: true,
        }
    }

    #[test]
    fn une_alerte_neuve_notifie() {
        assert_eq!(decide(&outcome("a", 1, "nas"), at(0)), Some(NotifyReason::Firing));
    }

    #[test]
    fn une_alerte_deja_notifiee_se_tait_jusqu_au_rappel() {
        let mut o = outcome("a", 1, "nas");
        o.state.notify_count = 1;
        o.state.last_notified_at = Some(at(0));

        assert_eq!(decide(&o, at(3599)), None, "before the time, stay silent");
        assert_eq!(decide(&o, at(3600)), Some(NotifyReason::Reminder));
    }

    #[test]
    fn sans_rappel_configure_une_alerte_ne_parle_qu_une_fois() {
        let mut o = outcome("a", 1, "nas");
        o.state.notify_count = 1;
        o.state.last_notified_at = Some(at(0));
        o.repeat_interval = None;
        assert_eq!(decide(&o, at(100_000)), None);
    }

    #[test]
    fn l_escalade_releve_la_severite_une_seule_fois() {
        let mut o = outcome("a", 1, "nas");
        o.state.notify_count = 1;
        o.state.last_notified_at = Some(at(0));
        o.escalate_after = Some(Duration::from_secs(1800));
        o.repeat_interval = None;

        assert_eq!(decide(&o, at(1799)), None);
        assert_eq!(decide(&o, at(1800)), Some(NotifyReason::Escalation));
        assert_eq!(o.effective_severity(at(1799)), Severity::Warning);
        assert_eq!(o.effective_severity(at(1800)), Severity::Critical);

        // Une fois l'escalade annoncée, elle ne se répète pas.
        o.state.last_notified_at = Some(at(1800));
        assert_eq!(decide(&o, at(3600)), None);
    }

    #[test]
    fn l_escalade_prime_sur_le_rappel() {
        let mut o = outcome("a", 1, "nas");
        o.state.notify_count = 1;
        o.state.last_notified_at = Some(at(0));
        o.escalate_after = Some(Duration::from_secs(600));
        o.repeat_interval = Some(Duration::from_secs(60));
        assert_eq!(decide(&o, at(600)), Some(NotifyReason::Escalation));
    }

    #[test]
    fn la_suppression_le_silence_et_l_apprentissage_font_taire() {
        for mutateur in [
            |s: &mut AlertState| s.suppressed = true,
            |s: &mut AlertState| s.silenced = true,
            |s: &mut AlertState| s.learning = true,
        ] {
            let mut o = outcome("a", 1, "nas");
            mutateur(&mut o.state);
            assert_eq!(decide(&o, at(0)), None);
        }
    }

    #[test]
    fn une_alerte_acquittee_ne_rappelle_plus_jusqu_a_l_echeance() {
        let mut o = outcome("a", 1, "nas");
        o.state.notify_count = 1;
        o.state.last_notified_at = Some(at(0));
        o.state.acked_until = Some(at(14_400));
        o.state.acked_by = Some("admin".to_string());
        o.escalate_after = Some(Duration::from_secs(1800));

        assert_eq!(decide(&o, at(3600)), None, "reminder due, but acked");
        assert_eq!(decide(&o, at(1800)), None, "escalation due, but acked");
        // L'acquittement échu : le rappel repart au cycle suivant.
        assert_eq!(decide(&o, at(14_400)), Some(NotifyReason::Escalation));
        o.state.last_notified_at = Some(at(14_400));
        assert_eq!(decide(&o, at(18_000)), Some(NotifyReason::Reminder));
    }

    #[test]
    fn une_alerte_acquittee_avant_toute_annonce_se_tait_aussi() {
        let mut o = outcome("a", 1, "nas");
        o.state.acked_until = Some(at(3600));
        assert_eq!(decide(&o, at(0)), None, "acked from the UI before the batch left");
        assert_eq!(decide(&o, at(3600)), Some(NotifyReason::Firing));
    }

    #[test]
    fn la_resolution_d_une_alerte_acquittee_est_annoncee() {
        let mut o = outcome("a", 1, "nas");
        o.state.notify_count = 1;
        o.state.last_notified_at = Some(at(0));
        o.state.acked_until = Some(at(14_400));
        // La machine à états a résolu — et effacé l'acquittement au passage ;
        // même si une ligne gardait la date, la résolution passe.
        o.state.phase = Phase::Resolved;
        o.state.resolved_at = Some(at(60));
        assert_eq!(decide(&o, at(60)), Some(NotifyReason::Resolved));
    }

    #[test]
    fn une_resolution_n_est_annoncee_que_si_le_declenchement_l_a_ete() {
        let mut o = outcome("a", 1, "nas");
        o.state.phase = Phase::Resolved;
        o.state.resolved_at = Some(at(60));
        o.state.last_notified_at = Some(at(0));
        o.state.notify_count = 0;
        assert_eq!(decide(&o, at(60)), None, "nobody was told about the problem");

        o.state.notify_count = 1;
        assert_eq!(decide(&o, at(60)), Some(NotifyReason::Resolved));

        // Une fois la résolution annoncée, elle ne se répète pas.
        o.state.last_notified_at = Some(at(60));
        assert_eq!(decide(&o, at(120)), None);
    }

    #[test]
    fn les_alertes_d_un_meme_hote_tiennent_dans_un_seul_message() {
        let mut disque = outcome("b", 1, "nas");
        disque.rule_name = "Disk almost full".to_string();
        disque.severity = Severity::Critical;

        let groups = group(&[outcome("a", 1, "nas"), disque], at(0));
        assert_eq!(groups.len(), 1, "one host, one message");
        assert_eq!(groups[0].items.len(), 2);
        assert_eq!(groups[0].severity, Severity::Critical, "the worst severity sets the tone");
        assert_eq!(groups[0].items[0].rule_name, "Disk almost full", "most severe first");
    }

    #[test]
    fn deux_hotes_donnent_deux_messages() {
        let groups = group(&[outcome("a", 1, "nas"), outcome("b", 2, "router")], at(0));
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].target_name, "nas");
        assert_eq!(groups[1].target_name, "router");
    }

    #[test]
    fn une_empreinte_repetee_ne_produit_qu_une_ligne() {
        let groups = group(&[outcome("a", 1, "nas"), outcome("a", 1, "nas")], at(0));
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].items.len(), 1, "deduplication by fingerprint");
    }

    #[test]
    fn les_alertes_muettes_ne_creent_pas_de_groupe_vide() {
        let mut muette = outcome("a", 1, "nas");
        muette.state.suppressed = true;
        assert!(group(&[muette], at(0)).is_empty());
    }

    #[test]
    fn les_canaux_des_regles_du_groupe_sont_unis_sans_doublon() {
        let mut autre = outcome("b", 1, "nas");
        autre.channels = vec![1, 2];
        let groups = group(&[outcome("a", 1, "nas"), autre], at(0));
        assert_eq!(groups[0].channels, vec![1, 2]);
    }

    #[test]
    fn les_hotes_les_plus_graves_sont_notifies_en_premier() {
        let mut grave = outcome("b", 2, "router");
        grave.severity = Severity::Critical;
        let groups = group(&[outcome("a", 1, "nas"), grave], at(0));
        assert_eq!(groups[0].target_name, "router");
    }

    #[test]
    fn un_groupe_entierement_resolu_est_reconnu_comme_tel() {
        let mut resolue = outcome("a", 1, "nas");
        resolue.state.phase = Phase::Resolved;
        resolue.state.notify_count = 1;
        resolue.state.resolved_at = Some(at(60));
        resolue.state.last_notified_at = Some(at(0));
        let groups = group(&[resolue], at(60));
        assert!(groups[0].is_resolution());
    }

    #[test]
    fn une_resolution_non_delivree_est_rejouee_au_cycle_suivant() {
        let mut o = outcome("a", 1, "nas");
        o.state.phase = Phase::Resolved;
        o.state.notify_count = 1;
        o.state.resolved_at = Some(at(60));
        o.state.last_notified_at = Some(at(0));
        // Les canaux étaient en panne : rien n'a été consigné, on réessaie.
        assert_eq!(decide(&o, at(90)), Some(NotifyReason::Resolved));
        assert_eq!(decide(&o, at(600)), Some(NotifyReason::Resolved));
    }

    // ----------------------------------------------------------------------
    // Relais vers le canal d'escalade
    // ----------------------------------------------------------------------

    #[test]
    fn un_relais_part_une_seule_fois_apres_le_delai() {
        let mut o = outcome("a", 1, "nas");
        o.state.notify_count = 1;
        o.state.last_notified_at = Some(at(0));
        o.repeat_interval = None;
        o.unacked_after = Some(Duration::from_secs(900));

        assert_eq!(decide(&o, at(899)), None, "before the time, nothing");
        assert_eq!(decide(&o, at(900)), Some(NotifyReason::Unacked));
        // Une fois parti, il ne se répète pas : un seul relais, jamais deux.
        o.state.last_notified_at = Some(at(900));
        assert_eq!(decide(&o, at(5_400)), None);
    }

    #[test]
    fn un_relais_ne_part_pas_si_l_alerte_est_acquittee() {
        let mut o = outcome("a", 1, "nas");
        o.state.notify_count = 1;
        o.state.last_notified_at = Some(at(0));
        o.repeat_interval = None;
        o.unacked_after = Some(Duration::from_secs(900));
        o.state.acked_until = Some(at(14_400));
        o.state.acked_by = Some("admin".to_string());

        assert_eq!(decide(&o, at(900)), None, "someone is on it");
        assert_eq!(decide(&o, at(10_000)), None, "still acked");
    }

    #[test]
    fn un_relais_ne_part_pas_si_l_alerte_est_resolue() {
        let mut o = outcome("a", 1, "nas");
        o.state.notify_count = 1;
        o.state.last_notified_at = Some(at(0));
        o.repeat_interval = None;
        o.unacked_after = Some(Duration::from_secs(900));
        o.state.phase = Phase::Resolved;
        o.state.resolved_at = Some(at(60));

        assert_eq!(
            decide(&o, at(900)),
            Some(NotifyReason::Resolved),
            "the resolution, not a relay"
        );
        o.state.last_notified_at = Some(at(900));
        assert_eq!(decide(&o, at(1_800)), None);
    }

    #[test]
    fn un_relais_se_tait_aussi_pendant_une_maintenance() {
        let mut o = outcome("a", 1, "nas");
        o.state.notify_count = 1;
        o.state.last_notified_at = Some(at(0));
        o.repeat_interval = None;
        o.unacked_after = Some(Duration::from_secs(900));
        o.state.silenced = true;
        assert_eq!(decide(&o, at(900)), None);
    }

    #[test]
    fn l_escalade_de_severite_prime_sur_le_relais() {
        // Les deux sont dus au même cycle : la sévérité monte d'abord, et le
        // relais suivra au cycle d'après si personne n'a acquitté entre-temps.
        let mut o = outcome("a", 1, "nas");
        o.state.notify_count = 1;
        o.state.last_notified_at = Some(at(0));
        o.repeat_interval = None;
        o.escalate_after = Some(Duration::from_secs(900));
        o.unacked_after = Some(Duration::from_secs(900));
        assert_eq!(decide(&o, at(900)), Some(NotifyReason::Escalation));
    }

    #[test]
    fn un_relais_porte_l_identifiant_de_sa_regle() {
        // Le filtre par étiquettes d'un canal vise des règles par `uid` : la
        // ligne doit le porter jusqu'à la politique de notification.
        let groups = group(&[outcome("a", 1, "nas")], at(0));
        assert_eq!(groups[0].items[0].rule_uid, "cpu_high");
    }

    #[test]
    fn une_regle_visant_tous_les_canaux_ne_restreint_pas_le_groupe() {
        let mut cible = outcome("a", 1, "nas");
        cible.channels = vec![2];
        let mut partout = outcome("b", 1, "nas");
        partout.channels = Vec::new();

        let groups = group(&[cible, partout], at(0));
        assert!(groups[0].channels.is_empty(), "the message must go to every active channel");
    }
}
