//! Politique de notification : ce qui décide, une fois qu'une alerte a le droit de
//! parler, *quand* et *sur quel canal* elle le fait — sans jamais spammer.
//!
//! Le cycle ([`crate::alerting::cycle`]) a déjà dédupliqué, appliqué la
//! suppression par dépendance, les fenêtres de maintenance, les rappels et
//! l'escalade, et regroupé par équipement. Ce module reçoit ces groupes et les
//! passe, pour chaque canal, dans l'entonnoir suivant :
//!
//! 1. retenue de battement — une empreinte qui déclenche et se résout en boucle
//!    n'envoie qu'un avis « flapping », puis se tait un moment ;
//! 2. filtre de sévérité du canal, et « résolutions oui/non » du canal ;
//! 3. délai minimal du canal entre deux messages pour la même empreinte ;
//! 4. heures calmes du canal — seule la sévérité la plus haute passe, le reste
//!    attend la fin des heures calmes ;
//! 5. fenêtre de regroupement — tout ce qui arrive dans la fenêtre part en un
//!    seul message par canal ;
//! 6. plafond horaire par canal — au-delà, on attend et on résume.
//!
//! Le module est pur : il reçoit un [`Ledger`] (file d'attente, registre des
//! envois, retenues) et rend un [`Plan`] que le moteur applique. Tout se teste
//! avec des horloges fixes.

use std::collections::{BTreeMap, HashMap, HashSet};

use chrono::{DateTime, TimeDelta, Utc};
use serde::{Deserialize, Serialize};

use crate::alerting::group::{AlertGroup, GroupItem, NotifyReason};
use crate::alerting::model::{Severity, TargetId};
use crate::alerting::silence::Schedule;

/// Fenêtre de regroupement par défaut. Une minute : le temps qu'un incident
/// montre toutes ses facettes (le disque, puis le service, puis la latence) et
/// qu'elles partent ensemble.
pub const DEFAULT_BATCH_WINDOW_SECS: u32 = 60;
/// Plafond de messages par canal et par heure. Vingt suffit à une panne sérieuse
/// sans qu'un téléphone vibre toute la nuit.
pub const DEFAULT_MAX_PER_HOUR: u32 = 20;
/// Battement : autant de changements d'état en autant de secondes, puis retenue.
pub const DEFAULT_FLAP_EVENTS: u32 = 4;
pub const DEFAULT_FLAP_WINDOW_SECS: u32 = 30 * 60;
pub const DEFAULT_FLAP_HOLD_SECS: u32 = 30 * 60;
/// Lignes détaillées au plus dans un résumé ; au-delà, « …and N more alerts ».
pub const DIGEST_MAX_LINES: usize = 12;
/// Profondeur du registre utile à un cycle : le plafond horaire regarde une heure
/// en arrière, le battement sa fenêtre, le délai minimal la sienne.
pub const LEDGER_HOUR_SECS: i64 = 3600;

/// Types de canaux d'astreinte : ils ouvrent et ferment des incidents par
/// équipement, un résumé multi-équipements casserait leur rapprochement. Ils
/// reçoivent donc un message par équipement, sans attendre la fenêtre.
pub const ONCALL_KINDS: &[&str] = &["pagerduty", "opsgenie"];

/// Politique globale, stockée dans `settings` sous `notify_policy`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct GlobalPolicy {
    /// Fenêtre de regroupement, en secondes. 0 : envoi dès le cycle.
    pub batch_window_secs: u32,
    /// Messages par canal et par heure. 0 : sans limite.
    pub max_per_hour: u32,
    /// Nombre de changements d'état (déclenchement, résolution) dans la fenêtre à
    /// partir duquel une empreinte est jugée en battement. 0 : détection coupée.
    pub flap_events: u32,
    pub flap_window_secs: u32,
    /// Durée pendant laquelle une empreinte en battement se tait.
    pub flap_hold_secs: u32,
    /// URL publique de l'instance, pour les liens vers l'équipement dans les
    /// messages. Vide : `EZYMONIT_PUBLIC_URL`, sinon pas de lien.
    pub public_url: String,
}

impl Default for GlobalPolicy {
    fn default() -> Self {
        Self {
            batch_window_secs: DEFAULT_BATCH_WINDOW_SECS,
            max_per_hour: DEFAULT_MAX_PER_HOUR,
            flap_events: DEFAULT_FLAP_EVENTS,
            flap_window_secs: DEFAULT_FLAP_WINDOW_SECS,
            flap_hold_secs: DEFAULT_FLAP_HOLD_SECS,
            public_url: String::new(),
        }
    }
}

impl GlobalPolicy {
    /// URL publique effective, sans barre finale. `None` quand rien n'est réglé.
    pub fn public_url(&self, env_fallback: Option<&str>) -> Option<String> {
        let raw = if self.public_url.trim().is_empty() { env_fallback? } else { &self.public_url };
        let trimmed = raw.trim().trim_end_matches('/');
        (!trimmed.is_empty()).then(|| trimmed.to_string())
    }
}

/// Politique d'un canal, stockée dans `notification_channels.policy`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ChannelPolicy {
    /// Sévérité minimale acceptée par le canal.
    pub min_severity: Severity,
    /// Faux : le canal n'annonce que les problèmes, jamais les retours à la normale.
    pub notify_resolved: bool,
    /// Délai minimal entre deux messages pour la même empreinte, en secondes.
    pub min_interval_secs: u32,
    /// Heures calmes : pendant la fenêtre, seule la sévérité `critical` passe ;
    /// le reste attend et part en résumé à la fin.
    pub quiet_hours: Option<Schedule>,
}

