//! Catalogue des types de canaux : ce que l'interface doit demander pour chacun.
//!
//! L'interface ne connaît aucun service : elle rend les champs annoncés ici, et
//! c'est tout. Chaque entrée est relevée sur le constructeur du notificateur
//! correspondant — les clés lues dans `settings` et `secrets`, ce qui est
//! obligatoire, les valeurs admises, les défauts — et les tests du module
//! vérifient que le formulaire ainsi décrit suffit réellement à construire le
//! canal. Ajouter une clé dans un notificateur sans la déclarer ici la rend
//! inaccessible depuis l'interface : c'est ce que ces tests rappellent.
//!
//! Le contrat de [`Field`] et [`KindInfo`] est partagé avec l'interface
//! (`web/src/lib/api/types.ts`) : les noms de champs n'y changent pas.

use std::collections::HashMap;
use std::sync::OnceLock;

use serde::Serialize;

use super::CHANNEL_KINDS;

/// Un champ de formulaire.
#[derive(Debug, Clone, Serialize)]
pub struct Field {
    /// Clé dans `settings` ou `secrets`.
    pub key: &'static str,
    pub label: &'static str,
    pub required: bool,
    /// `text`, `url`, `number`, `boolean`, `select`, `textarea`, `password`.
    pub input: &'static str,
    pub help: &'static str,
    pub placeholder: &'static str,
    /// Valeurs proposées quand `input == "select"`.
    pub options: &'static [&'static str],
    /// Forme de la valeur attendue par le serveur : `scalar` (une chaîne, un nombre
    /// ou un booléen selon `input`), `list` (un tableau JSON de chaînes, une par
    /// ligne dans la zone de saisie) ou `object` (un objet JSON, saisi tel quel).
    ///
    /// Le notificateur ne lit une liste que sous forme de tableau : une chaîne
    /// « a, b » déposée dans `to` passerait pour « aucun destinataire ». C'est donc
    /// à l'interface de découper, et elle a besoin de savoir où.
    pub shape: &'static str,
    /// Valeur appliquée par le serveur quand le champ est laissé vide, vide s'il
    /// n'y en a pas. Sert à préremplir les listes de choix et à documenter le reste.
    pub default: &'static str,
}

/// Un type de canal, prêt à être présenté.
#[derive(Debug, Clone, Serialize)]
pub struct KindInfo {
    pub kind: &'static str,
    pub label: &'static str,
    pub summary: &'static str,
    /// Ancre dans `docs/notifications.md`, servie par l'interface.
    pub doc_url: &'static str,
    pub settings: Vec<Field>,
    pub secrets: Vec<Field>,
}

/// Tous les types de canaux, dans l'ordre de [`CHANNEL_KINDS`] — celui du menu.
pub fn all() -> Vec<KindInfo> {
    CHANNEL_KINDS.iter().filter_map(|kind| describe(kind)).collect()
}

/// Clés que le catalogue range parmi les secrets d'un type de canal.
///
/// C'est ce que l'API consulte pour refuser qu'un secret soit déposé dans
/// `settings` : la liste vient du catalogue lui-même, si bien qu'un secret annoncé
/// à l'interface ne peut pas échapper au refus. Le catalogue est figé au premier
/// appel, ce qui donne des tranches statiques à un coût nul ensuite.
pub fn secret_keys(kind: &str) -> &'static [&'static str] {
    static BY_KIND: OnceLock<HashMap<&'static str, Vec<&'static str>>> = OnceLock::new();
    BY_KIND
        .get_or_init(|| {
            all()
                .into_iter()
                .map(|info| (info.kind, info.secrets.iter().map(|field| field.key).collect()))
                .collect()
        })
        .get(kind)
        .map_or(&[], Vec::as_slice)
}

// --------------------------------------------------------------------------
// Construction des champs
// --------------------------------------------------------------------------

/// Champ facultatif, scalaire, sans liste de choix : le cas général.
fn field(
    key: &'static str,
    label: &'static str,
    input: &'static str,
    help: &'static str,
    placeholder: &'static str,
) -> Field {
    Field {
        key,
        label,
        required: false,
        input,
        help,
        placeholder,
        options: &[],
        shape: "scalar",
        default: "",
    }
}

