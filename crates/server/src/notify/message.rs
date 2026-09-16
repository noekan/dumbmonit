//! Mise en forme des messages envoyés aux canaux.
//!
//! Un même contenu est rendu en texte brut et en Markdown : les services diffèrent
//! sur ce qu'ils acceptent, mais l'information doit être la même partout, y compris
//! sur une montre qui n'affichera que le titre.

use std::collections::BTreeMap;

use chrono::{DateTime, TimeDelta, Utc};

use crate::alerting::group::{AlertGroup, GroupItem, NotifyReason};
use crate::alerting::machine::EffectivePhase;
use crate::alerting::model::Severity;
use crate::alerting::notify_policy::{DIGEST_MAX_LINES, Digest, Hold};

/// Message prêt à partir, indépendant du canal.
#[derive(Debug, Clone, PartialEq)]
pub struct Message {
    pub title: String,
    /// Corps en texte brut, pour ntfy, Gotify et le courriel.
    pub text: String,
    /// Corps en Markdown, pour Discord, Slack et Telegram.
    pub markdown: String,
    pub severity: Severity,
    /// Vrai si le message annonce uniquement des retours à la normale.
    pub resolved: bool,
    pub target_name: String,
    pub target_id: Option<i64>,
    pub at: DateTime<Utc>,

    // Les champs qui suivent décrivent la *première* alerte du groupe. Un message
    // groupé en contient parfois cinq, mais un gabarit personnalisé et une clé de
    // déduplication d'astreinte ont besoin d'un dénominateur unique ; `count` dit
    // combien d'autres se cachent derrière.
    /// Nom de la règle déclenchée.
    pub rule_name: String,
    /// Empreinte de l'alerte, stable d'un cycle à l'autre : c'est ce qui permet à
    /// PagerDuty ou Opsgenie de rapprocher une résolution de son déclenchement.
    pub fingerprint: String,
    pub value: Option<f64>,
    pub threshold: Option<f64>,
    pub unit: String,
    pub operator: String,
    /// Nombre d'alertes réunies dans ce message.
    pub count: usize,
    /// Lien vers l'équipement (ou l'instance) dans DumbMonit, quand une URL
    /// publique est connue. Déjà inclus dans `text` et `markdown`.
    pub link: Option<String>,
}

/// Variables utilisables dans un gabarit de canal personnalisé, avec leur
/// description telle qu'affichée dans la documentation.
///
/// La liste est la source de vérité : un gabarit qui cite un nom absent d'ici est
/// refusé dès l'enregistrement, plutôt que de partir vide au premier incident.
pub const TEMPLATE_VARIABLES: &[(&str, &str)] = &[
    ("title", "Full message title"),
    ("message", "Message body in plain text, one line per alert"),
    ("message_markdown", "Message body in Markdown"),
    ("rule", "Name of the alert rule"),
    ("target", "Name of the monitored device"),
    ("target_id", "Numeric identifier of the device, empty if it has none"),
    ("severity", "Technical severity: info, warning or critical"),
    (
        "severity_fr",
        "Severity in French (information, avertissement or critique), kept for existing templates",
    ),
    ("status", "Technical state: firing or resolved"),
    ("status_fr", "State in French (« en cours » or « résolu »), kept for existing templates"),
    ("value", "Measured value with its unit; empty if the alert carries none"),
    ("value_raw", "Measured value without unit or rounding, usable as a JSON number"),
    ("threshold", "Crossed threshold with its unit"),
    ("threshold_raw", "Threshold without unit, usable as a JSON number"),
    ("unit", "Unit of the value, for example % or °C"),
    ("operator", "Threshold comparison, for example > or <"),
    ("count", "Number of alerts grouped in this message"),
    ("fingerprint", "Stable identifier of the alert, handy as a deduplication key"),
    ("timestamp", "Timestamp in ISO 8601 format, for example 2026-09-01T14:32:05+00:00"),
    ("timestamp_unix", "Timestamp in seconds since 1970"),
    ("date", "Human-readable timestamp in UTC, for example 2026-09-01 14:32 UTC"),
    ("priority", "Priority from 1 (low) to 5 (high)"),
    ("color", "Accent color in hexadecimal without the hash sign, for example D98A00"),
    ("color_hex", "Accent color with the hash sign, for example #D98A00"),
    ("emoji", "State glyph: 🔴, ⚠️, ℹ️ or ✅"),
    ("link", "Link to DumbMonit, if \"base_url\" is set in the settings"),
    ("source", "Always \"dumbmonit\": identifies the sender"),
    ("token", "The channel's \"token\" secret, for services that expect it in the body"),
];

impl Message {
    /// Table de substitution du gabarit personnalisé.
    ///
    /// `link` et `token` sont laissés vides : seul le canal connaît son URL publique
    /// et son secret, et il les complète avant le rendu.
    pub fn variables(&self) -> BTreeMap<&'static str, String> {
        let number = |value: Option<f64>| {
            value.filter(|v| v.is_finite()).map(|v| format!("{v}")).unwrap_or_default()
        };
        let with_unit =
            |value: Option<f64>| value.map(|v| format_value(v, &self.unit)).unwrap_or_default();