impl Default for ChannelPolicy {
    fn default() -> Self {
        Self {
            min_severity: Severity::Info,
            notify_resolved: true,
            min_interval_secs: 0,
            quiet_hours: None,
        }
    }
}

impl ChannelPolicy {
    pub fn in_quiet_hours(&self, now: DateTime<Utc>) -> bool {
        self.quiet_hours.as_ref().is_some_and(|schedule| schedule.covers(now))
    }
}

/// Canal tel que la politique le voit : identité, type et réglages, sans secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recipient {
    pub id: i64,
    pub kind: String,
    pub enabled: bool,
    pub policy: ChannelPolicy,
}

impl Recipient {
    fn is_oncall(&self) -> bool {
        ONCALL_KINDS.contains(&self.kind.as_str())
    }
}

/// Pourquoi une ligne attend dans la file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Hold {
    /// Fenêtre de regroupement.
    Batch,
    /// Heures calmes du canal.
    Quiet,
}

impl Hold {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Batch => "batch",
            Self::Quiet => "quiet",
        }
    }

    pub fn parse(raw: &str) -> Self {
        if raw == "quiet" { Self::Quiet } else { Self::Batch }
    }
}

/// Ligne en attente d'envoi sur un canal.
#[derive(Debug, Clone, PartialEq)]
pub struct QueuedItem {
    /// `None` tant que la ligne n'a pas été écrite en base.
    pub id: Option<i64>,
    pub channel_id: i64,
    pub target_id: Option<TargetId>,
    pub target_name: String,
    pub hold: Hold,
    pub item: GroupItem,
    /// L'alerte s'est résolue pendant l'attente : elle n'est plus qu'une mention.
    pub resolved_meanwhile: bool,
    pub queued_at: DateTime<Utc>,
}

/// Entrée du registre. `channel_id == 0` : entrée dans le pipeline (compte les
/// battements) ; sinon, envoi effectif sur ce canal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogEntry {
    pub channel_id: i64,
    pub fingerprint: String,
    pub reason: NotifyReason,
    pub at: DateTime<Utc>,
}

/// Registre du pipeline (empreinte de « pipeline » : 0).
pub const INTAKE_CHANNEL: i64 = 0;

/// Ce que le moteur charge en base avant d'appeler [`plan`].
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Ledger {
    pub queue: Vec<QueuedItem>,
    /// Entrées récentes, au moins celles de la dernière heure et de la fenêtre de
    /// battement.
    pub log: Vec<LogEntry>,
    /// Empreintes retenues pour battement, avec la fin de la retenue.
    pub flap_holds: HashMap<String, DateTime<Utc>>,
    /// Messages partis dans la dernière heure : (canal, instant).
    pub messages: Vec<(i64, DateTime<Utc>)>,
}

/// Contenu d'un message à rendre : un ou plusieurs équipements, plus ce qui s'est
/// résolu pendant l'attente.
#[derive(Debug, Clone, PartialEq)]
pub struct Digest {
    pub groups: Vec<AlertGroup>,
    pub resolved_meanwhile: Vec<(String, GroupItem)>,
    /// `Some(Quiet)` : le message clôt des heures calmes.
    pub hold: Option<Hold>,
    pub at: DateTime<Utc>,
}

impl Digest {
    pub fn item_count(&self) -> usize {
        self.groups.iter().map(|group| group.items.len()).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.groups.is_empty() && self.resolved_meanwhile.is_empty()
    }
}

/// Message prêt à partir sur un canal.
#[derive(Debug, Clone, PartialEq)]
pub struct Outgoing {
    pub channel_id: i64,
    pub digest: Digest,
    /// Lignes de file à effacer après un envoi réussi.
    pub queue_ids: Vec<i64>,
    /// Lignes à remettre en file si l'envoi échoue (celles qui n'avaient pas
    /// encore de ligne en base comprises).
    pub retry: Vec<QueuedItem>,
    /// Empreintes et raisons à consigner comme envoyées sur ce canal.
    pub sent: Vec<(String, NotifyReason)>,
}

/// Décisions d'un cycle, à appliquer par le moteur.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Plan {
    pub outgoing: Vec<Outgoing>,
    /// Lignes de file à créer ou remplacer (clé : canal + empreinte).
    pub enqueue: Vec<QueuedItem>,
    /// Lignes de file à effacer sans envoi (battement, remplacement).
    pub dequeue: Vec<i64>,
    /// Nouvelles retenues de battement.
    pub flap_holds: Vec<(String, DateTime<Utc>)>,
    /// Entrées de pipeline à consigner (comptage des battements).
    pub intake: Vec<LogEntry>,
    /// Empreintes traitées par la politique : à marquer notifiées pour que le
    /// cycle ne les représente pas, qu'elles soient parties, en attente ou
    /// écartées.
    pub handled: HashSet<String>,
    /// Empreintes routées vers au moins un canal (envoi ou attente).
    pub announced: HashSet<String>,
    /// Dernière raison d'écart par empreinte, pour l'historique.
    pub verdicts: HashMap<String, String>,
}