fn text(key: &'static str, label: &'static str, help: &'static str, ph: &'static str) -> Field {
    field(key, label, "text", help, ph)
}

fn url(key: &'static str, label: &'static str, help: &'static str, ph: &'static str) -> Field {
    field(key, label, "url", help, ph)
}

fn password(key: &'static str, label: &'static str, help: &'static str, ph: &'static str) -> Field {
    field(key, label, "password", help, ph)
}

fn number(key: &'static str, label: &'static str, help: &'static str, ph: &'static str) -> Field {
    field(key, label, "number", help, ph)
}

fn textarea(key: &'static str, label: &'static str, help: &'static str, ph: &'static str) -> Field {
    field(key, label, "textarea", help, ph)
}

/// Liste de choix fermée ; le premier choix est toujours le défaut du serveur.
fn select(
    key: &'static str,
    label: &'static str,
    help: &'static str,
    options: &'static [&'static str],
) -> Field {
    Field { options, default: options[0], ..field(key, label, "select", help, "") }
}

impl Field {
    fn required(self) -> Self {
        Self { required: true, ..self }
    }

    fn with_default(self, default: &'static str) -> Self {
        Self { default, ..self }
    }

    /// Une valeur par ligne, transmise comme tableau JSON.
    fn list(self) -> Self {
        Self { shape: "list", input: "textarea", ..self }
    }

    /// Un objet JSON saisi tel quel.
    fn object(self) -> Self {
        Self { shape: "object", input: "textarea", ..self }
    }
}

/// Adresse de serveur, obligatoire : le champ le plus fréquent du catalogue.
fn server_url(help: &'static str, placeholder: &'static str) -> Field {
    url("server_url", "Server address", help, placeholder).required()
}

/// URL de webhook entrant, toujours secrète : elle contient le jeton.
fn webhook_url(help: &'static str, placeholder: &'static str) -> Field {
    password("webhook_url", "Webhook URL", help, placeholder).required()
}

fn info(
    kind: &'static str,
    label: &'static str,
    summary: &'static str,
    doc_url: &'static str,
    settings: Vec<Field>,
    secrets: Vec<Field>,
) -> KindInfo {
    KindInfo { kind, label, summary, doc_url, settings, secrets }
}

// --------------------------------------------------------------------------
// Le catalogue
// --------------------------------------------------------------------------