        BTreeMap::from([
            ("title", self.title.clone()),
            ("message", self.text.clone()),
            ("message_markdown", self.markdown.clone()),
            ("rule", self.rule_name.clone()),
            ("target", self.target_name.clone()),
            ("target_id", self.target_id.map(|id| id.to_string()).unwrap_or_default()),
            ("severity", self.severity.as_str().to_string()),
            ("severity_fr", self.severity_fr().to_string()),
            ("status", self.status().to_string()),
            ("status_fr", if self.resolved { "résolu" } else { "en cours" }.to_string()),
            ("value", with_unit(self.value)),
            ("value_raw", number(self.value)),
            ("threshold", with_unit(self.threshold)),
            ("threshold_raw", number(self.threshold)),
            ("unit", self.unit.clone()),
            ("operator", self.operator.clone()),
            ("count", self.count.to_string()),
            ("fingerprint", self.fingerprint.clone()),
            ("timestamp", self.at.to_rfc3339()),
            ("timestamp_unix", self.at.timestamp().to_string()),
            ("date", self.at.format("%Y-%m-%d %H:%M UTC").to_string()),
            ("priority", self.priority().to_string()),
            ("color", format!("{:06X}", self.color())),
            ("color_hex", format!("#{:06X}", self.color())),
            ("emoji", self.emoji().to_string()),
            ("link", self.link.clone().unwrap_or_default()),
            ("source", "dumbmonit".to_string()),
            ("token", String::new()),
        ])
    }

    /// Ajoute le lien vers l'équipement, en fin de corps et dans `link`.
    ///
    /// Le lien est du texte comme le reste : les services qui n'affichent que le
    /// corps brut (ntfy, courriel) le montrent aussi, et un lien cliquable dans
    /// un message qui dit « le NAS va mal » est ce qu'on attend d'un téléphone.
    pub fn attach_link(&mut self, base_url: &str) {
        let base = base_url.trim_end_matches('/');
        let link = match self.target_id {
            Some(id) => format!("{base}/targets/{id}"),
            None => base.to_string(),
        };
        if !self.text.is_empty() {
            self.text.push('\n');
        }
        self.text.push_str(&link);
        self.markdown.push_str(&format!("\n[Open in DumbMonit]({link})"));
        self.link = Some(link);
    }

    /// Clé identifiant le fil de discussion d'un équipement.
    ///
    /// Un message DumbMonit couvre un équipement et un cycle, jamais une règle isolée :
    /// c'est donc l'équipement qui identifie l'incident à ouvrir puis à refermer. Se
    /// caler sur l'empreinte de la règle casserait la fermeture dès qu'un équipement
    /// porte deux alertes, puisque la résolution de l'une ne cite plus l'autre.
    pub fn group_key(&self) -> String {
        match self.target_id {
            Some(id) => format!("target-{id}"),
            None => format!("target-{}", self.target_name),
        }
    }

    /// État technique, tel que l'attendent les webhooks et les outils d'astreinte.
    pub fn status(&self) -> &'static str {
        if self.resolved { "resolved" } else { "firing" }
    }

    pub fn severity_fr(&self) -> &'static str {
        match self.severity {
            Severity::Info => "information",
            Severity::Warning => "avertissement",
            Severity::Critical => "critique",
        }
    }

    /// Sévérité telle qu'affichée dans les messages, dans le vocabulaire de
    /// l'interface.
    pub fn severity_label(&self) -> &'static str {
        match self.severity {
            Severity::Info => "advisory",
            Severity::Warning => "warning",
            Severity::Critical => "critical",
        }
    }

    /// Pictogramme du message entier. Sur un téléphone, c'est souvent la seule
    /// chose lue avant de déverrouiller.
    pub fn emoji(&self) -> &'static str {
        if self.resolved {
            return "✅";
        }
        match self.severity {
            Severity::Info => "ℹ️",
            Severity::Warning => "⚠️",
            Severity::Critical => "🔴",
        }
    }

    /// Couleur d'accentuation, au format entier attendu par Discord et Slack.
    pub fn color(&self) -> u32 {
        if self.resolved {
            return 0x2E_A0_43; // vert
        }
        match self.severity {
            Severity::Info => 0x31_74_D9,
            Severity::Warning => 0xD9_8A_00,
            Severity::Critical => 0xCF_22_2E,
        }
    }

    /// Priorité normalisée 1..5, que ntfy et Gotify interprètent chacun à leur façon.
    pub fn priority(&self) -> u8 {
        if self.resolved {
            return 2;
        }
        match self.severity {
            Severity::Info => 2,
            Severity::Warning => 4,
            Severity::Critical => 5,
        }
    }
}

/// Pictogramme d'un état. Sur un téléphone, c'est souvent la seule chose lue.
fn glyph(item: &GroupItem) -> &'static str {
    match item.reason {
        NotifyReason::Resolved => "✅",
        NotifyReason::Flapping => "🔁",
        _ => match item.severity {
            Severity::Info => "ℹ️",
            Severity::Warning => "⚠️",
            Severity::Critical => "🔴",
        },
    }
}

/// Mot d'état accolé au pictogramme : un lecteur d'écran, un courriel en texte
/// brut ou une montre monochrome ne voient pas la couleur du glyphe.
fn state_word(item: &GroupItem) -> &'static str {
    match item.reason {
        NotifyReason::Resolved => "Resolved",
        NotifyReason::Flapping => "Flapping",
        _ => match item.severity {
            Severity::Info => "Info",
            Severity::Warning => "Warning",
            Severity::Critical => "Critical",
        },
    }
}