impl Plan {
    fn note(&mut self, fingerprint: &str, verdict: impl Into<String>) {
        self.verdicts.insert(fingerprint.to_string(), verdict.into());
    }
}

fn secs(value: u32) -> TimeDelta {
    TimeDelta::seconds(i64::from(value))
}

/// Applique la politique aux groupes du cycle.
pub fn plan(
    groups: &[AlertGroup],
    recipients: &[Recipient],
    policy: &GlobalPolicy,
    ledger: &Ledger,
    now: DateTime<Utc>,
) -> Plan {
    let mut plan = Plan::default();
    let mut queue: Vec<(QueuedItem, bool)> =
        ledger.queue.iter().map(|item| (item.clone(), false)).collect();
    let mut flap_holds: HashMap<String, DateTime<Utc>> = ledger
        .flap_holds
        .iter()
        .filter(|(_, until)| **until > now)
        .map(|(fp, until)| (fp.clone(), *until))
        .collect();
    let mut intake: Vec<LogEntry> =
        ledger.log.iter().filter(|entry| entry.channel_id == INTAKE_CHANNEL).cloned().collect();

    let active: Vec<&Recipient> = recipients.iter().filter(|r| r.enabled).collect();

    for group in groups {
        let targets: Vec<&Recipient> = active
            .iter()
            .copied()
            .filter(|r| group.channels.is_empty() || group.channels.contains(&r.id))
            .collect();

        for item in &group.items {
            let fp = item.fingerprint.as_str();
            plan.handled.insert(fp.to_string());

            // Comptage des battements : seuls les vrais changements d'état comptent,
            // un rappel n'est pas un aller-retour.
            if matches!(item.reason, NotifyReason::Firing | NotifyReason::Resolved) {
                let entry = LogEntry {
                    channel_id: INTAKE_CHANNEL,
                    fingerprint: fp.to_string(),
                    reason: item.reason,
                    at: now,
                };
                intake.push(entry.clone());
                plan.intake.push(entry);
            }

            if let Some(until) = flap_holds.get(fp).copied() {
                plan.note(fp, format!("flapping: held until {}", until.format("%H:%M UTC")));
                purge_fingerprint(&mut queue, fp, &mut plan.dequeue);
                continue;
            }

            let mut item = item.clone();
            if policy.flap_events > 0
                && matches!(item.reason, NotifyReason::Firing | NotifyReason::Resolved)
            {
                let since = now - secs(policy.flap_window_secs);
                let changes = intake
                    .iter()
                    .filter(|e| e.fingerprint == fp && e.at > since)
                    .filter(|e| matches!(e.reason, NotifyReason::Firing | NotifyReason::Resolved))
                    .count();
                if changes >= policy.flap_events as usize {
                    let until = now + secs(policy.flap_hold_secs);
                    flap_holds.insert(fp.to_string(), until);
                    plan.flap_holds.push((fp.to_string(), until));
                    purge_fingerprint(&mut queue, fp, &mut plan.dequeue);
                    item.reason = NotifyReason::Flapping;
                    item.note = Some(format!(
                        "flapping: {changes} changes in {} min, notifications held for {} min",
                        policy.flap_window_secs / 60,
                        policy.flap_hold_secs / 60
                    ));
                }
            }

            if targets.is_empty() {
                // Aucun canal : l'alerte est traitée comme aujourd'hui, sans envoi,
                // pour ne pas la rejouer le jour où un canal apparaît.
                continue;
            }

            for recipient in &targets {
                route(recipient, group, &item, ledger, &mut queue, &mut plan, now);
            }
        }
    }

    flush(&active, policy, ledger, &mut queue, &mut plan, now);

    plan.enqueue = queue.into_iter().filter(|(_, dirty)| *dirty).map(|(item, _)| item).collect();
    plan
}

/// Retire toutes les lignes en attente d'une empreinte, sur tous les canaux.
fn purge_fingerprint(queue: &mut Vec<(QueuedItem, bool)>, fp: &str, dequeue: &mut Vec<i64>) {
    queue.retain(|(queued, _)| {
        if queued.item.fingerprint == fp {
            if let Some(id) = queued.id {
                dequeue.push(id);
            }
            false
        } else {
            true
        }
    });
}