/// Décrit un type de canal, `None` s'il n'est pas catalogué.
///
/// Chaque bras reflète le constructeur du notificateur du même nom ; le test
/// `chaque_type_de_canal_est_catalogue` garantit qu'aucun bras ne manque.
fn describe(kind: &'static str) -> Option<KindInfo> {
    Some(match kind {
        // --- Messageries grand public et autohébergées ------------------------
        "discord" => info(
            kind,
            "Discord",
            "Rich message in a Discord channel, through a webhook.",
            "/docs/notifications#discord",
            vec![],
            vec![webhook_url(
                "Channel settings → Integrations → Webhooks → Copy webhook URL.",
                "https://discord.com/api/webhooks/123456789/AbCdEf…",
            )],
        ),
        "slack" => info(
            kind,
            "Slack",
            "Message in a Slack channel, through an incoming webhook.",
            "/docs/notifications#slack",
            vec![],
            vec![webhook_url(
                "URL provided by \"Incoming Webhooks\" in your Slack app.",
                "https://hooks.slack.com/services/T000/B000/XXXX",
            )],
        ),
        "telegram" => info(
            kind,
            "Telegram",
            "Message from a Telegram bot to you, in a group or in a channel.",
            "/docs/notifications#telegram",
            vec![
                text(
                    "chat_id",
                    "Chat ID",
                    "A number. Positive for a private chat with the bot; negative and \
                     starting with -100 for a group or a channel. The documentation explains \
                     how to find it in three cases.",
                    "-1001234567890",
                )
                .required(),
                text(
                    "message_thread_id",
                    "Group topic (optional)",
                    "Only for a group in \"Topics\" mode: the topic number, visible in the \
                     link of one of its messages (https://t.me/c/…/TOPIC/…).",
                    "11",
                ),
                url(
                    "api_base",
                    "API address",
                    "Change it only to go through a relay of the Telegram API.",
                    "https://api.telegram.org",
                )
                .with_default("https://api.telegram.org"),
            ],
            vec![
                password(
                    "bot_token",
                    "Bot token",
                    "Given by @BotFather after /newbot, of the form 123456789:AAE…",
                    "123456789:AAExampleTokenFromBotFather",
                )
                .required(),
            ],
        ),
        "teams" => info(
            kind,
            "Microsoft Teams",
            "Adaptive card in a Teams channel, through a Workflows flow.",
            "/docs/notifications#microsoft-teams",
            vec![],
            vec![webhook_url(
                "HTTPS POST URL of the \"Post to a channel when a webhook request is \
                 received\" flow. The old Office 365 connectors no longer work.",
                "https://prod-00.westeurope.logic.azure.com:443/workflows/…",
            )],
        ),
        "matrix" => info(
            kind,
            "Matrix",
            "Message in a Matrix room, from an account dedicated to DumbMonit.",
            "/docs/notifications#matrix",
            vec![
                server_url("Homeserver of the DumbMonit account.", "https://matrix.org"),
                text(
                    "room_id",
                    "Room ID",
                    "The internal identifier, which starts with \"!\" — not the display name. \
                     The account must be a member of the room.",
                    "!aBcDeF:matrix.org",
                )
                .required(),
            ],
            vec![
                password(
                    "token",
                    "Access token",
                    "In Element: Settings → Help & About → Advanced → Access token.",
                    "syt_ZXp5bW9uaXQ_…",
                )
                .required(),
            ],
        ),
        "mattermost" => info(
            kind,
            "Mattermost",
            "Message in a Mattermost channel, through an incoming webhook.",
            "/docs/notifications#mattermost",
            vec![
                text(
                    "channel",
                    "Channel",
                    "To write somewhere other than the webhook's default channel.",
                    "alerts",
                ),
                text("username", "Display name", "Name under which the message appears.", "")
                    .with_default("DumbMonit"),
            ],
            vec![webhook_url(
                "Integrations → Incoming Webhooks → Add, then copy the URL.",
                "https://mattermost.example.org/hooks/abcdefghijklmnop",
            )],
        ),
        "rocketchat" => info(
            kind,
            "Rocket.Chat",
            "Message in a Rocket.Chat room, through an incoming webhook.",
            "/docs/notifications#rocketchat",
            vec![
                text(
                    "channel",
                    "Room",
                    "To write somewhere other than the room chosen when the webhook was created.",
                    "#alerts",
                ),
                text("alias", "Display name", "Name under which the message appears.", "")
                    .with_default("DumbMonit"),
            ],
            vec![webhook_url(
                "Administration → Integrations → Incoming webhook, then copy the webhook URL.",
                "https://rocket.example.org/hooks/abcdef/ghijkl",
            )],
        ),
        "googlechat" => info(
            kind,
            "Google Chat",
            "Card in a Google Chat space, through a webhook.",
            "/docs/notifications#google-chat",
            vec![],
            vec![webhook_url(
                "Space → Apps & integrations → Webhooks. The URL already contains the key \
                 and the token.",
                "https://chat.googleapis.com/v1/spaces/AAAA/messages?key=…&token=…",
            )],
        ),
        "zulip" => info(
            kind,
            "Zulip",
            "Message from a Zulip bot in a stream, under a dedicated topic.",
            "/docs/notifications#zulip",
            vec![
                server_url("Address of your Zulip organization.", "https://your-org.zulipchat.com"),
                text(
                    "email",
                    "Bot address",
                    "The email address assigned to the bot when it was created.",
                    "ezymonit-bot@your-org.zulipchat.com",
                )
                .required(),
                text(
                    "stream",
                    "Stream",
                    "Name of the target stream; the bot must be subscribed to it.",
                    "monitoring",
                )
                .required(),
                text("topic", "Topic", "Topic under which messages are grouped.", "")
                    .with_default("DumbMonit"),
            ],
            vec![
                password(
                    "api_key",
                    "Bot API key",
                    "Personal settings → Bots, next to the bot.",
                    "aBcDeFgHiJkLmNoPqRsTuVwXyZ012345",
                )
                .required(),
            ],
        ),

        // --- Notifications directes ----------------------------------------
        "ntfy" => info(
            kind,
            "ntfy",
            "Notification on an ntfy topic, on the public instance or your own.",
            "/docs/notifications#ntfy",
            vec![
                text(
                    "topic",
                    "Topic",
                    "On ntfy.sh, anyone who knows this name can read the topic: pick a long, \
                     random one.",
                    "ezymonit-k7x2p9qv",
                )
                .required(),
                url("server_url", "Server address", "Your own instance, if you host one.", "")
                    .with_default("https://ntfy.sh"),
            ],
            vec![password(
                "token",
                "Access token",
                "Only if the server requires authentication.",
                "tk_abcdefghijklmnopqrstuvwxyz",
            )],
        ),
        "gotify" => info(
            kind,
            "Gotify",
            "Notification on your self-hosted Gotify server.",
            "/docs/notifications#gotify",
            vec![server_url("Address of your Gotify server.", "https://gotify.home")],
            vec![
                password(
                    "token",
                    "Application token",
                    "Apps → Create Application, then copy the token shown.",
                    "AbCdEfGhIjKlMnO",
                )
                .required(),
            ],
        ),
        "pushover" => info(
            kind,
            "Pushover",
            "Pushover notification on your devices, with priority following severity.",
            "/docs/notifications#pushover",
            vec![
                number(
                    "priority",
                    "Forced priority",
                    "From -2 to 2. Empty, the priority follows severity: -1, 0 or 1. Priority 2 \
                     repeats until acknowledged.",
                    "0",
                ),
                text("sound", "Sound", "Name of a Pushover sound, for example \"siren\".", ""),
                number(
                    "retry",
                    "Retry (seconds)",
                    "Interval between two repeats of a priority 2; from 30 to 10,800.",
                    "60",
                )
                .with_default("60"),
                number(
                    "expire",
                    "Expiration (seconds)",
                    "How long a priority 2 keeps being repeated; from 30 to 10,800.",
                    "3600",
                )
                .with_default("3600"),
            ],
            vec![
                password(
                    "token",
                    "Application token",
                    "Create an Application/API Token, on pushover.net.",
                    "azGDORePK8gMaC0QOYAMyEEuzJnyUi",
                )
                .required(),
                password(
                    "user_key",
                    "User key",
                    "Shown on the pushover.net home page.",
                    "uQiRzpo4DXghDmr9QzzfQu27cmVRsG",
                )
                .required(),
            ],
        ),
        "pushbullet" => info(
            kind,
            "Pushbullet",
            "Pushbullet notification on all your devices, or on a single one.",
            "/docs/notifications#pushbullet",
            vec![text(
                "device_iden",
                "Device identifier",
                "Empty, all your devices are notified.",
                "ujpah72o0sjAoRtnM0jc",
            )],
            vec![
                password(
                    "token",
                    "Access token",
                    "Settings → Account → Create Access Token.",
                    "o.AbCdEfGhIjKlMnOpQrStUvWxYz",
                )
                .required(),
            ],
        ),
        "bark" => info(
            kind,
            "Bark (iOS)",
            "Notification on iPhone through Bark, with interruption level following severity.",
            "/docs/notifications#bark-ios",
            vec![
                url("server_url", "Server address", "Your own Bark instance, if you host one.", "")
                    .with_default("https://api.day.app"),
                text("group", "Group", "Group under which iOS files the notifications.", "")
                    .with_default("DumbMonit"),
                select(
                    "level",
                    "Forced interruption level",
                    "Empty, the level follows severity: passive, active or timeSensitive. \
                     \"critical\" breaks through Do Not Disturb.",
                    &["", "passive", "active", "timeSensitive", "critical"],
                ),
                text("sound", "Sound", "Name of a Bark sound, for example \"alarm\".", ""),
            ],
            vec![
                password(
                    "token",
                    "Device key",
                    "The string of characters after the domain name in the URL shown by the \
                     app.",
                    "AbCdEfGhIjKlMnOpQrStUv",
                )
                .required(),
            ],
        ),

        // --- Passerelles, domotique et messagerie ---------------------------
        "apprise" => info(
            kind,
            "Apprise",
            "Gateway to dozens of services, through an Apprise instance.",
            "/docs/notifications#apprise",
            vec![
                server_url("Address of your Apprise instance.", "http://apprise.home:8000"),
                text(
                    "config_key",
                    "Configuration key",
                    "Recommended mode: key of a configuration stored in Apprise. Required if \
                     no destination URL is provided.",
                    "home",
                ),
                text("tag", "Tag", "To target only part of the configuration's destinations.", ""),
                select(
                    "format",
                    "Message format",
                    "Plain text suits every destination.",
                    &["text", "markdown"],
                ),
            ],
            vec![textarea(
                "urls",
                "Destination URLs",
                "Direct mode: one or more Apprise URLs separated by commas. Secret, because \
                 they often contain a password. Required if no configuration key is \
                 provided.",
                "mailto://user:password@example.org",
            )],
        ),
        "homeassistant" => info(
            kind,
            "Home Assistant",
            "Call to a Home Assistant service: a notification, or any other action.",
            "/docs/notifications#home-assistant",
            vec![
                server_url("Address of Home Assistant.", "http://homeassistant.local:8123"),
                text(
                    "service",
                    "Service to call",
                    "In \"domain.service\" form, for example notify.mobile_app_phone.",
                    "",
                )
                .with_default("persistent_notification.create"),
                text(
                    "target",
                    "Target",
                    "Passed as-is in \"target\", for services that expect one.",
                    "",
                ),
                textarea(
                    "data",
                    "Extra data",
                    "JSON object merged last into the call, for example an entity and a colour \
                     for light.turn_on.",
                    "{\"entity_id\": \"light.living_room\"}",
                )
                .object(),
            ],
            vec![
                password(
                    "token",
                    "Long-lived access token",
                    "Profile → Security → Long-lived access tokens → Create token.",
                    "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9…",
                )
                .required(),
            ],
        ),
        "smtp" => info(
            kind,
            "Email (SMTP)",
            "Email sent by your own SMTP server, with no third-party service.",
            "/docs/notifications#email-smtp",
            vec![
                text("host", "SMTP server", "Host name or IP address.", "smtp.example.org")
                    .required(),
                select(
                    "security",
                    "Encryption",
                    "STARTTLS on 587, TLS from the start of the connection on 465, none \
                     (local relay only) on 25.",
                    &["starttls", "tls", "none"],
                ),
                number("port", "Port", "Empty, derived from encryption: 587, 465 or 25.", "587"),
                text("from", "Sender", "Sending address.", "ezymonit@example.org").required(),
                textarea("to", "Recipients", "One address per line.", "admin@example.org")
                    .required()
                    .list(),
                text(
                    "username",
                    "Username",
                    "For a server that requires authentication. Requires the password.",
                    "",
                ),
            ],
            vec![password(
                "password",
                "Password",
                "With Gmail, an app password, not the account password.",
                "",
            )],
        ),
        "signal" => info(
            kind,
            "Signal",
            "Signal message sent by your signal-cli-rest-api instance.",
            "/docs/notifications#signal",
            vec![
                server_url("Address of your signal-cli-rest-api.", "http://signal.home:8080"),
                text(
                    "number",
                    "Sender number",
                    "The number registered in signal-cli, in international format.",
                    "+33612345678",
                )
                .required(),
                textarea(
                    "recipients",
                    "Recipients",
                    "One number in international format or one group identifier per line.",
                    "+33698765432",
                )
                .required()
                .list(),
                text(
                    "username",
                    "Username",
                    "If the instance is protected by HTTP authentication. Requires the \
                     password.",
                    "",
                ),
            ],
            vec![password(
                "password",
                "Password",
                "Password of the instance's HTTP authentication.",
                "",
            )],
        ),
        "twilio" => info(
            kind,
            "SMS via Twilio",
            "SMS sent by Twilio, trimmed to the essentials to fit in one segment.",
            "/docs/notifications#sms-via-twilio",
            vec![
                text(
                    "account_sid",
                    "Account SID",
                    "Shown on the Twilio console home page.",
                    "ACxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx",
                )
                .required(),
                text(
                    "from",
                    "Sender number",
                    "Your Twilio number. Required if no messaging service is provided.",
                    "+15005550006",
                ),
                text(
                    "messaging_service_sid",
                    "Messaging service",
                    "Required if no sender number is provided; Twilio then picks the number.",
                    "MGxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx",
                ),
                textarea(
                    "to",
                    "Recipients",
                    "One number in international format per line.",
                    "+33612345678",
                )
                .required()
                .list(),
            ],
            vec![
                password(
                    "token",
                    "Auth Token",
                    "Shown next to the Account SID in the console.",
                    "0123456789abcdef0123456789abcdef",
                )
                .required(),
            ],
        ),

        // --- Astreinte -----------------------------------------------------
        "pagerduty" => info(
            kind,
            "PagerDuty",
            "PagerDuty incident opened by the alert, closed by its resolution.",
            "/docs/notifications#pagerduty",
            vec![
                select(
                    "region",
                    "Account region",
                    "\"eu\" if your account is hosted in Europe.",
                    &["us", "eu"],
                ),
                text("source", "Source", "Source name shown in the incident.", "")
                    .with_default("ezymonit"),
            ],
            vec![
                password(
                    "token",
                    "Integration Key",
                    "Services → Integrations → Add integration, of type Events API v2.",
                    "0123456789abcdef0123456789abcdef",
                )
                .required(),
            ],
        ),
        "opsgenie" => info(
            kind,
            "Opsgenie",
            "Opsgenie alert opened by the alert, closed by its resolution.",
            "/docs/notifications#opsgenie",
            vec![
                select("region", "Account region", "\"eu\" for a European account.", &["us", "eu"]),
                textarea("responders", "Teams to notify", "One team name per line.", "on-call")
                    .list(),
                textarea("tags", "Tags", "One tag per line.", "homelab").list(),
            ],
            vec![
                password(
                    "api_key",
                    "API key",
                    "Teams → Integrations → Add integration → API.",
                    "01234567-89ab-cdef-0123-456789abcdef",
                )
                .required(),
            ],
        ),

        // --- Sur mesure ----------------------------------------------------
        "webhook" => info(
            kind,
            "Custom webhook",
            "HTTP request fully described by you: method, headers, templated body.",
            "/docs/notifications#custom-webhook",
            vec![
                select("method", "Method", "A GET request has no body.", &["POST", "PUT", "GET"]),
                select(
                    "content_type",
                    "Content type",
                    "Also determines how template variables are escaped.",
                    &["json", "form", "text"],
                ),
                textarea(
                    "body_template",
                    "Body template",
                    "Variables between double braces, for example {{title}}. Empty, the \
                     standard JSON payload is sent.",
                    "{\"title\": \"{{title}}\", \"detail\": \"{{message}}\"}",
                ),
                textarea(
                    "headers",
                    "Extra headers",
                    "JSON object, one value per header.",
                    "{\"X-Application\": \"ezymonit\"}",
                )
                .object(),
                url(
                    "base_url",
                    "Public address of DumbMonit",
                    "So that {{link}} gives a clickable link to the device.",
                    "https://ezymonit.home",
                ),
                text(
                    "username",
                    "Username",
                    "For HTTP \"Basic\" authentication. Requires the password.",
                    "",
                ),
            ],
            vec![
                url(
                    "url",
                    "Address to call",
                    "May contain variables. Secret, because it often carries a token.",
                    "https://api.example.org/incidents",
                )
                .required(),
                password(
                    "token",
                    "Token",
                    "Sent as an \"Authorization: Bearer\" header, unless the template uses \
                     {{token}}.",
                    "",
                ),
                password("password", "Password", "Password for HTTP \"Basic\" authentication.", ""),
            ],
        ),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use serde_json::{Map, Value, json};

    use super::*;
    use crate::notify::channel::test_config;
    use crate::notify::{NotifyError, build, http_client};

    const DOC: &str = include_str!("../../../../docs/notifications.md");

    /// Ancre qu'un moteur Markdown classique attribue à un titre : minuscules,
    /// accents conservés, espaces en tirets, ponctuation supprimée.
    fn anchor(title: &str) -> String {
        title
            .trim()
            .to_lowercase()
            .chars()
            .filter(|c| c.is_alphanumeric() || *c == ' ' || *c == '-')
            .map(|c| if c == ' ' { '-' } else { c })
            .collect()
    }

    /// Ancres de tous les titres de troisième niveau de la documentation.
    fn doc_anchors() -> BTreeSet<String> {
        DOC.lines()
            .filter_map(|line| line.strip_prefix("### "))
            .map(|title| format!("#{}", anchor(title)))
            .collect()
    }

    /// Valeur plausible d'un champ, déduite de son exemple : c'est la manière la
    /// plus honnête d'éprouver le formulaire, puisque c'est ce que l'utilisateur
    /// recopiera.
    fn sample(field: &Field) -> Value {
        let text = if field.placeholder.is_empty() { field.default } else { field.placeholder };
        match (field.shape, field.input) {
            ("list", _) => json!([text]),
            ("object", _) => serde_json::from_str(text).expect("invalid JSON object example"),
            (_, "number") => json!(text.parse::<i64>().expect("invalid numeric example")),
            (_, "select") => json!(field.default),
            _ => json!(text),
        }
    }

    fn values(fields: &[Field], keep: impl Fn(&Field) -> bool) -> Value {
        Value::Object(
            fields.iter().filter(|f| keep(f)).map(|f| (f.key.to_string(), sample(f))).collect(),
        )
    }

    /// Réglages qu'un formulaire ne peut pas marquer obligatoires, parce que le
    /// notificateur accepte l'un ou l'autre de deux champs.
    fn either_or(kind: &str) -> Option<(&'static str, &'static str)> {
        match kind {
            "apprise" => Some(("config_key", "urls")),
            "twilio" => Some(("from", "messaging_service_sid")),
            _ => None,
        }
    }

    fn find<'a>(fields: &'a [Field], key: &str) -> &'a Field {
        fields.iter().find(|f| f.key == key).unwrap_or_else(|| panic!("field \"{key}\" missing"))
    }

    #[test]
    fn chaque_type_de_canal_est_catalogue() {
        let catalogue = all();
        let kinds: Vec<&str> = catalogue.iter().map(|k| k.kind).collect();
        assert_eq!(kinds, CHANNEL_KINDS, "the catalogue must follow CHANNEL_KINDS, in order");
        for kind in &catalogue {
            assert!(!kind.label.trim().is_empty(), "{}: empty label", kind.kind);
            assert!(!kind.summary.trim().is_empty(), "{}: empty summary", kind.kind);
            assert!(!kind.doc_url.is_empty(), "{}: empty documentation link", kind.kind);
            assert!(
                !kind.settings.is_empty() || !kind.secrets.is_empty(),
                "{}: no field",
                kind.kind
            );
        }
    }

    #[test]
    fn les_champs_sont_coherents() {
        for kind in all() {
            let mut keys = BTreeSet::new();
            for field in kind.settings.iter().chain(&kind.secrets) {
                let key = field.key;
                assert!(keys.insert(key), "{}: \"{key}\" declared twice", kind.kind);
                assert!(!field.label.trim().is_empty(), "{}: \"{key}\" has no label", kind.kind);
                assert!(
                    ["text", "url", "number", "boolean", "select", "textarea", "password"]
                        .contains(&field.input),
                    "{}: \"{key}\" has an unknown input type \"{}\"",
                    kind.kind,
                    field.input
                );
                assert!(
                    ["scalar", "list", "object"].contains(&field.shape),
                    "{}: \"{key}\" has an unknown shape \"{}\"",
                    kind.kind,
                    field.shape
                );
                if field.input == "select" {
                    assert!(!field.options.is_empty(), "{}: \"{key}\" has no options", kind.kind);
                    assert!(
                        field.options.contains(&field.default),
                        "{}: the default of \"{key}\" is not among its options",
                        kind.kind
                    );
                } else {
                    assert!(field.options.is_empty(), "{}: \"{key}\" is not a select", kind.kind);
                }
                if field.required {
                    // Le test de construction s'appuie sur l'exemple : il doit exister.
                    assert!(
                        !field.placeholder.is_empty(),
                        "{}: \"{key}\" is required but has no example",
                        kind.kind
                    );
                }
            }
        }
    }

    #[test]
    fn chaque_lien_de_documentation_pointe_vers_une_ancre_existante() {
        let anchors = doc_anchors();
        assert!(anchors.contains("#email-smtp"), "inconsistent anchor computation: {anchors:?}");
        assert!(anchors.contains("#bark-ios"), "inconsistent anchor computation: {anchors:?}");
        for kind in all() {
            let Some(fragment) = kind.doc_url.strip_prefix("/docs/notifications") else {
                panic!("{}: unexpected link \"{}\"", kind.kind, kind.doc_url)
            };
            assert!(
                anchors.contains(fragment),
                "{}: anchor \"{fragment}\" does not exist in docs/notifications.md",
                kind.kind
            );
        }
    }

    #[test]
    fn les_seuls_champs_obligatoires_suffisent_a_construire_le_canal() {
        let http = http_client();
        for kind in all() {
            let mut settings = values(&kind.settings, |f| f.required);
            let secrets = values(&kind.secrets, |f| f.required);
            if let Some((first, _)) = either_or(kind.kind) {
                let field = find(&kind.settings, first);
                settings[first] = sample(field);
            }
            let config = test_config(kind.kind, settings, secrets);
            if let Err(error) = build(&http, &config) {
                panic!("{}: the minimal form is not enough — {error}", kind.kind);
            }
        }
    }

    #[test]
    fn un_choix_entre_deux_champs_est_explique_quand_aucun_n_est_rempli() {
        // Ni « from » ni « messaging_service_sid », ni « config_key » ni « urls » ne
        // peuvent être marqués obligatoires ; en échange, le message d'erreur du
        // notificateur nomme les deux possibilités, et l'aide du formulaire aussi.
        let http = http_client();
        for kind in all() {
            let Some((first, second)) = either_or(kind.kind) else { continue };
            let settings = values(&kind.settings, |f| f.required);
            let secrets = values(&kind.secrets, |f| f.required);
            let Err(error) = build(&http, &test_config(kind.kind, settings, secrets)) else {
                panic!("{}: the alternative is no longer required, update the catalogue", kind.kind)
            };
            assert!(matches!(error, NotifyError::Config(_)), "{}: {error}", kind.kind);
            let message = error.to_string();
            assert!(
                message.contains(first) && message.contains(second),
                "{}: {message}",
                kind.kind
            );
            let all_fields: Vec<&Field> = kind.settings.iter().chain(&kind.secrets).collect();
            for key in [first, second] {
                let field =
                    all_fields.iter().find(|f| f.key == key).expect("field of the alternative");
                assert!(!field.required, "{}: \"{key}\" cannot be required", kind.kind);
                assert!(
                    field.help.contains("Required if"),
                    "{}: the help of \"{key}\" must say when it is required",
                    kind.kind
                );
            }
        }
    }

    #[test]
    fn tous_les_exemples_sont_acceptes_ensemble() {
        // Chaque exemple doit être une valeur valide : c'est ce que l'utilisateur
        // recopie en premier, et un exemple refusé serait pire qu'aucun exemple.
        let http = http_client();
        for kind in all() {
            let settings = values(&kind.settings, |_| true);
            let secrets = values(&kind.secrets, |_| true);
            let config = test_config(kind.kind, settings, secrets);
            if let Err(error) = build(&http, &config) {
                panic!("{}: an example is rejected — {error}", kind.kind);
            }
        }
    }

    #[test]
    fn les_secrets_par_type_sont_ceux_du_catalogue() {
        // C'est cette liste que l'API refuse dans « settings » : elle doit suivre le
        // catalogue à la lettre, et couvrir au moins ce qu'elle refusait déjà avant
        // d'en dépendre.
        for kind in all() {
            let expected: Vec<&str> = kind.secrets.iter().map(|f| f.key).collect();
            assert_eq!(secret_keys(kind.kind), expected.as_slice(), "{}", kind.kind);
        }
        assert!(secret_keys("webhook").contains(&"url"));
        assert!(secret_keys("pushover").contains(&"user_key"));
        assert!(secret_keys("apprise").contains(&"urls"));
        assert!(secret_keys("carrier-pigeon").is_empty());
    }

    #[test]
    fn le_catalogue_se_serialise_avec_le_contrat_de_l_interface() {
        let json = serde_json::to_value(all()).unwrap();
        let Value::Array(kinds) = json else { panic!("an array was expected") };
        let first = kinds.first().and_then(Value::as_object).expect("one object per kind");
        for key in ["kind", "label", "summary", "doc_url", "settings", "secrets"] {
            assert!(first.contains_key(key), "\"{key}\" missing from KindInfo");
        }
        let field: &Map<String, Value> =
            first["secrets"][0].as_object().expect("one object per field");
        for key in ["key", "label", "required", "input", "help", "placeholder", "options"] {
            assert!(field.contains_key(key), "\"{key}\" missing from Field");
        }
    }
}