/// Étiquettes qui identifient l'équipement entier, pas la série : elles ne
/// disent rien de plus que le nom déjà en titre.
const TARGET_LABELS: [&str; 7] =
    ["__name__", "target", "target_id", "host", "hostname", "instance", "address"];

/// Étiquettes qui nomment le mieux une série, par ordre de préférence. Une VM
/// se reconnaît à son nom, une sauvegarde à son groupe, un port à son nom.
const IDENTITY_LABELS: [&str; 24] = [
    "name",
    "container",
    "service",
    "task",
    "job",
    "group",
    "url",
    "node",
    "cluster",
    "datastore",
    "storage",
    "disk",
    "device",
    "mountpoint",
    "filesystem",
    "volume",
    "pool",
    "ifname",
    "interface",
    "ifalias",
    "worktype",
    "process",
    "unit",
    "core",
];

/// Nombre maximal de valeurs d'étiquettes citées pour nommer une série.
const IDENTITY_MAX: usize = 2;

/// Étiquettes d'une clé de série `metric{a="1",b="2"}`, sans le nom de métrique.
///
/// Les valeurs ne sont pas échappées dans la clé : une virgule n'y termine une
/// valeur que si elle suit un guillemet fermant, ce qui laisse passer les
/// descriptions de ports (« Workshop, rack 2 »).
fn series_labels(series_key: &str) -> BTreeMap<&str, &str> {
    let mut labels = BTreeMap::new();
    let Some(start) = series_key.find('{') else { return labels };
    let mut rest = series_key[start + 1..].strip_suffix('}').unwrap_or(&series_key[start + 1..]);
    while !rest.is_empty() {
        let Some((key, after_key)) = rest.split_once("=\"") else { break };
        let Some(end) = after_key.match_indices('"').map(|(i, _)| i).find(|&i| {
            let next = after_key[i + 1..].chars().next();
            next.is_none() || next == Some(',')
        }) else {
            break;
        };
        labels.insert(key, &after_key[..end]);
        rest = after_key[end + 1..].strip_prefix(',').unwrap_or("");
    }
    labels
}

/// Nom de la série derrière l'alerte : la VM, le port, le point de montage.
///
/// Sans lui, deux « Service down » sur le même équipement sont indiscernables
/// et un « VM stopped » ne dit pas laquelle. `None` quand la série n'a que des
/// étiquettes d'équipement (`host_down`, par exemple).
pub fn series_identity(series_key: &str) -> Option<String> {
    let labels = series_labels(series_key);
    // Une VM ou un conteneur se lit « nom (identifiant) » : ce qu'affiche Proxmox.
    if let Some(name) = labels.get("name") {
        return Some(match labels.get("vmid") {
            Some(vmid) => format!("{name} ({vmid})"),
            None => (*name).to_string(),
        });
    }
    let mut parts: Vec<&str> = IDENTITY_LABELS
        .iter()
        .filter_map(|label| labels.get(label).copied())
        .filter(|value| !value.is_empty())
        .take(IDENTITY_MAX)
        .collect();
    if parts.is_empty() {
        parts = labels
            .iter()
            .filter(|(key, value)| {
                !TARGET_LABELS.contains(key) && !key.starts_with("tag_") && !value.is_empty()
            })
            .map(|(_, value)| *value)
            .take(IDENTITY_MAX)
            .collect();
    }
    if parts.is_empty() { None } else { Some(parts.join(" · ")) }
}

/// Vrai pour une règle « tout ou rien » sur une métrique 0/1 : « 1 (threshold
/// > 0) » n'apprend rien que le nom de la règle ne dise déjà.
fn is_boolean(item: &GroupItem) -> bool {
    let Some(value) = item.value else { return false };
    let is_bit = |v: f64| v == 0.0 || v == 1.0;
    is_bit(value)
        && is_bit(item.threshold)
        && item.unit.is_empty()
        && matches!(
            (item.operator.as_str(), item.threshold as u8),
            (">", 0) | (">=", 1) | ("<", 1) | ("<=", 0)
        )
}

/// Tronque un texte sur une frontière de caractère, en signalant la coupe.
///
/// Compter en caractères et non en octets : un titre accentué coupé au milieu d'un
/// UTF-8 ferait échouer la sérialisation, pas seulement l'affichage. Plusieurs
/// services refusent en bloc un champ trop long, ce qui rend la troncature
/// indispensable et non cosmétique.
pub fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let kept: String = text.chars().take(max.saturating_sub(1)).collect();
    format!("{kept}…")
}

/// Formate une valeur pour l'affichage.
///
/// Les grandeurs de supervision se lisent à une décimale près ; au-delà, on affiche
/// du bruit de mesure.
pub fn format_value(value: f64, unit: &str) -> String {
    if !value.is_finite() {
        return "n/a".to_string();
    }
    let rendered = if value.abs() >= 100.0 || value.fract().abs() < 1e-9 {
        format!("{}", value.round() as i64)
    } else {
        format!("{value:.1}")
    };
    if unit.is_empty() { rendered } else { format!("{rendered} {unit}") }
}

/// Formate une durée en français, à la granularité utile.
pub fn format_duration(delta: TimeDelta) -> String {
    let seconds = delta.num_seconds().max(0);
    match seconds {
        0..=59 => format!("{seconds} s"),
        60..=3599 => format!("{} min", seconds / 60),
        3600..=86_399 => format!("{} h", seconds / 3600),
        _ => format!("{} d", seconds / 86_400),
    }
}