/// Décide du sort d'une ligne pour un canal : écart, ou mise en file.
fn route(
    recipient: &Recipient,
    group: &AlertGroup,
    item: &GroupItem,
    ledger: &Ledger,
    queue: &mut Vec<(QueuedItem, bool)>,
    plan: &mut Plan,
    now: DateTime<Utc>,
) {
    let fp = item.fingerprint.as_str();
    let policy = &recipient.policy;

    if item.severity < policy.min_severity {
        plan.note(fp, format!("below the minimum severity of channel {}", recipient.id));
        return;
    }
    if item.reason == NotifyReason::Resolved && !policy.notify_resolved {
        plan.note(fp, format!("channel {} does not announce resolutions", recipient.id));
        return;
    }

    let last_sent = ledger
        .log
        .iter()
        .filter(|e| e.channel_id == recipient.id && e.fingerprint == fp)
        .max_by_key(|e| e.at);
    let existing =
        queue.iter().position(|(q, _)| q.channel_id == recipient.id && q.item.fingerprint == fp);

    if item.reason == NotifyReason::Resolved {
        if let Some(pos) = existing {
            let (queued, dirty) = &mut queue[pos];
            // Déclenchée pendant l'attente, résolue avant l'envoi : le message ne
            // la cite plus que pour mémoire.
            if queued.item.reason.is_firing_like() {
                queued.resolved_meanwhile = true;
            }
            queued.item = item.clone();
            *dirty = true;
            plan.announced.insert(fp.to_string());
            return;
        }
        // Une résolution ne s'annonce que si ce canal a annoncé le déclenchement :
        // sinon l'utilisateur lit « tout va bien » pour un problème qu'il ignorait.
        if !last_sent.is_some_and(|e| e.reason.is_firing_like()) {
            plan.note(fp, format!("channel {} never announced the firing", recipient.id));
            return;
        }
    } else if policy.min_interval_secs > 0
        && item.reason != NotifyReason::Flapping
        && let Some(last) = last_sent
        && now.signed_duration_since(last.at) < secs(policy.min_interval_secs)
    {
        plan.note(
            fp,
            format!(
                "channel {}: cooldown, last message {} s ago",
                recipient.id,
                now.signed_duration_since(last.at).num_seconds()
            ),
        );
        return;
    }

    // Heures calmes : seule la sévérité la plus haute passe tout de suite.
    let hold = if policy.in_quiet_hours(now) && item.severity < Severity::Critical {
        Hold::Quiet
    } else {
        Hold::Batch
    };

    plan.announced.insert(fp.to_string());
    match existing {
        Some(pos) => {
            let (queued, dirty) = &mut queue[pos];
            // Un rappel qui rattrape un déclenchement pas encore parti ne le
            // rétrograde pas en rappel : la première annonce reste une annonce.
            let keep_firing =
                queued.item.reason == NotifyReason::Firing && item.reason == NotifyReason::Reminder;
            let mut merged = item.clone();
            if keep_firing {
                merged.reason = NotifyReason::Firing;
            }
            queued.item = merged;
            queued.resolved_meanwhile = false;
            queued.hold = hold;
            *dirty = true;
        }
        None => queue.push((
            QueuedItem {
                id: None,
                channel_id: recipient.id,
                target_id: group.target_id,
                target_name: group.target_name.clone(),
                hold,
                item: item.clone(),
                resolved_meanwhile: false,
                queued_at: now,
            },
            true,
        )),
    }
}

/// Vide ce qui est mûr dans la file de chaque canal.
fn flush(
    recipients: &[&Recipient],
    policy: &GlobalPolicy,
    ledger: &Ledger,
    queue: &mut Vec<(QueuedItem, bool)>,
    plan: &mut Plan,
    now: DateTime<Utc>,
) {
    for recipient in recipients {
        let mine: Vec<&QueuedItem> =
            queue.iter().map(|(q, _)| q).filter(|q| q.channel_id == recipient.id).collect();
        if mine.is_empty() {
            continue;
        }

        let window =
            if recipient.is_oncall() { TimeDelta::zero() } else { secs(policy.batch_window_secs) };
        let quiet_now = recipient.policy.in_quiet_hours(now);
        let has_quiet = mine.iter().any(|q| q.hold == Hold::Quiet);
        let batch_due = mine
            .iter()
            .any(|q| q.hold == Hold::Batch && now.signed_duration_since(q.queued_at) >= window);
        // La fin des heures calmes libère tout : les lignes de regroupement pas
        // encore mûres partent avec, plutôt que dans un second message une minute
        // plus tard.
        let take_quiet = has_quiet && !quiet_now;
        let take_batch = batch_due || take_quiet;
        if !take_batch && !take_quiet {
            continue;
        }

        if policy.max_per_hour > 0 {
            let hour_ago = now - TimeDelta::seconds(LEDGER_HOUR_SECS);
            let sent = ledger
                .messages
                .iter()
                .filter(|(channel, at)| *channel == recipient.id && *at > hour_ago)
                .count();
            if sent >= policy.max_per_hour as usize {
                for q in &mine {
                    plan.note(
                        &q.item.fingerprint,
                        format!(
                            "channel {}: hourly cap of {} messages reached, waiting",
                            recipient.id, policy.max_per_hour
                        ),
                    );
                }
                continue;
            }
        }

        let ready: Vec<QueuedItem> = mine
            .into_iter()
            .filter(|q| match q.hold {
                Hold::Batch => take_batch,
                Hold::Quiet => take_quiet,
            })
            .cloned()
            .collect();
        let ready_keys: HashSet<(i64, String)> =
            ready.iter().map(|q| (q.channel_id, q.item.fingerprint.clone())).collect();
        queue.retain(|(q, _)| !ready_keys.contains(&(q.channel_id, q.item.fingerprint.clone())));

        let hold = take_quiet.then_some(Hold::Quiet);
        if recipient.is_oncall() {
            // Un message par équipement : c'est l'unité d'incident de l'astreinte.
            let mut by_target: BTreeMap<Option<TargetId>, Vec<QueuedItem>> = BTreeMap::new();
            for q in ready {
                by_target.entry(q.target_id).or_default().push(q);
            }
            for items in by_target.into_values() {
                plan.outgoing.push(outgoing(recipient.id, items, hold, now));
            }
        } else {
            plan.outgoing.push(outgoing(recipient.id, ready, hold, now));
        }
    }
}