fn line(item: &GroupItem, now: DateTime<Utc>) -> String {
    let mut line = format!("{} {} · {}", glyph(item), state_word(item), item.rule_name);

    if let Some(series) = series_identity(&item.series_key) {
        line.push_str(&format!(" — {series}"));
    }
    if let Some(value) = item.value
        && !is_boolean(item)
    {
        line.push_str(&format!(" — {}", format_value(value, &item.unit)));
        // Le seuil n'est rappelé que pour une alerte en cours : sur une résolution,
        // ce qui compte est que la valeur soit revenue, pas ce qu'elle a franchi.
        if item.reason != NotifyReason::Resolved {
            line.push_str(&format!(
                " (threshold {} {})",
                item.operator,
                format_value(item.threshold, &item.unit)
            ));
        }
    }
    if let Some(score) = item.score {
        line.push_str(&format!(" [deviation {score:.1} σ]"));
    }
    if let Some(since) = item.since
        && item.reason != NotifyReason::Resolved
    {
        line.push_str(&format!(", for {}", format_duration(now.signed_duration_since(since))));
    }
    if item.reason == NotifyReason::Escalation {
        line.push_str(" — escalation");
    }
    if item.phase == EffectivePhase::Suppressed {
        line.push_str(" — suppressed");
    }
    if let Some(note) = &item.note {
        line.push_str(&format!(" — {note}"));
    }
    line
}

/// Rend un groupe d'alertes en message.
pub fn render(group: &AlertGroup) -> Message {
    let resolved = group.is_resolution();
    let count = group.items.len();

    let title = if resolved {
        format!("Resolved — {}", group.target_name)
    } else if count == 1 {
        format!("{} — {}", group.target_name, group.items[0].rule_name)
    } else {
        format!("{} — {count} alerts", group.target_name)
    };

    let body: Vec<String> = group.items.iter().map(|item| line(item, group.at)).collect();
    let text = body.join("\n");
    let markdown = format!(
        "**{}**\n{}",
        title,
        body.iter().map(|l| format!("• {l}")).collect::<Vec<_>>().join("\n")
    );

    // Le premier élément représente le groupe pour tout ce qui exige une valeur
    // unique : gabarit personnalisé, clé de déduplication d'astreinte. Un groupe
    // sans élément ne devrait pas exister, mais le supposer coûterait une panique.
    let head = group.items.first();

    Message {
        title,
        text,
        markdown,
        severity: group.severity,
        resolved,
        target_name: group.target_name.clone(),
        target_id: group.target_id,
        at: group.at,
        rule_name: head.map(|item| item.rule_name.clone()).unwrap_or_default(),
        fingerprint: head.map(|item| item.fingerprint.clone()).unwrap_or_default(),
        value: head.and_then(|item| item.value),
        threshold: head.map(|item| item.threshold),
        unit: head.map(|item| item.unit.clone()).unwrap_or_default(),
        operator: head.map(|item| item.operator.clone()).unwrap_or_default(),
        count,
        link: None,
    }
}

/// Rend un résumé : un message par canal, plusieurs équipements dedans.
///
/// Un résumé d'un seul équipement, sans mention ni clôture d'heures calmes,
/// est rendu exactement comme un groupe simple : le format habituel reste la
/// norme, le résumé n'apparaît que quand il y a réellement plusieurs choses à
/// dire. `public_url` ajoute un lien par équipement.
pub fn render_digest(digest: &Digest, public_url: Option<&str>) -> Message {
    if digest.groups.len() == 1 && digest.resolved_meanwhile.is_empty() && digest.hold.is_none() {
        let mut message = render(&digest.groups[0]);
        if let Some(base) = public_url {
            message.attach_link(base);
        }
        return message;
    }

    let firing: usize = digest
        .groups
        .iter()
        .flat_map(|group| group.items.iter())
        .filter(|item| item.reason != NotifyReason::Resolved)
        .count();
    let resolved: usize = digest.item_count() - firing + digest.resolved_meanwhile.len();
    let devices = digest.groups.len();
    let all_resolved = firing == 0;

    let mut summary = Vec::new();
    if firing > 0 {
        summary.push(format!("{firing} alert{}", if firing > 1 { "s" } else { "" }));
    }
    if resolved > 0 {
        summary.push(format!("{resolved} resolved"));
    }
    let scope = match devices {
        0 => String::new(),
        1 => format!(" on {}", digest.groups[0].target_name),
        n => format!(" on {n} devices"),
    };
    let mut title = format!("{}{scope}", summary.join(", "));
    if digest.hold == Some(Hold::Quiet) {
        title = format!("Quiet hours over — {title}");
    }

    // Les lignes sont plafonnées par message, pas par équipement : un résumé de
    // trente lignes après une nuit calme n'apprend rien de plus que douze.
    let mut text_lines: Vec<String> = Vec::new();
    let mut md_lines: Vec<String> = Vec::new();
    let mut shown = 0usize;
    let mut omitted = 0usize;
    for group in &digest.groups {
        let name = if devices > 1 || digest.hold.is_some() {
            Some(group.target_name.clone())
        } else {
            None
        };
        let mut wrote_header = false;
        for item in &group.items {
            if shown >= DIGEST_MAX_LINES {
                omitted += 1;
                continue;
            }
            if !wrote_header && let Some(name) = &name {
                text_lines.push(format!("{name}:"));
                md_lines.push(format!("**{name}**"));
                wrote_header = true;
            }
            let rendered = line(item, digest.at);
            text_lines.push(if name.is_some() {
                format!("  {rendered}")
            } else {
                rendered.clone()
            });
            md_lines.push(format!("• {rendered}"));
            shown += 1;
        }
        if wrote_header
            && let Some(base) = public_url
            && let Some(id) = group.target_id
        {
            let link = format!("{}/targets/{id}", base.trim_end_matches('/'));
            text_lines.push(format!("  {link}"));
            md_lines.push(format!("  [Open {}]({link})", group.target_name));
        }
    }
    if omitted > 0 {
        let more = format!("…and {omitted} more alert{}", if omitted > 1 { "s" } else { "" });
        text_lines.push(more.clone());
        md_lines.push(more);
    }
    if !digest.resolved_meanwhile.is_empty() {
        let names: Vec<String> = digest
            .resolved_meanwhile
            .iter()
            .map(|(target, item)| match series_identity(&item.series_key) {
                Some(series) => format!("{} — {series} ({target})", item.rule_name),
                None => format!("{} ({target})", item.rule_name),
            })
            .collect();
        let label = match digest.hold {
            Some(Hold::Quiet) => "Resolved during quiet hours",
            _ => "Resolved before this was sent",
        };
        text_lines.push(format!("{label}: {}", names.join(", ")));
        md_lines.push(format!("_{label}: {}_", names.join(", ")));
    }

    let text = text_lines.join("\n");
    let markdown = format!("**{title}**\n{}", md_lines.join("\n"));
    // La sévérité du message est celle de la ligne la plus grave, pas celle que
    // porte l'en-tête du groupe : c'est ce que l'utilisateur lit.
    let severity = digest
        .groups
        .iter()
        .flat_map(|group| group.items.iter().map(|item| item.severity))
        .max()
        .unwrap_or(Severity::Info);
    let head = digest.groups.first().and_then(|group| group.items.first());
    let (target_name, target_id) = match devices {
        1 => (digest.groups[0].target_name.clone(), digest.groups[0].target_id),
        0 => ("DumbMonit".to_string(), None),
        n => (format!("{n} devices"), None),
    };

    let mut message = Message {
        title,
        text,
        markdown,
        severity,
        resolved: all_resolved,
        target_name,
        target_id,
        at: digest.at,
        rule_name: head.map(|item| item.rule_name.clone()).unwrap_or_default(),
        fingerprint: head.map(|item| item.fingerprint.clone()).unwrap_or_default(),
        value: head.and_then(|item| item.value),
        threshold: head.map(|item| item.threshold),
        unit: head.map(|item| item.unit.clone()).unwrap_or_default(),
        operator: head.map(|item| item.operator.clone()).unwrap_or_default(),
        count: digest.item_count() + digest.resolved_meanwhile.len(),
        link: None,
    };
    // Le lien global n'est ajouté que si aucun lien par équipement ne l'a été.
    if let Some(base) = public_url
        && devices != 1
    {
        message.link = Some(base.trim_end_matches('/').to_string());
    } else if let Some(base) = public_url {
        message.link =
            message.target_id.map(|id| format!("{}/targets/{id}", base.trim_end_matches('/')));
    }
    message
}

/// Message envoyé par le bouton « Envoyer un message de test ».
///
/// Il emprunte exactement le même chemin qu'une vraie alerte : c'est le seul moyen
/// de valider aussi bien le jeton que le format accepté par le service.
pub fn test_message(channel_name: &str) -> Message {
    let at = Utc::now();
    let title = format!("DumbMonit — test of channel \"{channel_name}\"");
    let text = "This message confirms the channel is configured correctly. \
                No alert is firing."
        .to_string();
    Message {
        markdown: format!("**{title}**\n{text}"),
        title,
        text,
        severity: Severity::Info,
        resolved: false,
        target_name: "DumbMonit".to_string(),
        target_id: None,
        at,
        rule_name: "Configuration test".to_string(),
        // Empreinte constante : un test répété ne doit pas ouvrir une astreinte de
        // plus à chaque clic sur le bouton.
        fingerprint: "dumbmonit-test".to_string(),
        value: None,
        threshold: None,
        unit: String::new(),
        operator: String::new(),
        count: 1,
        link: None,
    }
}