/// Assemble un message à partir des lignes mûres d'un canal.
fn outgoing(
    channel_id: i64,
    items: Vec<QueuedItem>,
    hold: Option<Hold>,
    now: DateTime<Utc>,
) -> Outgoing {
    let mut groups: BTreeMap<Option<TargetId>, AlertGroup> = BTreeMap::new();
    let mut resolved_meanwhile = Vec::new();
    let mut queue_ids = Vec::new();
    let mut sent = Vec::new();

    for queued in &items {
        if let Some(id) = queued.id {
            queue_ids.push(id);
        }
        sent.push((queued.item.fingerprint.clone(), queued.item.reason));
        if queued.resolved_meanwhile {
            resolved_meanwhile.push((queued.target_name.clone(), queued.item.clone()));
            continue;
        }
        let group = groups.entry(queued.target_id).or_insert_with(|| AlertGroup {
            target_id: queued.target_id,
            target_name: queued.target_name.clone(),
            severity: Severity::Info,
            items: Vec::new(),
            channels: Vec::new(),
            at: now,
        });
        group.severity = group.severity.max(queued.item.severity);
        group.items.push(queued.item.clone());
    }

    let mut groups: Vec<AlertGroup> = groups.into_values().collect();
    for group in &mut groups {
        group.items.sort_by(|a, b| {
            b.severity.cmp(&a.severity).then_with(|| a.rule_name.cmp(&b.rule_name))
        });
    }
    groups.sort_by(|a, b| {
        b.severity.cmp(&a.severity).then_with(|| a.target_name.cmp(&b.target_name))
    });

    Outgoing {
        channel_id,
        digest: Digest { groups, resolved_meanwhile, hold, at: now },
        queue_ids,
        retry: items,
        sent,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::alerting::machine::EffectivePhase;

    fn at(seconds: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_700_000_000 + seconds, 0).expect("valid timestamp")
    }

    fn item(fp: &str, reason: NotifyReason, severity: Severity) -> GroupItem {
        GroupItem {
            fingerprint: fp.to_string(),
            rule_name: fp.to_string(),
            severity,
            reason,
            value: Some(95.0),
            score: None,
            unit: "%".to_string(),
            operator: ">".to_string(),
            threshold: 90.0,
            series_key: "s".to_string(),
            since: Some(at(0)),
            phase: EffectivePhase::Firing,
            note: None,
        }
    }

    fn group(target: TargetId, items: Vec<GroupItem>, now: DateTime<Utc>) -> AlertGroup {
        let severity = items.iter().map(|i| i.severity).max().unwrap_or(Severity::Info);
        AlertGroup {
            target_id: Some(target),
            target_name: format!("device-{target}"),
            severity,
            items,
            channels: Vec::new(),
            at: now,
        }
    }

    fn recipient(id: i64, policy: ChannelPolicy) -> Recipient {
        Recipient { id, kind: "ntfy".to_string(), enabled: true, policy }
    }

    fn immediate() -> GlobalPolicy {
        GlobalPolicy { batch_window_secs: 0, ..GlobalPolicy::default() }
    }

    /// Rejoue les effets d'un plan sur un registre, comme le moteur après des
    /// envois réussis.
    fn apply(ledger: &mut Ledger, plan: &Plan, now: DateTime<Utc>) {
        let first_id = ledger.queue.iter().filter_map(|q| q.id).max().unwrap_or(0) + 1;
        ledger.queue.retain(|q| !q.id.is_some_and(|id| plan.dequeue.contains(&id)));
        for (next_id, item) in (first_id..).zip(plan.enqueue.iter()) {
            ledger.queue.retain(|q| {
                !(q.channel_id == item.channel_id && q.item.fingerprint == item.item.fingerprint)
            });
            let mut stored = item.clone();
            stored.id = Some(next_id);
            ledger.queue.push(stored);
        }
        for out in &plan.outgoing {
            ledger.queue.retain(|q| !q.id.is_some_and(|id| out.queue_ids.contains(&id)));
            ledger.messages.push((out.channel_id, now));
            for (fp, reason) in &out.sent {
                ledger.log.push(LogEntry {
                    channel_id: out.channel_id,
                    fingerprint: fp.clone(),
                    reason: *reason,
                    at: now,
                });
            }
        }
        ledger.log.extend(plan.intake.iter().cloned());
        for (fp, until) in &plan.flap_holds {
            ledger.flap_holds.insert(fp.clone(), *until);
        }
    }

    #[test]
    fn sans_fenetre_un_groupe_part_tel_quel() {
        let groups =
            vec![group(1, vec![item("cpu", NotifyReason::Firing, Severity::Warning)], at(0))];
        let plan = plan(
            &groups,
            &[recipient(1, ChannelPolicy::default())],
            &immediate(),
            &Ledger::default(),
            at(0),
        );
        assert_eq!(plan.outgoing.len(), 1);
        assert_eq!(plan.outgoing[0].digest.item_count(), 1);
        assert!(plan.handled.contains("cpu"));
        assert!(plan.announced.contains("cpu"));
        assert!(plan.enqueue.is_empty(), "sent, not queued");
    }

    #[test]
    fn la_fenetre_de_regroupement_reunit_deux_equipements_en_un_message() {
        let mut ledger = Ledger::default();
        let policy = GlobalPolicy { batch_window_secs: 60, ..GlobalPolicy::default() };
        let channels = [recipient(1, ChannelPolicy::default())];

        let first = plan(
            &[group(1, vec![item("disk@1", NotifyReason::Firing, Severity::Warning)], at(0))],
            &channels,
            &policy,
            &ledger,
            at(0),
        );
        assert!(first.outgoing.is_empty(), "inside the window: held");
        assert_eq!(first.enqueue.len(), 1);
        assert!(first.announced.contains("disk@1"));
        apply(&mut ledger, &first, at(0));

        let second = plan(
            &[group(2, vec![item("disk@2", NotifyReason::Firing, Severity::Critical)], at(30))],
            &channels,
            &policy,
            &ledger,
            at(30),
        );
        assert!(second.outgoing.is_empty());
        apply(&mut ledger, &second, at(30));

        // Le plus ancien a soixante secondes : tout part, en un seul message.
        let third = plan(&[], &channels, &policy, &ledger, at(60));
        assert_eq!(third.outgoing.len(), 1);
        let digest = &third.outgoing[0].digest;
        assert_eq!(digest.groups.len(), 2);
        assert_eq!(digest.item_count(), 2);
        assert_eq!(digest.groups[0].target_name, "device-2", "most severe first");
        assert_eq!(third.outgoing[0].queue_ids.len(), 2);
    }

    #[test]
    fn une_alerte_resolue_pendant_l_attente_n_est_plus_qu_une_mention() {
        let mut ledger = Ledger::default();
        let policy = GlobalPolicy { batch_window_secs: 60, ..GlobalPolicy::default() };
        let channels = [recipient(1, ChannelPolicy::default())];

        let fired = plan(
            &[group(1, vec![item("cpu", NotifyReason::Firing, Severity::Warning)], at(0))],
            &channels,
            &policy,
            &ledger,
            at(0),
        );
        apply(&mut ledger, &fired, at(0));

        let resolved = plan(
            &[group(1, vec![item("cpu", NotifyReason::Resolved, Severity::Warning)], at(20))],
            &channels,
            &policy,
            &ledger,
            at(20),
        );
        assert!(resolved.outgoing.is_empty());
        assert!(resolved.enqueue[0].resolved_meanwhile);
        apply(&mut ledger, &resolved, at(20));

        let flushed = plan(&[], &channels, &policy, &ledger, at(60));
        assert_eq!(flushed.outgoing.len(), 1);
        let digest = &flushed.outgoing[0].digest;
        assert!(digest.groups.is_empty(), "nothing is still firing");
        assert_eq!(digest.resolved_meanwhile.len(), 1);
    }

    #[test]
    fn le_filtre_de_severite_et_le_refus_des_resolutions_sont_par_canal() {
        let strict = recipient(
            1,
            ChannelPolicy {
                min_severity: Severity::Critical,
                notify_resolved: false,
                ..ChannelPolicy::default()
            },
        );
        let lax = recipient(2, ChannelPolicy::default());
        let groups = vec![group(
            1,
            vec![
                item("cpu", NotifyReason::Firing, Severity::Warning),
                item("down", NotifyReason::Firing, Severity::Critical),
            ],
            at(0),
        )];
        let plan =
            plan(&groups, &[strict.clone(), lax.clone()], &immediate(), &Ledger::default(), at(0));
        let to_strict = plan.outgoing.iter().find(|o| o.channel_id == 1).expect("strict channel");
        assert_eq!(to_strict.digest.item_count(), 1);
        assert_eq!(to_strict.sent[0].0, "down");
        let to_lax = plan.outgoing.iter().find(|o| o.channel_id == 2).expect("lax channel");
        assert_eq!(to_lax.digest.item_count(), 2);

        // La résolution de « down » : le canal strict ne l'annonce pas.
        let mut ledger = Ledger::default();
        apply(&mut ledger, &plan, at(0));
        let resolved =
            vec![group(1, vec![item("down", NotifyReason::Resolved, Severity::Critical)], at(100))];
        let plan = super::plan(&resolved, &[strict, lax], &immediate(), &ledger, at(100));
        assert_eq!(plan.outgoing.len(), 1);
        assert_eq!(plan.outgoing[0].channel_id, 2);
        assert!(plan.verdicts["down"].contains("does not announce resolutions"));
    }

    #[test]
    fn une_resolution_ne_part_que_sur_les_canaux_qui_ont_vu_le_declenchement() {
        // Le déclenchement est parti seulement sur le canal 2 (canal 1 filtré).
        let mut ledger = Ledger::default();
        ledger.log.push(LogEntry {
            channel_id: 2,
            fingerprint: "cpu".into(),
            reason: NotifyReason::Firing,
            at: at(0),
        });
        let channels =
            [recipient(1, ChannelPolicy::default()), recipient(2, ChannelPolicy::default())];
        let groups =
            vec![group(1, vec![item("cpu", NotifyReason::Resolved, Severity::Warning)], at(60))];
        let plan = plan(&groups, &channels, &immediate(), &ledger, at(60));
        assert_eq!(plan.outgoing.len(), 1);
        assert_eq!(plan.outgoing[0].channel_id, 2);
    }

    #[test]
    fn le_delai_minimal_par_canal_bloque_une_repetition_trop_proche() {
        let channel =
            recipient(1, ChannelPolicy { min_interval_secs: 600, ..ChannelPolicy::default() });
        let mut ledger = Ledger::default();
        let first = plan(
            &[group(1, vec![item("cpu", NotifyReason::Firing, Severity::Warning)], at(0))],
            std::slice::from_ref(&channel),
            &immediate(),
            &ledger,
            at(0),
        );
        assert_eq!(first.outgoing.len(), 1);
        apply(&mut ledger, &first, at(0));

        // Résolution puis nouveau déclenchement trois minutes plus tard.
        let resolved = plan(
            &[group(1, vec![item("cpu", NotifyReason::Resolved, Severity::Warning)], at(60))],
            std::slice::from_ref(&channel),
            &immediate(),
            &ledger,
            at(60),
        );
        assert_eq!(resolved.outgoing.len(), 1, "resolutions are not subject to the cooldown");
        apply(&mut ledger, &resolved, at(60));

        let again = plan(
            &[group(1, vec![item("cpu", NotifyReason::Firing, Severity::Warning)], at(180))],
            std::slice::from_ref(&channel),
            &immediate(),
            &ledger,
            at(180),
        );
        assert!(again.outgoing.is_empty());
        assert!(again.handled.contains("cpu"), "still marked, or the cycle would replay it");
        assert!(again.verdicts["cpu"].contains("cooldown"));

        // Passé le délai, un rappel passe.
        let later = plan(
            &[group(1, vec![item("cpu", NotifyReason::Reminder, Severity::Warning)], at(700))],
            &[channel],
            &immediate(),
            &ledger,
            at(700),
        );
        assert_eq!(later.outgoing.len(), 1);
    }

    #[test]
    fn le_plafond_horaire_retient_puis_resume() {
        let policy =
            GlobalPolicy { batch_window_secs: 0, max_per_hour: 2, ..GlobalPolicy::default() };
        let channels = [recipient(1, ChannelPolicy::default())];
        let mut ledger = Ledger::default();
        for (i, t) in [0, 60].iter().enumerate() {
            let p = plan(
                &[group(
                    1,
                    vec![item(&format!("a{i}"), NotifyReason::Firing, Severity::Warning)],
                    at(*t),
                )],
                &channels,
                &policy,
                &ledger,
                at(*t),
            );
            assert_eq!(p.outgoing.len(), 1);
            apply(&mut ledger, &p, at(*t));
        }
        // Troisième et quatrième messages dans l'heure : retenus.
        for (i, t) in [120, 180].iter().enumerate() {
            let p = plan(
                &[group(
                    2,
                    vec![item(&format!("b{i}"), NotifyReason::Firing, Severity::Warning)],
                    at(*t),
                )],
                &channels,
                &policy,
                &ledger,
                at(*t),
            );
            assert!(p.outgoing.is_empty(), "cap reached");
            assert!(p.verdicts[&format!("b{i}")].contains("hourly cap"));
            apply(&mut ledger, &p, at(*t));
        }
        assert_eq!(ledger.queue.len(), 2);

        // Une heure après le premier message, une place se libère : tout part en
        // un seul résumé.
        let p = plan(&[], &channels, &policy, &ledger, at(3601));
        assert_eq!(p.outgoing.len(), 1);
        assert_eq!(p.outgoing[0].digest.item_count(), 2);
    }

    #[test]
    fn les_heures_calmes_laissent_passer_le_critique_et_retiennent_le_reste() {
        // Heures calmes tous les jours de 22 h à 7 h, UTC. 1_700_000_000 est un
        // mardi 14 novembre 2023 à 22:13:20 UTC.
        let quiet = Schedule::Weekly {
            days: vec![0, 1, 2, 3, 4, 5, 6],
            start_minute: 22 * 60,
            end_minute: 7 * 60,
            utc_offset_minutes: 0,
        };
        let channels =
            [recipient(1, ChannelPolicy { quiet_hours: Some(quiet), ..ChannelPolicy::default() })];
        let policy = GlobalPolicy { batch_window_secs: 0, ..GlobalPolicy::default() };
        let mut ledger = Ledger::default();

        let night = plan(
            &[group(
                1,
                vec![
                    item("down", NotifyReason::Firing, Severity::Critical),
                    item("cpu", NotifyReason::Firing, Severity::Warning),
                ],
                at(0),
            )],
            &channels,
            &policy,
            &ledger,
            at(0),
        );
        assert_eq!(night.outgoing.len(), 1);
        assert_eq!(night.outgoing[0].sent[0].0, "down", "critical goes through at night");
        assert_eq!(night.enqueue.len(), 1);
        assert_eq!(night.enqueue[0].hold, Hold::Quiet);
        apply(&mut ledger, &night, at(0));

        // Toujours la nuit : rien ne bouge.
        let still_night = plan(&[], &channels, &policy, &ledger, at(3600));
        assert!(still_night.outgoing.is_empty());

        // 07:00 UTC le lendemain = 1_700_000_000 + 8 h 46 min 40 s.
        let morning = at(8 * 3600 + 46 * 60 + 40);
        let digest = plan(&[], &channels, &policy, &ledger, morning);
        assert_eq!(digest.outgoing.len(), 1);
        assert_eq!(digest.outgoing[0].digest.hold, Some(Hold::Quiet));
        assert_eq!(digest.outgoing[0].sent[0].0, "cpu");
    }

    #[test]
    fn le_battement_envoie_un_avis_puis_retient() {
        let channels = [recipient(1, ChannelPolicy::default())];
        let policy = GlobalPolicy { batch_window_secs: 0, ..GlobalPolicy::default() };
        let mut ledger = Ledger::default();

        let mut sent_reasons = Vec::new();
        for (i, reason) in [
            NotifyReason::Firing,
            NotifyReason::Resolved,
            NotifyReason::Firing,
            NotifyReason::Resolved,
        ]
        .into_iter()
        .enumerate()
        {
            let t = at(i as i64 * 120);
            let p = plan(
                &[group(1, vec![item("svc", reason, Severity::Warning)], t)],
                &channels,
                &policy,
                &ledger,
                t,
            );
            sent_reasons.extend(p.outgoing.iter().flat_map(|o| o.sent.iter().map(|(_, r)| *r)));
            apply(&mut ledger, &p, t);
        }
        assert_eq!(
            sent_reasons,
            vec![
                NotifyReason::Firing,
                NotifyReason::Resolved,
                NotifyReason::Firing,
                NotifyReason::Flapping
            ],
            "the fourth change becomes a single flapping notice"
        );
        assert!(ledger.flap_holds.contains_key("svc"));

        // Pendant la retenue : plus rien ne part.
        let held = plan(
            &[group(1, vec![item("svc", NotifyReason::Firing, Severity::Warning)], at(600))],
            &channels,
            &policy,
            &ledger,
            at(600),
        );
        assert!(held.outgoing.is_empty());
        assert!(held.verdicts["svc"].contains("flapping"));
        assert!(held.handled.contains("svc"));

        // Retenue expirée : le compteur repart (les anciens changements sont hors
        // fenêtre), une résolution passe car le dernier envoi était un avis.
        let after = at(600 + 30 * 60);
        let resolved = plan(
            &[group(1, vec![item("svc", NotifyReason::Resolved, Severity::Warning)], after)],
            &channels,
            &policy,
            &ledger,
            after,
        );
        assert_eq!(resolved.outgoing.len(), 1);
        assert_eq!(resolved.outgoing[0].sent[0].1, NotifyReason::Resolved);
    }

    #[test]
    fn un_canal_d_astreinte_recoit_un_message_par_equipement_sans_attendre() {
        let oncall = Recipient {
            id: 9,
            kind: "pagerduty".to_string(),
            enabled: true,
            policy: ChannelPolicy::default(),
        };
        let policy = GlobalPolicy { batch_window_secs: 60, ..GlobalPolicy::default() };
        let groups = vec![
            group(1, vec![item("a", NotifyReason::Firing, Severity::Warning)], at(0)),
            group(2, vec![item("b", NotifyReason::Firing, Severity::Warning)], at(0)),
        ];
        let plan = plan(&groups, &[oncall], &policy, &Ledger::default(), at(0));
        assert_eq!(plan.outgoing.len(), 2, "one incident per device, no window");
        assert!(plan.outgoing.iter().all(|o| o.digest.groups.len() == 1));
    }

    #[test]
    fn un_rappel_qui_rattrape_un_declenchement_en_attente_reste_un_declenchement() {
        let channels = [recipient(1, ChannelPolicy::default())];
        let policy = GlobalPolicy { batch_window_secs: 120, ..GlobalPolicy::default() };
        let mut ledger = Ledger::default();
        let first = plan(
            &[group(1, vec![item("cpu", NotifyReason::Firing, Severity::Warning)], at(0))],
            &channels,
            &policy,
            &ledger,
            at(0),
        );
        apply(&mut ledger, &first, at(0));
        let reminder = plan(
            &[group(1, vec![item("cpu", NotifyReason::Reminder, Severity::Critical)], at(30))],
            &channels,
            &policy,
            &ledger,
            at(30),
        );
        assert_eq!(reminder.enqueue.len(), 1);
        assert_eq!(reminder.enqueue[0].item.reason, NotifyReason::Firing);
        assert_eq!(
            reminder.enqueue[0].item.severity,
            Severity::Critical,
            "escalated severity kept"
        );
    }

    #[test]
    fn sans_canal_les_empreintes_sont_traitees_sans_rien_envoyer() {
        let groups =
            vec![group(1, vec![item("cpu", NotifyReason::Firing, Severity::Warning)], at(0))];
        let plan = plan(&groups, &[], &immediate(), &Ledger::default(), at(0));
        assert!(plan.outgoing.is_empty());
        assert!(plan.handled.contains("cpu"));
        assert!(!plan.announced.contains("cpu"));
    }

    #[test]
    fn les_reglages_de_canal_se_desserialisent_avec_leurs_defauts() {
        let policy: ChannelPolicy = serde_json::from_str("{}").unwrap();
        assert_eq!(policy, ChannelPolicy::default());
        let policy: ChannelPolicy =
            serde_json::from_str(r#"{"min_severity":"critical","notify_resolved":false}"#).unwrap();
        assert_eq!(policy.min_severity, Severity::Critical);
        assert!(!policy.notify_resolved);
        let global: GlobalPolicy = serde_json::from_str(r#"{"batch_window_secs":0}"#).unwrap();
        assert_eq!(global.batch_window_secs, 0);
        assert_eq!(global.max_per_hour, DEFAULT_MAX_PER_HOUR);
    }

    #[test]
    fn l_url_publique_retombe_sur_l_environnement() {
        let policy = GlobalPolicy::default();
        assert_eq!(policy.public_url(Some("https://monit.lan/")), Some("https://monit.lan".into()));
        assert_eq!(policy.public_url(None), None);
        let set = GlobalPolicy { public_url: "https://a.b/".into(), ..GlobalPolicy::default() };
        assert_eq!(set.public_url(Some("https://x")), Some("https://a.b".into()));
    }
}