/// Message figé servant de référence aux tests de charge utile des canaux.
///
/// Partagé plutôt que recopié dans chaque module : les charges utiles attendues
/// sont comparées à des constantes, et elles ne resteraient pas comparables si
/// chaque canal se fabriquait son propre exemple.
#[cfg(test)]
pub fn sample_message(resolved: bool) -> Message {
    let title =
        if resolved { "Resolved — nas".to_string() } else { "nas — Disk full".to_string() };
    Message {
        markdown: format!("**{title}**\n• ⚠️ Disk full"),
        title,
        text: "⚠️ Disk full — 95 % (threshold > 90 %)".to_string(),
        severity: Severity::Warning,
        resolved,
        target_name: "nas".to_string(),
        target_id: Some(42),
        at: DateTime::from_timestamp(1_756_735_925, 0).expect("valid timestamp"),
        rule_name: "Disk full".to_string(),
        fingerprint: "nas/disk-full".to_string(),
        value: Some(95.0),
        threshold: Some(90.0),
        unit: "%".to_string(),
        operator: ">".to_string(),
        count: 1,
        link: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::alerting::model::Severity;

    fn at(seconds: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_700_000_000 + seconds, 0).expect("valid timestamp")
    }

    fn item(name: &str, reason: NotifyReason, severity: Severity) -> GroupItem {
        GroupItem {
            fingerprint: name.to_string(),
            rule_name: name.to_string(),
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

    fn group(items: Vec<GroupItem>) -> AlertGroup {
        AlertGroup {
            target_id: Some(1),
            target_name: "nas".to_string(),
            severity: Severity::Warning,
            items,
            channels: Vec::new(),
            at: at(3600),
        }
    }

    #[test]
    fn une_alerte_unique_donne_un_titre_explicite() {
        let message =
            render(&group(vec![item("Disk full", NotifyReason::Firing, Severity::Warning)]));
        assert_eq!(message.title, "nas — Disk full");
        assert!(message.text.contains("95 %"));
        assert!(message.text.contains("threshold > 90 %"));
        assert!(message.text.contains("for 1 h"));
    }

    #[test]
    fn la_ligne_porte_le_mot_de_severite_a_cote_du_pictogramme() {
        let text =
            |severity| render(&group(vec![item("Disk full", NotifyReason::Firing, severity)])).text;
        assert!(text(Severity::Critical).starts_with("🔴 Critical · Disk full"));
        assert!(text(Severity::Warning).starts_with("⚠️ Warning · Disk full"));
        assert!(text(Severity::Info).starts_with("ℹ️ Info · Disk full"));
        let resolved =
            render(&group(vec![item("Disk full", NotifyReason::Resolved, Severity::Warning)]));
        assert!(resolved.text.starts_with("✅ Resolved · Disk full"), "{}", resolved.text);
    }

    #[test]
    fn la_ligne_nomme_la_serie_derriere_l_alerte() {
        let mut vm = item("VM or container stopped", NotifyReason::Firing, Severity::Warning);
        vm.series_key = r#"dumbmonit_pve_guest_running{host="pve",name="win11-desktop",node="pve1",target="11",type="qemu",vmid="101"}"#.to_string();
        vm.value = Some(1.0);
        vm.threshold = 0.0;
        vm.unit = String::new();
        let message = render(&group(vec![vm]));
        assert_eq!(
            message.text,
            "⚠️ Warning · VM or container stopped — win11-desktop (101), for 1 h"
        );

        let mut node = item("Proxmox node offline", NotifyReason::Firing, Severity::Critical);
        node.series_key =
            r#"dumbmonit_pve_node_online{host="pve",node="pve2",target="11"}"#.to_string();
        node.value = Some(1.0);
        node.threshold = 0.0;
        node.unit = String::new();
        assert!(render(&group(vec![node])).text.contains("Proxmox node offline — pve2,"));

        // Deux « Service down » sur le même équipement se distinguent par l'URL.
        let mut service = item("Service down", NotifyReason::Firing, Severity::Warning);
        service.series_key =
            r#"dumbmonit_probe_success{host="bad",probe="http",target="25",url="http://lab/x6"}"#
                .to_string();
        assert!(render(&group(vec![service])).text.contains("Service down — http://lab/x6 —"));

        // Une série sans autre étiquette que celles de l'équipement n'ajoute rien.
        let mut host = item("Device unreachable", NotifyReason::Firing, Severity::Critical);
        host.series_key = r#"dumbmonit_up{host="nas",tag_site="lab",target="1"}"#.to_string();
        assert!(
            render(&group(vec![host])).text.starts_with("🔴 Critical · Device unreachable — 95 %")
        );
    }

    #[test]
    fn l_identite_d_une_serie_prefere_les_etiquettes_parlantes() {
        assert_eq!(
            series_identity(r#"m{datastore="main",group="vm/101",host="pbs",target="12"}"#)
                .as_deref(),
            Some("vm/101 · main")
        );
        assert_eq!(
            series_identity(
                r#"m{host="nas",result="fail",target="13",task="Lab VMs",task_id="6"}"#
            )
            .as_deref(),
            Some("Lab VMs")
        );
        assert_eq!(
            series_identity(r#"m{host="nas",mountpoint="/data",target="1"}"#).as_deref(),
            Some("/data")
        );
        // Sans étiquette connue, les valeurs restantes servent, mais jamais plus de deux.
        assert_eq!(
            series_identity(r#"m{host="sw",index="5",target="4",zone="a"}"#).as_deref(),
            Some("5 · a")
        );
        assert_eq!(series_identity("dumbmonit_up"), None);
        assert_eq!(series_identity(r#"m{host="nas",target="1"}"#), None);
        // Une virgule dans une valeur ne coupe pas la clé.
        assert_eq!(
            series_identity(r#"m{host="sw",ifalias="Workshop, rack 2",ifname="g5",target="4"}"#)
                .as_deref(),
            Some("g5 · Workshop, rack 2")
        );
    }

    #[test]
    fn une_regle_tout_ou_rien_ne_repete_pas_sa_valeur() {
        let boolean = |value: f64, operator: &str, threshold: f64| {
            let mut it = item("UPS on battery", NotifyReason::Firing, Severity::Warning);
            it.value = Some(value);
            it.operator = operator.to_string();
            it.threshold = threshold;
            it.unit = String::new();
            render(&group(vec![it])).text
        };
        assert_eq!(boolean(1.0, ">", 0.0), "⚠️ Warning · UPS on battery, for 1 h");
        assert_eq!(boolean(1.0, ">=", 1.0), "⚠️ Warning · UPS on battery, for 1 h");
        assert_eq!(boolean(0.0, "<", 1.0), "⚠️ Warning · UPS on battery, for 1 h");
        // Un état codé sur plusieurs valeurs reste chiffré : 5 n'est pas un booléen.
        assert!(boolean(5.0, ">", 0.0).contains("— 5 (threshold > 0)"));
        assert!(boolean(3.0, ">=", 3.0).contains("— 3 (threshold >= 3)"));
        // Un pourcentage à 1 % sous un seuil de 0 % n'est pas non plus un booléen.
        let mut pct = item("Disk full", NotifyReason::Firing, Severity::Warning);
        pct.value = Some(1.0);
        pct.threshold = 0.0;
        assert!(render(&group(vec![pct])).text.contains("— 1 % (threshold > 0 %)"));
    }

    #[test]
    fn plusieurs_alertes_donnent_un_titre_compte() {
        let message = render(&group(vec![
            item("Disk full", NotifyReason::Firing, Severity::Warning),
            item("CPU", NotifyReason::Firing, Severity::Critical),
        ]));
        assert_eq!(message.title, "nas — 2 alerts");
        assert_eq!(message.text.lines().count(), 2);
    }

    #[test]
    fn un_groupe_entierement_resolu_le_dit_dans_son_titre() {
        let message =
            render(&group(vec![item("Disk full", NotifyReason::Resolved, Severity::Warning)]));
        assert_eq!(message.title, "Resolved — nas");
        assert!(message.resolved);
        assert_eq!(message.color(), 0x2E_A0_43);
        assert!(!message.text.contains("threshold"), "pointless on a resolution");
    }

    #[test]
    fn une_escalade_est_annoncee_dans_la_ligne() {
        let message =
            render(&group(vec![item("CPU", NotifyReason::Escalation, Severity::Critical)]));
        assert!(message.text.contains("escalation"));
    }

    #[test]
    fn le_score_d_anomalie_apparait_quand_il_existe() {
        let mut anomalie = item("Unusual traffic", NotifyReason::Firing, Severity::Info);
        anomalie.score = Some(7.28);
        let message = render(&group(vec![anomalie]));
        assert!(message.text.contains("7.3 σ"), "{}", message.text);
    }

    #[test]
    fn les_valeurs_sont_lisibles() {
        assert_eq!(format_value(95.0, "%"), "95 %");
        assert_eq!(format_value(95.46, "%"), "95.5 %");
        assert_eq!(format_value(1234.56, ""), "1235");
        assert_eq!(format_value(f64::NAN, "%"), "n/a");
    }

    #[test]
    fn les_durees_sont_lisibles() {
        assert_eq!(format_duration(TimeDelta::seconds(45)), "45 s");
        assert_eq!(format_duration(TimeDelta::seconds(90)), "1 min");
        assert_eq!(format_duration(TimeDelta::hours(5)), "5 h");
        assert_eq!(format_duration(TimeDelta::days(3)), "3 d");
        assert_eq!(format_duration(TimeDelta::seconds(-10)), "0 s", "clock went backwards");
    }

    #[test]
    fn la_priorite_suit_la_severite() {
        let mut message = render(&group(vec![item("x", NotifyReason::Firing, Severity::Critical)]));
        message.severity = Severity::Critical;
        assert_eq!(message.priority(), 5);
        message.severity = Severity::Info;
        assert_eq!(message.priority(), 2);
        message.resolved = true;
        assert_eq!(message.priority(), 2);
    }

    #[test]
    fn la_table_de_substitution_couvre_exactement_les_variables_annoncees() {
        let message =
            render(&group(vec![item("Disk full", NotifyReason::Firing, Severity::Warning)]));
        let variables = message.variables();
        for (nom, description) in TEMPLATE_VARIABLES {
            assert!(variables.contains_key(nom), "\"{nom}\" is documented but missing");
            assert!(!description.is_empty(), "\"{nom}\" has no description");
        }
        for nom in variables.keys() {
            let documentee = TEMPLATE_VARIABLES.iter().any(|(known, _)| known == nom);
            assert!(documentee, "\"{nom}\" exists but is not documented");
        }
    }

    #[test]
    fn les_variables_decrivent_l_alerte_en_tete_de_groupe() {
        let message = render(&group(vec![
            item("Disk full", NotifyReason::Firing, Severity::Warning),
            item("CPU", NotifyReason::Firing, Severity::Critical),
        ]));
        let vars = message.variables();
        assert_eq!(vars["rule"], "Disk full");
        assert_eq!(vars["target"], "nas");
        assert_eq!(vars["count"], "2");
        assert_eq!(vars["status"], "firing");
        assert_eq!(vars["status_fr"], "en cours");
        assert_eq!(vars["severity"], "warning");
        assert_eq!(vars["severity_fr"], "avertissement");
        assert_eq!(vars["value"], "95 %");
        assert_eq!(vars["value_raw"], "95", "usable as is as a JSON number");
        assert_eq!(vars["threshold_raw"], "90");
        assert_eq!(vars["unit"], "%");
        assert_eq!(vars["operator"], ">");
        assert_eq!(vars["color_hex"], "#D98A00");
        assert_eq!(vars["emoji"], "⚠️");
        assert_eq!(vars["source"], "dumbmonit");
        // Complétés par le canal, qui seul connaît son URL publique et son secret.
        assert_eq!(vars["link"], "");
        assert_eq!(vars["token"], "");
    }

    #[test]
    fn une_alerte_sans_valeur_laisse_les_variables_chiffrees_vides() {
        // Vide plutôt que « null » ou « NaN » : un gabarit JSON reste valide, et un
        // SMS ne part pas avec « valeur : NaN ».
        let mut sans_valeur = item("Host unreachable", NotifyReason::Firing, Severity::Critical);
        sans_valeur.value = None;
        let vars = render(&group(vec![sans_valeur])).variables();
        assert_eq!(vars["value"], "");
        assert_eq!(vars["value_raw"], "");
    }

    #[test]
    fn une_resolution_le_dit_dans_ses_variables() {
        let vars =
            render(&group(vec![item("Disk full", NotifyReason::Resolved, Severity::Warning)]))
                .variables();
        assert_eq!(vars["status"], "resolved");
        assert_eq!(vars["status_fr"], "résolu");
        assert_eq!(vars["emoji"], "✅");
    }

    #[test]
    fn le_message_de_test_ne_pretend_pas_etre_une_alerte() {
        let message = test_message("Home Discord");
        assert!(message.title.contains("Home Discord"));
        assert!(message.text.contains("No alert"));
        assert_eq!(message.severity, Severity::Info);
    }

    fn digest(groups: Vec<AlertGroup>) -> Digest {
        Digest { groups, resolved_meanwhile: Vec::new(), hold: None, at: at(300) }
    }

    fn named_group(target: i64, name: &str, items: Vec<GroupItem>) -> AlertGroup {
        let mut group = group(items);
        group.target_id = Some(target);
        group.target_name = name.to_string();
        group
    }

    #[test]
    fn un_resume_d_un_seul_equipement_garde_le_format_habituel() {
        let d = digest(vec![named_group(
            1,
            "nas",
            vec![item("Disk full", NotifyReason::Firing, Severity::Warning)],
        )]);
        let message = render_digest(&d, Some("https://monit.lan/"));
        assert_eq!(message.title, "nas — Disk full");
        assert_eq!(message.link.as_deref(), Some("https://monit.lan/targets/1"));
        assert!(message.text.ends_with("https://monit.lan/targets/1"), "{}", message.text);
    }

    #[test]
    fn un_resume_multi_equipements_compte_et_sectionne() {
        let d = digest(vec![
            named_group(
                1,
                "nas",
                vec![
                    item("Disk full", NotifyReason::Firing, Severity::Warning),
                    item("Backup old", NotifyReason::Resolved, Severity::Warning),
                ],
            ),
            named_group(2, "router", vec![item("CPU", NotifyReason::Firing, Severity::Critical)]),
        ]);
        let message = render_digest(&d, Some("https://monit.lan"));
        assert_eq!(message.title, "2 alerts, 1 resolved on 2 devices");
        assert!(message.text.contains("nas:\n"), "{}", message.text);
        assert!(message.text.contains("router:\n"), "{}", message.text);
        assert!(message.text.contains("https://monit.lan/targets/2"));
        assert_eq!(message.count, 3);
        assert!(!message.resolved);
        assert_eq!(message.severity, Severity::Critical);
        assert_eq!(message.target_name, "2 devices");
    }

    #[test]
    fn un_resume_trop_long_est_tronque_avec_un_compte() {
        let items: Vec<GroupItem> = (0..DIGEST_MAX_LINES + 3)
            .map(|i| item(&format!("rule {i:02}"), NotifyReason::Firing, Severity::Warning))
            .collect();
        let d = digest(vec![named_group(1, "nas", items), named_group(2, "b", vec![])]);
        let message = render_digest(&d, None);
        assert!(message.text.contains("…and 3 more alerts"), "{}", message.text);
    }

    #[test]
    fn la_fin_des_heures_calmes_mentionne_ce_qui_s_est_resolu_entre_temps() {
        let mut d = digest(vec![named_group(
            1,
            "nas",
            vec![item("Disk full", NotifyReason::Firing, Severity::Warning)],
        )]);
        d.hold = Some(Hold::Quiet);
        d.resolved_meanwhile =
            vec![("router".to_string(), item("CPU", NotifyReason::Resolved, Severity::Warning))];
        let message = render_digest(&d, None);
        assert!(
            message.title.starts_with("Quiet hours over — 1 alert, 1 resolved on nas"),
            "{}",
            message.title
        );
        assert!(
            message.text.contains("Resolved during quiet hours: CPU (router)"),
            "{}",
            message.text
        );
    }

    #[test]
    fn une_ligne_de_battement_porte_sa_note() {
        let mut flapping = item("Service", NotifyReason::Flapping, Severity::Warning);
        flapping.note = Some("flapping: 4 changes in 30 min".to_string());
        let message = render(&group(vec![flapping]));
        assert!(message.text.starts_with("🔁 Flapping · Service"), "{}", message.text);
        assert!(message.text.contains("— flapping: 4 changes in 30 min"));
    }
}
