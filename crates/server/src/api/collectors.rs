//! Types d'équipements que l'on peut ajouter, et comment les préparer.
//!
//! L'interface découvre cette liste au lieu de la coder en dur : elle ne propose
//! donc jamais un type que ce serveur n'embarque pas, et récupère automatiquement
//! ceux qu'une version ultérieure ajoutera.
//!
//! Chaque type porte sa propre notice de mise en route. C'est ce qui permet à
//! l'interface d'afficher, à côté du formulaire, les étapes à faire sur
//! l'équipement lui-même — la moitié du travail d'ajout se passe là-bas, pas ici.
//!
//! Chaque type décrit aussi ses options : les réglages qu'un collecteur lit dans
//! les étiquettes de la cible (`Target::tags`). Sans cette liste, l'utilisateur
//! devrait deviner le nom exact d'une étiquette et sa syntaxe ; avec elle,
//! l'interface propose un champ par option, avec son défaut et son aide. La liste
//! est un miroir des `options.rs` de chaque collecteur : une étiquette absente
//! ici reste lue, mais personne ne saura qu'elle existe.

use axum::Json;
use axum::extract::State;
use serde::Serialize;

use crate::state::AppState;

#[derive(Serialize)]
pub struct CollectorView {
    /// Valeur à placer dans le champ `kind` d'une cible.
    pub kind: &'static str,
    /// Libellé destiné à l'affichage.
    pub label: &'static str,
    /// Une phrase disant à quoi sert ce type, pour la liste de choix.
    pub summary: &'static str,
    /// Exemples d'équipements concernés, pour que l'utilisateur se reconnaisse.
    pub examples: &'static [&'static str],
    /// Formes de `credential` acceptées (leurs `kind`), dans l'ordre de préférence.
    /// Redondant avec `credentials`, conservé pour les interfaces plus anciennes.
    pub credential_types: &'static [&'static str],
    /// Les mêmes formes, décrites champ par champ : c'est ce que le formulaire
    /// affiche. Un jeton Proxmox se saisit ainsi en deux cases (identifiant et
    /// secret) plutôt qu'en une chaîne `user@pve!nom=secret` que personne ne devine.
    pub credentials: &'static [CredentialView],
    /// Adresse d'exemple, utilisée comme texte indicatif du champ.
    pub address_hint: &'static str,
    /// Port par défaut, affiché pour lever le doute.
    pub default_port: u16,
    /// Notice de mise en route affichée à côté du formulaire.
    pub setup: Setup,
    /// Réglages lus dans les étiquettes de la cible, dans l'ordre d'affichage.
    /// Vide pour un type qui n'en lit aucune.
    pub options: &'static [OptionView],
}

#[derive(Serialize)]
pub struct Setup {
    /// Titre de la notice.
    pub title: &'static str,
    /// Étapes à effectuer sur l'équipement, dans l'ordre.
    ///
    /// Une étape est une phrase. Si elle est suivie d'un saut de ligne, ce qui
    /// suit est une commande ou une valeur à copier telle quelle : l'interface
    /// l'affiche dans un bloc avec un bouton « copier », et la documentation la
    /// reprend dans un bloc de code. Les mêmes étapes figurent dans
    /// `docs/devices/<kind>.md` ; un test vérifie qu'elles n'ont pas divergé.
    pub steps: &'static [&'static str],
    /// Point d'attention fréquent, à mettre en évidence. Vide s'il n'y en a pas.
    pub warning: &'static str,
    /// Lien vers la documentation du constructeur. Vide s'il n'y en a pas.
    pub doc_url: &'static str,
}

/// Un réglage porté par une étiquette de la cible.
///
/// C'est le contrat avec l'interface : elle construit un champ de formulaire par
/// entrée, et enregistre la valeur saisie sous la clé `key` dans `Target::tags`.
#[derive(Serialize)]
pub struct OptionView {
    /// Clé du tag dans `Target::tags`.
    pub key: &'static str,
    pub label: &'static str,
    pub help: &'static str,
    pub placeholder: &'static str,
    /// Valeur par défaut affichée (vide si aucune).
    pub default: &'static str,
    pub required: bool,
    /// `text`, `number`, `boolean`, `select`.
    pub input: &'static str,
    /// Valeurs proposées quand `input == "select"`.
    pub choices: &'static [&'static str],
}

/// Une forme d'identifiant acceptée par un type, et les champs à remplir.
///
/// C'est le contrat avec le formulaire : un champ par entrée, envoyé sous la clé
/// `key` dans l'objet `credential` (`{"type": kind, key: valeur, …}`). Le serveur
/// sait recomposer ce qu'il attend — un jeton Proxmox à partir de `token_id` et
/// `secret`, un SNMP v3 à partir de ses protocoles et phrases de passe.
#[derive(Serialize)]
pub struct CredentialView {
    /// Valeur du champ `type` de `credential`.
    pub kind: &'static str,
    pub label: &'static str,
    /// Une phrase pour situer cette forme par rapport aux autres. Vide si inutile.
    pub help: &'static str,
    pub fields: &'static [CredentialField],
}

#[derive(Serialize)]
pub struct CredentialField {
    /// Clé du champ dans l'objet `credential`.
    pub key: &'static str,
    pub label: &'static str,
    pub help: &'static str,
    pub placeholder: &'static str,
    /// `text`, `password` ou `select`.
    pub input: &'static str,
    /// Valeurs proposées quand `input == "select"`, la première par défaut.
    pub choices: &'static [&'static str],
    pub required: bool,
}

/// Champ visible (nom d'utilisateur, identifiant de jeton).
const fn cred_text(
    key: &'static str,
    label: &'static str,
    help: &'static str,
    placeholder: &'static str,
) -> CredentialField {
    CredentialField { key, label, help, placeholder, input: "text", choices: &[], required: true }
}

/// Champ masqué avec un bouton « afficher » : mot de passe, secret, phrase de passe.
const fn cred_secret(
    key: &'static str,
    label: &'static str,
    help: &'static str,
    placeholder: &'static str,
    required: bool,
) -> CredentialField {
    CredentialField { key, label, help, placeholder, input: "password", choices: &[], required }
}

const fn cred_select(
    key: &'static str,
    label: &'static str,
    help: &'static str,
    choices: &'static [&'static str],
) -> CredentialField {
    CredentialField { key, label, help, placeholder: "", input: "select", choices, required: false }
}

const NO_AUTH: CredentialView = CredentialView {
    kind: "none",
    label: "No authentication",
    help: "Nothing is sent: the device answers without credentials.",
    fields: &[],
};

const SNMP_COMMUNITY: CredentialView = CredentialView {
    kind: "snmp_community",
    label: "SNMP v1 / v2c (community)",
    help: "The simplest form: one shared word, sent unencrypted.",
    fields: &[cred_secret(
        "community",
        "SNMP community",
        "Most devices ship with \"public\". A read-only community is enough.",
        "public",
        true,
    )],
};

const SNMP_V3: CredentialView = CredentialView {
    kind: "snmp_v3",
    label: "SNMP v3 (user, authentication, privacy)",
    help: "Authenticated and encrypted. Needs a USM user on the device.",
    fields: &[
        cred_text("username", "User name", "The SNMP v3 user configured on the device.", "monitor"),
        cred_select(
            "auth_protocol",
            "Authentication protocol",
            "Must match what the device was configured with.",
            &["sha256", "sha512", "sha384", "sha224", "sha1", "md5"],
        ),
        cred_secret(
            "auth_passphrase",
            "Authentication passphrase",
            "At least 8 characters, as set on the device.",
            "",
            true,
        ),
        cred_select(
            "privacy_protocol",
            "Privacy protocol",
            "Encryption of the SNMP traffic. Ignored when the privacy passphrase is empty.",
            &["aes128", "aes192", "aes256", "des"],
        ),
        cred_secret(
            "privacy_passphrase",
            "Privacy passphrase",
            "Leave blank for authentication without encryption (authNoPriv).",
            "",
            false,
        ),
    ],
};

/// Identifiant d'un jeton Proxmox (VE ou PBS) : `user@realm!nom`, tel que le
/// produit l'affiche à la création.
const fn token_id_field(placeholder: &'static str) -> CredentialField {
    CredentialField {
        key: "token_id",
        label: "Token ID",
        help: "User, realm and token name, exactly as shown when the token was created.",
        placeholder,
        input: "text",
        choices: &[],
        required: true,
    }
}

/// Secret d'un jeton Proxmox : l'UUID montré une seule fois.
const TOKEN_SECRET_FIELD: CredentialField = cred_secret(
    "secret",
    "Secret",
    "The UUID shown once when the token was created. Stored encrypted, never shown again.",
    "xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx",
    true,
);

const PROXMOX_TOKEN_LABEL: &str = "API token (recommended)";
const PROXMOX_TOKEN_HELP: &str =
    "No expiry and no session opened: the right choice for monitoring.";

/// Jeton d'API Proxmox VE, en deux morceaux tels que le produit les affiche.
const PROXMOX_TOKEN: CredentialView = CredentialView {
    kind: "api_token",
    label: PROXMOX_TOKEN_LABEL,
    help: PROXMOX_TOKEN_HELP,
    fields: &[token_id_field("dumbmonit@pve!monitor"), TOKEN_SECRET_FIELD],
};

const PBS_TOKEN: CredentialView = CredentialView {
    kind: "api_token",
    label: PROXMOX_TOKEN_LABEL,
    help: PROXMOX_TOKEN_HELP,
    fields: &[token_id_field("dumbmonit@pbs!monitor"), TOKEN_SECRET_FIELD],
};

/// Nom d'utilisateur Proxmox, realm compris.
const fn proxmox_user_field(placeholder: &'static str) -> CredentialField {
    cred_text("username", "User name", "User and realm, as in \"dumbmonit@pve\".", placeholder)
}

const PROXMOX_LOGIN_LABEL: &str = "Username / password";
const PROXMOX_LOGIN_HELP: &str =
    "Opens a two-hour session, renewed automatically. Use it only if tokens are not an option.";
const PASSWORD_FIELD: CredentialField = cred_secret("password", "Password", "", "", true);

/// Connexion par mot de passe à l'API Proxmox VE : ouvre un ticket de deux heures.
const PROXMOX_LOGIN: CredentialView = CredentialView {
    kind: "username_password",
    label: PROXMOX_LOGIN_LABEL,
    help: PROXMOX_LOGIN_HELP,
    fields: &[proxmox_user_field("dumbmonit@pve"), PASSWORD_FIELD],
};

const PBS_LOGIN: CredentialView = CredentialView {
    kind: "username_password",
    label: PROXMOX_LOGIN_LABEL,
    help: PROXMOX_LOGIN_HELP,
    fields: &[proxmox_user_field("dumbmonit@pbs"), PASSWORD_FIELD],
};

const SYNOLOGY_LOGIN: CredentialView = CredentialView {
    kind: "username_password",
    label: "DSM account",
    help: "",
    fields: &[
        cred_text("username", "User name", "The DSM account created for monitoring.", "dumbmonit"),
        cred_secret(
            "password",
            "Password",
            "Two-step verification must be off for this account: a monitor cannot type a one-time code.",
            "",
            true,
        ),
    ],
};

const HTTP_TOKEN: CredentialView = CredentialView {
    kind: "api_token",
    label: "Bearer token",
    help: "Sent as \"Authorization: Bearer …\" on every request.",
    fields: &[cred_secret("token", "Token", "Stored encrypted, never shown again.", "", true)],
};

const HTTP_LOGIN: CredentialView = CredentialView {
    kind: "username_password",
    label: "Username / password",
    help: "Sent as HTTP basic authentication.",
    fields: &[
        cred_text("username", "User name", "", ""),
        cred_secret("password", "Password", "", "", true),
    ],
};

/// Champ texte libre.
const fn text(
    key: &'static str,
    label: &'static str,
    help: &'static str,
    placeholder: &'static str,
    default: &'static str,
) -> OptionView {
    OptionView {
        key,
        label,
        help,
        placeholder,
        default,
        required: false,
        input: "text",
        choices: &[],
    }
}

/// Champ numérique. Le défaut est transmis tel qu'écrit dans le code du collecteur.
const fn number(
    key: &'static str,
    label: &'static str,
    help: &'static str,
    placeholder: &'static str,
    default: &'static str,
) -> OptionView {
    OptionView {
        key,
        label,
        help,
        placeholder,
        default,
        required: false,
        input: "number",
        choices: &[],
    }
}

/// Case à cocher. Les collecteurs lisent `true`/`false` mais aussi `1`/`0`,
/// `oui`/`non` : le défaut est donné sous la forme canonique `true`/`false`.
const fn boolean(
    key: &'static str,
    label: &'static str,
    help: &'static str,
    default: bool,
) -> OptionView {
    OptionView {
        key,
        label,
        help,
        placeholder: "",
        default: if default { "true" } else { "false" },
        required: false,
        input: "boolean",
        choices: &[],
    }
}

/// Liste de choix fermée.
const fn select(
    key: &'static str,
    label: &'static str,
    help: &'static str,
    default: &'static str,
    choices: &'static [&'static str],
) -> OptionView {
    OptionView {
        key,
        label,
        help,
        placeholder: "",
        default,
        required: false,
        input: "select",
        choices,
    }
}

/// Délai propre à une sonde de disponibilité, commun aux cinq (`uptime/tags.rs`).
///
/// Le plafond de 60 s est celui du collecteur : au-delà, le délai global du
/// planificateur interromprait la sonde avant qu'elle n'ait pu enregistrer
/// l'indisponibilité.
const PROBE_TIMEOUT: OptionView = number(
    "timeout_seconds",
    "Timeout (seconds)",
    "Time after which the service is reported down if it has not answered. Between 1 and 60.",
    "5",
    "5",
);

/// Accepter un certificat que l'on ne peut pas vérifier (auto-signé, autorité
/// privée). Même clé et même sens pour HTTP, TLS, Proxmox et Synology.
const fn insecure_tls(help: &'static str) -> OptionView {
    boolean("insecure_tls", "Accept an unverifiable certificate", help, false)
}

/// Lever le garde-fou d'adresses des sondes (`collectors/uptime/guard.rs`) : la
/// boucle locale et le lien local sont refusés par défaut, parce qu'ils ne sont
/// joignables que depuis l'hôte de supervision lui-même.
const ALLOW_PRIVATE_TARGETS: OptionView = boolean(
    "allow_private_targets",
    "Allow loopback and link-local targets",
    "By default the check refuses addresses only the DumbMonit host itself can reach (127.0.0.1, ::1, 169.254.x.x). Enable this to monitor a service running on the DumbMonit host. Private LAN addresses (10.x, 192.168.x) are always allowed.",
    false,
);

/// Options lues par `collectors/uptime/http/options.rs`.
const HTTP_OPTIONS: &[OptionView] = &[
    select(
        "method",
        "HTTP method",
        "GET fits almost every case. HEAD avoids downloading the page when only the status code matters.",
        "GET",
        &["GET", "HEAD", "POST", "PUT", "PATCH", "DELETE", "OPTIONS"],
    ),
    text(
        "accepted_status",
        "Accepted status codes",
        "Codes or ranges considered normal, separated by commas. Any other code counts as down.",
        "200-299,301,404",
        "200-299",
    ),
    text(
        "keyword",
        "Expected keyword",
        "Text that must appear in the page. If it is missing, the service is reported down even though the page answers.",
        "Welcome",
        "",
    ),
    boolean(
        "keyword_absent",
        "Keyword must be absent",
        "Inverts the check: the presence of the keyword signals a failure. Useful for an error page that answers 200.",
        false,
    ),
    boolean(
        "keyword_case_sensitive",
        "Match keyword case",
        "By default, upper and lower case are treated the same.",
        false,
    ),
    text(
        "json_path",
        "JSON path to check",
        "For a JSON response: path of the value to check. Fill it in together with the expected value, never one without the other.",
        "$.status",
        "",
    ),
    text(
        "json_expect",
        "Expected JSON value",
        "Value the JSON path above must have, for example \"ok\" or \"true\".",
        "ok",
        "",
    ),
    text(
        "headers",
        "Extra headers",
        "One or more \"Name: value\" headers, separated by \"|\". Do not put secrets here: options are visible in the charts.",
        "Accept: application/json | X-Origin: dumbmonit",
        "",
    ),
    text(
        "body",
        "Request body",
        "Content sent with the request, for methods that expect one (POST, PUT…).",
        "",
        "",
    ),
    boolean(
        "follow_redirects",
        "Follow redirects",
        "Disable to monitor the redirect itself, for example a 301 to HTTPS.",
        true,
    ),
    number(
        "max_redirects",
        "Maximum redirects followed",
        "Between 1 and 20. No effect if redirects are not followed.",
        "10",
        "10",
    ),
    insecure_tls(
        "The check no longer fails on a self-signed certificate or one issued by a private authority.",
    ),
    ALLOW_PRIVATE_TARGETS,
    boolean(
        "check_certificate",
        "Read the certificate",
        "Over HTTPS, also records the certificate expiry date so you can be warned before it expires.",
        true,
    ),
    number(
        "max_body_bytes",
        "Maximum bytes read",
        "Beyond this, the rest of the page is not downloaded. The keyword and JSON path are only searched in this part.",
        "524288",
        "524288",
    ),
    text(
        "user_agent",
        "Announced identity (User-Agent)",
        "Name the check gives to the server. Change it if the server filters robots.",
        "DumbMonit/1.0",
        concat!("DumbMonit/", env!("CARGO_PKG_VERSION")),
    ),
    PROBE_TIMEOUT,
];

/// Options lues par `collectors/uptime/tcp.rs`.
const TCP_OPTIONS: &[OptionView] = &[
    number(
        "port",
        "Port",
        "Port to open, if the address does not already give it as \"host:port\". One of the two is required.",
        "22",
        "",
    ),
    ALLOW_PRIVATE_TARGETS,
    PROBE_TIMEOUT,
];

/// Options lues par `collectors/uptime/dns/options.rs`.
const DNS_OPTIONS: &[OptionView] = &[
    select(
        "record_type",
        "Record type",
        "A for an IPv4 address, AAAA for IPv6, MX for mail, CNAME for an alias…",
        "A",
        &["A", "AAAA", "CNAME", "MX", "TXT", "NS", "SOA", "SRV", "PTR", "CAA"],
    ),
    text(
        "resolver",
        "DNS server to query",
        "IP address of a resolver, port optional. Empty: the system resolver. Handy to check your own DNS server.",
        "1.1.1.1 or 10.0.0.1:5353",
        "",
    ),
    text(
        "expect",
        "Expected values",
        "Values that must all appear in the answer, separated by commas. Empty: only the resolution is checked.",
        "93.184.216.34",
        "",
    ),
    PROBE_TIMEOUT,
];

/// Options lues par `collectors/uptime/ping/options.rs`.
const PING_OPTIONS: &[OptionView] = &[
    number("count", "Number of echoes", "Packets sent on each poll, from 1 to 20.", "4", "4"),
    number(
        "packet_timeout_ms",
        "Wait per packet (ms)",
        "Time allowed for each reply, from 50 to 10,000 ms.",
        "1000",
        "1000",
    ),
    number(
        "interval_ms",
        "Gap between two echoes (ms)",
        "Pause between two echoes, from 0 to 5,000 ms.",
        "100",
        "100",
    ),
    number(
        "payload_bytes",
        "Payload size (bytes)",
        "Data carried by each echo, from 0 to 1,400 bytes.",
        "56",
        "56",
    ),
    select(
        "ip_version",
        "IP version",
        "\"auto\" takes the first resolved address; force 4 or 6 if the host has both and one does not answer.",
        "auto",
        &["auto", "4", "6"],
    ),
    number(
        "loss_threshold_percent",
        "Tolerated loss (%)",
        "Above this percentage of lost packets, the host is reported down. 100: only a total loss counts, partial loss stays visible in the charts.",
        "100",
        "100",
    ),
    number(
        "timeout_seconds",
        "Timeout (seconds)",
        "Total budget for the poll, from 1 to 60. Refused if it does not cover \"number of echoes × wait per packet\".",
        "5",
        "5",
    ),
];

/// Options lues par `collectors/uptime/tls/options.rs`.
const TLS_OPTIONS: &[OptionView] = &[
    text(
        "server_name",
        "Server name (SNI)",
        "Domain name announced to the server and checked in the certificate. Set it when the address is an IP behind a reverse proxy.",
        "www.example.com",
        "",
    ),
    insecure_tls(
        "An unverifiable chain no longer counts as a failure: the expiry date is still recorded.",
    ),
    ALLOW_PRIVATE_TARGETS,
    PROBE_TIMEOUT,
];

/// Options lues par `collectors/push/mod.rs` (`Settings::from_target`).
const PUSH_OPTIONS: &[OptionView] = &[
    text(
        "expected_interval",
        "Expected interval",
        "How often the job is supposed to call in: 30m, 1h, 6h, 24h, 7d (or a number of seconds). A missed call is declared once this interval plus the grace period has passed.",
        "24h",
        "24h",
    ),
    text(
        "grace",
        "Grace period",
        "Extra time tolerated after the expected interval before the heartbeat counts as missed: a percentage of the interval (10%) or a fixed duration (15m). Never less than one minute.",
        "10%",
        "10%",
    ),
];

/// Options lues par `collectors/proxmox/options.rs`.
const PROXMOX_OPTIONS: &[OptionView] = &[
    number("port", "API port", "Used if the address does not give a port.", "8006", "8006"),
    insecure_tls(
        "Proxmox ships with a self-signed certificate by default: enable this if the connection is refused for that reason.",
    ),
    number(
        "request_timeout_seconds",
        "Timeout per request (seconds)",
        "Time allowed for each API call, from 1 to 120.",
        "10",
        "10",
    ),
    number(
        "backup_lookback_days",
        "Backup lookback (days)",
        "Older backup tasks are not examined, from 1 to 3650.",
        "31",
        "31",
    ),
    boolean(
        "scan_backup_storage",
        "Inventory backup archives",
        "Scans the backup storages to date the last backup of each machine. Disable if the storage is slow to answer.",
        true,
    ),
    text(
        "nodes",
        "Monitored nodes",
        "Names of the nodes to monitor, separated by commas. Empty: every node in the cluster.",
        "pve1, pve2",
        "",
    ),
    boolean(
        "ha",
        "Watch high availability",
        "Reads the HA manager state: quorum, master, LRMs and the state of each HA resource.",
        true,
    ),
    boolean(
        "backup_jobs",
        "Watch backup jobs",
        "Reads the scheduled backup jobs (next run, last result) and lists the guests no job covers.",
        true,
    ),
    boolean(
        "scan_snapshots",
        "Inventory snapshots",
        "Lists the snapshots of every VM and container to report their number and age. One API call per guest.",
        true,
    ),
    number(
        "max_snapshot_guests",
        "Snapshot inventory limit",
        "Maximum number of guests whose snapshots are listed per probe, from 1 to 10000. Beyond it, the remaining guests are counted as skipped.",
        "200",
        "200",
    ),
    boolean(
        "replication",
        "Watch replication jobs",
        "Reads the state of ZFS replication jobs on every node.",
        true,
    ),
    boolean(
        "ceph",
        "Watch Ceph",
        "Reads the Ceph cluster health, OSDs and usage. Silently skipped when Ceph is not set up.",
        true,
    ),
    boolean(
        "updates",
        "Count pending updates",
        "Lists the packages waiting for an update on each node. Needs Sys.Modify on \"/nodes\" (see the docs); silently skipped otherwise.",
        true,
    ),
    boolean(
        "certificates",
        "Watch node certificates",
        "Reports the days left before each node certificate expires.",
        true,
    ),
    boolean(
        "guest_agent",
        "Ask the QEMU guest agent",
        "For each running VM: balloon memory and, when the guest agent is enabled, the disk usage seen from inside (needs VM.Monitor). Silently skipped when the agent is absent.",
        true,
    ),
    boolean(
        "disks",
        "Watch physical disks",
        "SMART health, wearout and temperature of every disk of each node.",
        true,
    ),
    boolean(
        "zfs",
        "Watch ZFS pools",
        "Health, capacity and fragmentation of the ZFS pools of each node.",
        true,
    ),
    boolean(
        "packages",
        "Detect package changes",
        "Compares the installed Proxmox packages with the previous probe and reports an upgrade for one hour.",
        true,
    ),
    boolean(
        "subscription",
        "Watch subscription and repositories",
        "Subscription status and APT repositories of each node (enterprise without subscription, unreadable sources).",
        true,
    ),
];

/// Options lues par `collectors/pbs/options.rs`.
const PBS_OPTIONS: &[OptionView] = &[
    number("port", "API port", "Used if the address does not give a port.", "8007", "8007"),
    insecure_tls(
        "Proxmox Backup Server ships with a self-signed certificate by default: enable this if the connection is refused for that reason.",
    ),
    number(
        "request_timeout_seconds",
        "Timeout per request (seconds)",
        "Time allowed for each API call, from 1 to 120. Listing the snapshots of a large datastore can take several seconds.",
        "15",
        "15",
    ),
    number(
        "task_lookback_hours",
        "Task window (hours)",
        "Older tasks are not counted among the failures, from 1 to 8760.",
        "24",
        "24",
    ),
    text(
        "datastores",
        "Monitored datastores",
        "Names of the datastores to monitor, separated by commas. Empty: every datastore.",
        "main, archive",
        "",
    ),
    number(
        "max_groups",
        "Backup group limit",
        "Maximum number of backed-up machines producing series; beyond it, the oldest are ignored. From 1 to 100000.",
        "500",
        "500",
    ),
    boolean(
        "jobs",
        "Watch sync, verify and prune jobs",
        "Reads the job lists to report each job's last result and next run. Needs Datastore.Audit on the datastores, plus Remote.Audit for sync jobs.",
        true,
    ),
    boolean(
        "updates",
        "Count pending updates",
        "Lists the packages waiting for an update on the backup server. Needs Sys.Audit on \"/\"; silently skipped otherwise.",
        true,
    ),
    boolean(
        "disks",
        "Watch disks and ZFS pools",
        "Reads the physical disks (SMART verdict, SSD wear) and the ZFS pools of the backup server (/nodes/localhost/disks). Needs Sys.Audit on \"/system\"; silently skipped otherwise.",
        true,
    ),
];

/// Options lues par `collectors/synology/options.rs`.
const SYNOLOGY_OPTIONS: &[OptionView] = &[
    select(
        "scheme",
        "Protocol",
        "HTTPS fits a NAS fresh out of the box. HTTP only if DSM listens in clear text only.",
        "https",
        &["https", "http"],
    ),
    number(
        "port",
        "DSM port",
        "Used if the address does not give a port. Empty: 5001 over HTTPS, 5000 over HTTP.",
        "5001",
        "",
    ),
    insecure_tls(
        "A NAS ships with a self-signed certificate by default: enable this if the connection is refused for that reason.",
    ),
    number(
        "request_timeout_seconds",
        "Timeout per request (seconds)",
        "Time allowed for each API call, from 1 to 120. The storage inventory can wake up sleeping disks.",
        "15",
        "15",
    ),
    boolean(
        "abb",
        "Watch Active Backup for Business",
        "Reads the Active Backup for Business tasks (PCs, servers, virtual machines, file servers): last result, age of the last successful backup, schedule. Needs the package installed and an account allowed to use it; a NAS without the package is simply skipped.",
        true,
    ),
];

pub async fn list(State(state): State<AppState>) -> Json<Vec<CollectorView>> {
    Json(state.collectors.kinds().into_iter().map(describe).collect())
}

fn describe(kind: &'static str) -> CollectorView {
    match kind {
        "snmp" => CollectorView {
            kind,
            label: "SNMP device",
            summary: "The most universal: almost every piece of network hardware speaks SNMP.",
            examples: &["Switch", "Router", "NAS", "UPS", "Printer"],
            credential_types: &["snmp_community", "snmp_v3"],
            credentials: &[SNMP_COMMUNITY, SNMP_V3],
            address_hint: "192.168.1.10",
            default_port: 161,
            setup: Setup {
                title: "Enable SNMP on the device",
                steps: &[
                    "Open the device's administration interface.",
                    "Look for the SNMP section, often under \"Network\", \"Services\" or \"Administration\".",
                    "Enable SNMP v2c and note the read-only community (\"public\" by default on many devices).",
                    "If the device filters by address, allow this server's address.",
                    "Come back here, enter the address and the community: the rest is detected automatically.",
                ],
                warning: "A community is not encrypted on the network. On a shared network, prefer SNMP v3, which authenticates and encrypts.",
                doc_url: "",
            },
            options: &[],
        },
        "proxmox" => CollectorView {
            kind,
            label: "Proxmox VE",
            summary: "Hypervisor: nodes, virtual machines, containers, storages and backups.",
            examples: &["Proxmox server", "Proxmox cluster"],
            credential_types: &["api_token", "username_password"],
            credentials: &[PROXMOX_TOKEN, PROXMOX_LOGIN],
            address_hint: "192.168.1.20",
            default_port: 8006,
            setup: Setup {
                title: "Create a read-only user and token in Proxmox VE",
                steps: &[
                    "Open a shell on any node (in the web UI: select the node, then Shell; or SSH) and create a user reserved for monitoring. It needs no password: the token is what logs in.\npveum user add dumbmonit@pve --comment \"DumbMonit monitoring\"",
                    "Create a role with only the privileges the collector uses: Sys.Audit (nodes, cluster, HA, disks and SMART, ZFS, certificates, package versions, subscription), Datastore.Audit (storages and backup archives), VM.Audit (VMs, containers, snapshots) and VM.Monitor (disk usage inside VMs, through the QEMU guest agent). None of them can change anything.\npveum role add DumbMonit --privs \"Datastore.Audit Sys.Audit VM.Audit VM.Monitor\"",
                    "Give the user that role on the whole cluster.\npveum aclmod / -user dumbmonit@pve -role DumbMonit",
                    "Create the user's API token. Privilege separation is off, so the token simply inherits the user's rights.\npveum user token add dumbmonit@pve monitor --privsep 0",
                    "The command prints a table with full-tokenid and value. Copy full-tokenid into DumbMonit's Token ID field and value (the UUID) into its Secret field. The secret is shown once: if it is lost, remove the token and create a new one.\ndumbmonit@pve!monitor",
                    "Optional, to count pending updates and pending security fixes: Proxmox guards that list with Sys.Modify. Grant it on /nodes only; without it the collector skips the list silently.\npveum role add DumbMonitUpdates --privs Sys.Modify\npveum aclmod /nodes -user dumbmonit@pve -role DumbMonitUpdates",
                    "Prefer the web UI? The same steps live under Datacenter → Permissions: Users, Roles, Add → User Permission, then API Tokens with \"Privilege Separation\" unticked.",
                    "In DumbMonit, enter the address of any node (port 8006 by default).",
                ],
                warning: "Do not reuse the account you log in with: a leaked token would then control the whole cluster. The DumbMonit role above can only read. Proxmox also uses a self-signed certificate by default: if the connection is refused for that reason, tick \"Accept an unverifiable certificate\" in the options.",
                doc_url: "https://pve.proxmox.com/wiki/User_Management",
            },
            options: PROXMOX_OPTIONS,
        },
        "pbs" => CollectorView {
            kind,
            label: "Proxmox Backup Server",
            summary: "Backup server: backup calendar per machine, failed tasks with their logs, sync/verify/prune/GC jobs, datastores and disks.",
            examples: &["Proxmox Backup Server"],
            credential_types: &["api_token", "username_password"],
            credentials: &[PBS_TOKEN, PBS_LOGIN],
            address_hint: "pbs.lan",
            default_port: 8007,
            setup: Setup {
                title: "Create a read-only user and token in Proxmox Backup Server",
                steps: &[
                    "Open a shell on the backup server (in the web UI: Administration → Shell; or SSH) and create a user reserved for monitoring. It needs no password: the token is what logs in.\nproxmox-backup-manager user create dumbmonit@pbs --comment \"DumbMonit monitoring\"",
                    "Create the user's API token. The command prints the token id and its secret: copy both now, PBS never shows the secret again.\nproxmox-backup-manager user generate-token dumbmonit@pbs monitor",
                    "Give the token the exact read-only minimum, per path. DatastoreAudit on /datastore (Datastore.Audit) reads the datastores, snapshots, verify and prune jobs and GC; Audit on /system (Sys.Audit) reads the node status, the task list and task logs, the disks and ZFS pools. In PBS a token has its own permissions, so the ACL names the token, not the user.\nproxmox-backup-manager acl update /datastore DatastoreAudit --auth-id 'dumbmonit@pbs!monitor'\nproxmox-backup-manager acl update /system Audit --auth-id 'dumbmonit@pbs!monitor'",
                    "Optional: RemoteAudit on /remote (Remote.Audit) lists the sync jobs; Audit on / (Sys.Audit at the top level) lists pending package updates. Without them those two items are silently skipped, nothing else changes. Audit on / alone also covers /datastore and /system if you prefer one line.\nproxmox-backup-manager acl update /remote RemoteAudit --auth-id 'dumbmonit@pbs!monitor'\nproxmox-backup-manager acl update / Audit --auth-id 'dumbmonit@pbs!monitor'",
                    "Copy the token id into DumbMonit's Token ID field and the secret into its Secret field.\ndumbmonit@pbs!monitor",
                    "Prefer the web UI? Configuration → Access Control: Users → Add, then API Tokens → Add, then Permissions → Add → API Token Permission with path /datastore and role DatastoreAudit, and again with path /system and role Audit.",
                    "In DumbMonit, enter the server address, for example \"pbs.lan\" or \"pbs.lan:8007\".",
                ],
                warning: "Do not reuse the account you log in with: a leaked token would then manage every backup. The Audit role can only read. PBS also uses a self-signed certificate by default: if the connection is refused for that reason, tick \"Accept an unverifiable certificate\" in the options.",
                doc_url: "https://pbs.proxmox.com/docs/user-management.html#api-tokens",
            },
            options: PBS_OPTIONS,
        },
        "synology" => CollectorView {
            kind,
            label: "Synology DSM",
            summary: "Synology NAS: volumes, disk health, temperature, Hyper Backup and Active Backup for Business.",
            examples: &["DiskStation", "RackStation"],
            credential_types: &["username_password"],
            credentials: &[SYNOLOGY_LOGIN],
            address_hint: "192.168.1.30",
            default_port: 5001,
            setup: Setup {
                title: "Create a monitoring account in DSM",
                steps: &[
                    "In DSM, open Control Panel → User & Group → User and click Create. Name the account as follows and give it a long password that is used nowhere else.\ndumbmonit",
                    "Join groups: tick administrators. DSM only answers the storage, volume and disk SMART calls to that group; without it the NAS shows as alive but says nothing about its disks. The next two steps take back everything else.",
                    "Assign shared folder permissions: No access on every shared folder. Assign application permissions: Deny everything except DSM, plus Active Backup for Business if you want its tasks read. Skip the quota and speed limit pages.",
                    "Two-step verification must stay off for this account: no automated monitor can type a one-time code. If Control Panel → Security → Account enforces it, restrict the rule to groups this user is not in, or exempt it.",
                    "Active Backup for Business, if installed: its tasks are read only by an account allowed to use the package (Active Backup for Business → Settings → Privileges). Otherwise untick DumbMonit's \"Watch Active Backup for Business\" option to stop asking.",
                    "In DumbMonit, enter the NAS address (HTTPS, port 5001 by default), then this account's user name and password.",
                ],
                warning: "The administrators group is required by DSM's storage API, not by DumbMonit. That is why this account gets no shared folder, no application and a password used nowhere else: it can read the NAS, not touch your files.",
                doc_url: "https://kb.synology.com/en-global/DSM/help/DSM/AdminCenter/file_user_create",
            },
            options: SYNOLOGY_OPTIONS,
        },
        "agent" => CollectorView {
            kind,
            label: "Server with agent",
            summary: "Detailed view of a server: CPU, memory, disks, services, containers.",
            examples: &["Linux server", "Windows server", "Raspberry Pi"],
            credential_types: &["none"],
            credentials: &[NO_AUTH],
            address_hint: "detected automatically",
            default_port: 0,
            setup: Setup {
                title: "Install the agent on the machine",
                steps: &[
                    "Save the device in DumbMonit: an enrollment token (dmon_…) is shown once, with the install command ready to copy for Linux and for Windows.",
                    "That token is the agent's key to push its measurements to DumbMonit. It is not an account on the machine: nothing to create there, and one token may enrol several machines.",
                    "Run the install command on the machine to monitor, with elevated rights (sudo on Linux, an elevated PowerShell on Windows). It downloads the agent, writes the token into agent.yaml and starts the service.",
                    "The machine shows up on its own within a few seconds, named after its host name. Lost the token? Settings → Agents lets you revoke it and create another.",
                    "Docker: to see the containers and let DumbMonit restart or update them, the agent must reach the Docker socket. The service the installer registers already can; if you run the agent under a dedicated user instead, add that user to the \"docker\" group and restart it. Restart and auto-update policies are then set per container on the device page.\nusermod -aG docker dumbmonit",
                    "Plakar backups: detected automatically — the Backups panel appears when the plakar binary or a kloset (~/.config/plakar/stores.yml of every user, ~/.plakar, /var/lib/plakar) is found, and nothing is shown otherwise. Set \"plakar_klosets\" in agent.yaml to watch a fixed list, or \"plakar: false\" to opt out.",
                ],
                warning: "The agent contacts the server, never the other way round: no port needs to be opened on the monitored machine.",
                doc_url: "",
            },
            options: &[],
        },
        // Les cinq sondes de disponibilité surveillent des services, pas des
        // équipements : leur notice explique ce qu'on saisit dans l'adresse, et
        // rappelle que l'état se lit sur « le service répond » et non sur « la sonde
        // a tourné ».
        "http" => CollectorView {
            kind,
            label: "Website or web API (HTTP)",
            summary: "Checks that a page or an API answers, with the right status code, the right content, and a valid certificate.",
            examples: &[
                "Website",
                "Application health page",
                "REST API",
                "Self-hosted service interface",
            ],
            credential_types: &["none", "username_password", "api_token"],
            credentials: &[NO_AUTH, HTTP_LOGIN, HTTP_TOKEN],
            address_hint: "https://example.com/health",
            default_port: 443,
            setup: Setup {
                title: "Monitor a web page",
                steps: &[
                    "In the address, paste the full URL of the page to monitor. Without \"http://\" or \"https://\", HTTPS is assumed.",
                    "Prefer a light page that needs no login, for example the application's health page (\"/health\", \"/status\"), rather than the home page.",
                    "If the page requires authentication, fill in the credential: a username and password give basic authentication, a token is sent as \"Bearer\".",
                    "By default, any status code between 200 and 299 is fine. To go further, require a keyword in the page or a specific value in a JSON response.",
                    "Over HTTPS, the certificate is read automatically: you will be warned before it expires.",
                ],
                warning: "Never write a password or a token in the options: they are copied in clear text on every measurement. Use the credential field, which is encrypted.",
                doc_url: "",
            },
            options: HTTP_OPTIONS,
        },
        "tcp" => CollectorView {
            kind,
            label: "Network port (TCP)",
            summary: "Checks that a port accepts connections: SSH, database, file share…",
            examples: &["SSH", "SMB or NFS share", "Database", "Game server", "Network printer"],
            credential_types: &["none"],
            credentials: &[NO_AUTH],
            address_hint: "nas.home.lan:22",
            default_port: 0,
            setup: Setup {
                title: "Monitor a port",
                steps: &[
                    "In the address, write the host followed by the port, separated by a colon: \"nas.home.lan:22\" or \"192.168.1.5:445\".",
                    "For an IPv6 address, put it in brackets: \"[fd00::1]:445\".",
                    "You can also leave the address without a port and enter it in the \"Port\" option: one of the two is required.",
                    "Only the connection opening is tested: no data is sent to the service, so it has no effect on it.",
                ],
                warning: "An open port does not prove the application behind it works. For a web service, prefer the \"Website or web API\" type, which reads the response.",
                doc_url: "",
            },
            options: TCP_OPTIONS,
        },
        "dns" => CollectorView {
            kind,
            label: "Domain name (DNS)",
            summary: "Checks that a name resolves, and that it points to the right address.",
            examples: &[
                "Your domain name",
                "An internal name served by your Pi-hole or AdGuard",
                "An MX record",
            ],
            credential_types: &["none"],
            credentials: &[NO_AUTH],
            address_hint: "www.example.com",
            default_port: 53,
            setup: Setup {
                title: "Monitor a DNS resolution",
                steps: &[
                    "In the address, write the name to resolve, without \"http://\": \"www.example.com\".",
                    "By default, the check asks the system resolver for an IPv4 address (A record). Pick another record type in the options if needed.",
                    "To monitor your own DNS server, enter its IP address in \"DNS server to query\": the check will fail if it stops answering.",
                    "To detect hijacking or a misconfiguration, list in \"Expected values\" the addresses the answer must contain.",
                ],
                warning: "The DNS server is given by its IP address, not by a name: it would take a resolver to resolve the resolver.",
                doc_url: "",
            },
            options: DNS_OPTIONS,
        },
        "ping" => CollectorView {
            kind,
            label: "Reachable host (ping)",
            summary: "Sends a few ICMP echoes and measures response time and packet loss.",
            examples: &[
                "Home router or gateway",
                "Wi-Fi access point",
                "Printer",
                "Machine without agent or SNMP",
                "Remote host",
            ],
            credential_types: &["none"],
            credentials: &[NO_AUTH],
            address_hint: "192.168.1.1",
            default_port: 0,
            setup: Setup {
                title: "Monitor a host with ping",
                steps: &[
                    "In the address, write the host name or IP address: \"192.168.1.1\" or \"router.home.lan\".",
                    "On each poll, the check sends four echoes and measures the response time, along with the share of lost packets.",
                    "By default, only a total loss counts as down; partial loss stays visible in the charts. Lower \"Tolerated loss\" to be warned earlier.",
                    "If DumbMonit runs in Docker, add the NET_RAW capability to the container: in docker-compose.yml, uncomment the \"cap_add: - NET_RAW\" lines under the dumbmonit service, then restart it.",
                ],
                warning: "Without the NET_RAW capability, the check cannot open an ICMP socket: it reports this as a configuration error, not as a host failure. Some devices also ignore pings on purpose: check that before drawing conclusions.",
                doc_url: "",
            },
            options: PING_OPTIONS,
        },
        "tls" => CollectorView {
            kind,
            label: "TLS certificate",
            summary: "Checks that a certificate is valid and warns before it expires, on any encrypted port.",
            examples: &[
                "Mail server (IMAPS, SMTPS)",
                "Reverse proxy",
                "LDAPS directory",
                "MQTT broker",
                "Administration interface",
            ],
            credential_types: &["none"],
            credentials: &[NO_AUTH],
            address_hint: "mail.example.com:993",
            default_port: 443,
            setup: Setup {
                title: "Monitor a certificate",
                steps: &[
                    "In the address, write the host and, if it is not 443, the port of the encrypted service: \"mail.example.com:993\", \"ldap.home.lan:636\".",
                    "The check opens an encrypted connection, reads the presented certificate and closes right away: no data is exchanged with the application.",
                    "It records the number of days before expiry, the issuer and the TLS version. The bundled alert rules warn at fourteen days, then at expiry.",
                    "If the service is reached by its IP address but the certificate carries a name, enter that name in \"Server name (SNI)\".",
                    "For a certificate issued by your own authority, tick \"Accept an unverifiable certificate\": the expiry date is still monitored.",
                ],
                warning: "For a website, the \"Website or web API\" type already reads the certificate: this type is meant for services that do not speak HTTP, or whose application you do not want to hit.",
                doc_url: "",
            },
            options: TLS_OPTIONS,
        },
        // Moniteur en poussée : rien n'est interrogé, c'est le travail surveillé
        // qui appelle. L'adresse n'est qu'un libellé : elle doit rester unique
        // parmi les heartbeats, comme toute adresse pour un type donné.
        "push" => CollectorView {
            kind,
            label: "Heartbeat (push)",
            summary: "A cron job, backup script or automation that must call DumbMonit regularly; if it stops calling, you are told.",
            examples: &[
                "Nightly backup script",
                "Cron job",
                "Home Assistant automation",
                "Certificate renewal",
                "Database dump",
            ],
            credential_types: &["none"],
            credentials: &[NO_AUTH],
            address_hint: "nightly-backup",
            default_port: 0,
            setup: Setup {
                title: "Make the job call in",
                steps: &[
                    "In the address, write a short label for the job (\"nightly-backup\", \"certbot-renew\"): nothing is contacted, the label only has to be unique among your heartbeats.",
                    "Set the expected interval to the job's schedule (\"24h\" for a nightly job, \"1h\" for an hourly one) and, if the schedule drifts, a wider grace period. Save the device.",
                    "The device page shows the URL to call. Add it at the end of the job, so it is called only when the job succeeded:\ncurl -fsS -m 10 --retry 3 https://monit.example.com/api/push/<token>",
                    "A job that can tell when it failed may say so instead of staying silent: append ?status=down&msg=… to the URL, and the alert fires at once.",
                    "Until the first call arrives the device shows \"Waiting\" and nothing is alerted. Lost or leaked URL? \"Regenerate\" on the device page issues a new one; the old one stops answering immediately.",
                ],
                warning: "Call the URL at the end of the job, after the part that matters. A call placed at the top would report a success even when the backup itself failed.",
                doc_url: "",
            },
            options: PUSH_OPTIONS,
        },
        "dummy" => CollectorView {
            kind,
            label: "Demo device",
            summary: "Fictional device producing measurements, to explore the tool without hardware.",
            examples: &["No hardware required"],
            credential_types: &["none"],
            credentials: &[NO_AUTH],
            address_hint: "demo",
            default_port: 0,
            setup: Setup {
                title: "Nothing to prepare",
                steps: &[
                    "Give it any name and confirm.",
                    "Fake measurements will appear right away, enough to browse the interface.",
                ],
                warning: "",
                doc_url: "",
            },
            options: &[],
        },
        other => CollectorView {
            kind,
            label: other,
            summary: "",
            examples: &[],
            credential_types: &["none"],
            credentials: &[NO_AUTH],
            address_hint: "",
            default_port: 0,
            setup: Setup { title: "", steps: &[], warning: "", doc_url: "" },
            options: &[],
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Les types enregistrés dans `main.rs`. Un type ajouté là-bas sans notice ici
    /// s'afficherait sous son nom brut, sans explication ni exemple d'adresse.
    const KINDS_ENREGISTRES: &[&str] = &[
        "snmp", "proxmox", "pbs", "synology", "agent", "http", "tcp", "dns", "ping", "tls", "push",
        "dummy",
    ];

    #[test]
    fn chaque_type_enregistre_a_une_notice_qui_lui_est_propre() {
        for kind in KINDS_ENREGISTRES {
            let view = describe(kind);
            assert_eq!(view.kind, *kind);
            assert!(
                view.label != *kind || !view.summary.is_empty(),
                "« {kind} » n'a que la description générique"
            );
            assert!(!view.setup.title.is_empty(), "« {kind} » n'a pas de notice de mise en route");
            assert!(!view.setup.steps.is_empty(), "« {kind} » n'a aucune étape");
            assert!(!view.address_hint.is_empty(), "« {kind} » n'a pas d'adresse d'exemple");
            assert!(
                !view.credential_types.is_empty(),
                "« {kind} » n'annonce aucune forme d'identifiant"
            );
        }
    }

    #[test]
    fn un_type_inconnu_recoit_une_description_generique_mais_complete() {
        let view = describe("inconnu");
        assert_eq!(view.label, "inconnu");
        assert!(view.summary.is_empty());
        assert!(view.options.is_empty());
        assert_eq!(view.credential_types, &["none"]);
    }

    /// Les sondes lisent ces étiquettes dans leurs `options.rs` : l'interface ne
    /// doit pas proposer un champ que le collecteur ignorerait, ni en cacher un.
    #[test]
    fn les_options_des_sondes_reprennent_les_etiquettes_lues_par_les_collecteurs() {
        let attendues: &[(&str, &[&str])] = &[
            (
                "http",
                &[
                    "method",
                    "accepted_status",
                    "keyword",
                    "keyword_absent",
                    "keyword_case_sensitive",
                    "json_path",
                    "json_expect",
                    "headers",
                    "body",
                    "follow_redirects",
                    "max_redirects",
                    "insecure_tls",
                    "allow_private_targets",
                    "check_certificate",
                    "max_body_bytes",
                    "user_agent",
                    "timeout_seconds",
                ],
            ),
            ("tcp", &["port", "allow_private_targets", "timeout_seconds"]),
            ("dns", &["record_type", "resolver", "expect", "timeout_seconds"]),
            (
                "ping",
                &[
                    "count",
                    "packet_timeout_ms",
                    "interval_ms",
                    "payload_bytes",
                    "ip_version",
                    "loss_threshold_percent",
                    "timeout_seconds",
                ],
            ),
            ("tls", &["server_name", "insecure_tls", "allow_private_targets", "timeout_seconds"]),
            (
                "proxmox",
                &[
                    "port",
                    "insecure_tls",
                    "request_timeout_seconds",
                    "backup_lookback_days",
                    "scan_backup_storage",
                    "nodes",
                    "ha",
                    "backup_jobs",
                    "scan_snapshots",
                    "max_snapshot_guests",
                    "replication",
                    "ceph",
                    "updates",
                    "certificates",
                    "guest_agent",
                    "disks",
                    "zfs",
                    "packages",
                    "subscription",
                ],
            ),
            (
                "pbs",
                &[
                    "port",
                    "insecure_tls",
                    "request_timeout_seconds",
                    "task_lookback_hours",
                    "datastores",
                    "max_groups",
                    "jobs",
                    "updates",
                    "disks",
                ],
            ),
            ("synology", &["scheme", "port", "insecure_tls", "request_timeout_seconds", "abb"]),
            ("push", &["expected_interval", "grace"]),
        ];
        for (kind, cles) in attendues {
            let obtenues: Vec<&str> = describe(kind).options.iter().map(|o| o.key).collect();
            assert_eq!(&obtenues, cles, "options de « {kind} »");
        }
        for kind in ["snmp", "agent", "dummy"] {
            assert!(describe(kind).options.is_empty(), "« {kind} » ne lit aucune étiquette");
        }
    }

    #[test]
    fn chaque_option_est_coherente_avec_son_type_de_champ() {
        for kind in KINDS_ENREGISTRES {
            for option in describe(kind).options {
                let contexte = format!("option « {} » de « {kind} »", option.key);
                assert!(!option.label.is_empty(), "{contexte} : sans libellé");
                assert!(!option.help.is_empty(), "{contexte} : sans aide");
                match option.input {
                    "select" => {
                        assert!(!option.choices.is_empty(), "{contexte} : liste sans choix");
                        assert!(
                            option.choices.contains(&option.default),
                            "{contexte} : le défaut « {} » n'est pas dans la liste",
                            option.default
                        );
                    }
                    "boolean" => assert!(
                        matches!(option.default, "true" | "false"),
                        "{contexte} : défaut booléen « {} »",
                        option.default
                    ),
                    "number" => assert!(
                        option.default.is_empty() || option.default.parse::<u64>().is_ok(),
                        "{contexte} : défaut numérique « {} »",
                        option.default
                    ),
                    "text" => assert!(option.choices.is_empty(), "{contexte} : choix sur un texte"),
                    other => panic!("{contexte} : type de champ inconnu « {other} »"),
                }
            }
        }
    }

    /// Les défauts affichés doivent être ceux que les collecteurs appliquent
    /// réellement : un défaut faux dans le formulaire est pire qu'aucun.
    #[test]
    fn les_defauts_affiches_sont_ceux_des_collecteurs() {
        let defaut = |kind: &'static str, cle: &str| {
            describe(kind).options.iter().find(|o| o.key == cle).map(|o| o.default).unwrap()
        };
        assert_eq!(defaut("http", "timeout_seconds"), "5");
        assert_eq!(defaut("http", "max_body_bytes"), "524288");
        assert_eq!(defaut("http", "max_redirects"), "10");
        assert_eq!(
            defaut("http", "user_agent"),
            format!("DumbMonit/{}", env!("CARGO_PKG_VERSION"))
        );
        assert_eq!(defaut("ping", "count"), "4");
        assert_eq!(defaut("ping", "packet_timeout_ms"), "1000");
        assert_eq!(defaut("proxmox", "port"), "8006");
        assert_eq!(defaut("proxmox", "backup_lookback_days"), "31");
        assert_eq!(defaut("pbs", "port"), "8007");
        assert_eq!(defaut("pbs", "request_timeout_seconds"), "15");
        assert_eq!(defaut("pbs", "task_lookback_hours"), "24");
        assert_eq!(defaut("pbs", "max_groups"), "500");
        assert_eq!(defaut("synology", "request_timeout_seconds"), "15");
        assert_eq!(defaut("synology", "abb"), "true");
        assert_eq!(
            defaut("push", "expected_interval"),
            crate::collectors::push::DEFAULT_EXPECTED_INTERVAL
        );
        assert_eq!(defaut("push", "grace"), crate::collectors::push::DEFAULT_GRACE);
    }

    /// `credential_types` et `credentials` décrivent la même liste : l'ancienne
    /// interface lit la première, la nouvelle la seconde.
    #[test]
    fn les_formes_didentifiant_sont_decrites_champ_par_champ() {
        for kind in KINDS_ENREGISTRES {
            let view = describe(kind);
            let kinds: Vec<&str> = view.credentials.iter().map(|c| c.kind).collect();
            assert_eq!(&kinds, view.credential_types, "« {kind} » : formes annoncées");
            for credential in view.credentials {
                assert!(!credential.label.is_empty(), "« {kind} » : forme sans libellé");
                let mut keys = std::collections::BTreeSet::new();
                for field in credential.fields {
                    let contexte = format!("champ « {} » de « {kind} »", field.key);
                    assert!(keys.insert(field.key), "{contexte} : déclaré deux fois");
                    assert!(!field.label.is_empty(), "{contexte} : sans libellé");
                    match field.input {
                        "select" => assert!(!field.choices.is_empty(), "{contexte} : sans choix"),
                        "text" | "password" => {
                            assert!(field.choices.is_empty(), "{contexte} : choix sur un texte")
                        }
                        other => panic!("{contexte} : type de champ inconnu « {other} »"),
                    }
                }
                if credential.kind == "none" {
                    assert!(credential.fields.is_empty(), "« {kind} » : « none » sans champ");
                } else {
                    assert!(!credential.fields.is_empty(), "« {kind} » : forme sans champ");
                }
            }
        }
    }

    /// Un jeton Proxmox se saisit en deux cases ; les clés sont celles que
    /// `dumbmonit_proto::Credential` sait recomposer.
    #[test]
    fn un_jeton_proxmox_se_saisit_en_deux_champs() {
        for kind in ["proxmox", "pbs"] {
            let view = describe(kind);
            let token = view.credentials.iter().find(|c| c.kind == "api_token").unwrap();
            let keys: Vec<&str> = token.fields.iter().map(|f| f.key).collect();
            assert_eq!(keys, ["token_id", "secret"], "« {kind} »");
            assert!(token.fields[0].placeholder.contains('!'), "l'exemple montre user@realm!nom");
            assert_eq!(token.fields[1].input, "password");
            let credential: dumbmonit_proto::Credential =
                serde_json::from_value(serde_json::json!({
                    "type": "api_token",
                    "token_id": token.fields[0].placeholder,
                    "secret": "8f3a1c9e-0000-4444-8888-aaaabbbbcccc",
                }))
                .unwrap();
            assert_eq!(
                credential,
                dumbmonit_proto::Credential::ApiToken {
                    token: format!(
                        "{}=8f3a1c9e-0000-4444-8888-aaaabbbbcccc",
                        token.fields[0].placeholder
                    )
                }
            );
        }
    }

    /// Texte d'une notice, étapes et mise en garde comprises.
    fn notice(kind: &'static str) -> String {
        let view = describe(kind);
        let mut text = view.setup.steps.join("\n");
        text.push('\n');
        text.push_str(view.setup.warning);
        text
    }

    /// Les tutoriels font créer un compte réservé, jamais réutiliser le compte
    /// tout-puissant : le mot « root » ou « admin » n'a rien à y faire — sauf le
    /// nom du groupe `administrators` que l'API de stockage de DSM exige, et qui
    /// est justement expliqué.
    #[test]
    fn les_notices_font_creer_un_compte_dedie_et_ne_citent_jamais_le_compte_racine() {
        let attendus = [
            ("proxmox", "dumbmonit@pve"),
            ("pbs", "dumbmonit@pbs"),
            ("synology", "dumbmonit"),
            ("agent", "token"),
        ];
        for (kind, dedie) in attendus {
            let text = notice(kind);
            assert!(text.contains(dedie), "« {kind} » ne nomme pas le compte dédié « {dedie} »");
            let allowed = text.to_lowercase().replace("administrators", "");
            for word in allowed.split(|c: char| !c.is_alphanumeric()) {
                assert!(
                    !matches!(word, "root" | "admin" | "administrator"),
                    "la notice de « {kind} » cite « {word} » comme compte à utiliser"
                );
            }
        }
    }

    /// Les commandes des notices sont des lignes à copier telles quelles : pas
    /// d'espace autour, pas de guillemets typographiques qu'un shell refuserait.
    #[test]
    fn les_commandes_des_notices_sont_copiables_telles_quelles() {
        for kind in KINDS_ENREGISTRES {
            for step in describe(kind).setup.steps {
                let mut lines = step.lines();
                let text = lines.next().unwrap_or_default();
                assert!(!text.trim().is_empty(), "« {kind} » : étape sans texte");
                for command in lines {
                    assert_eq!(command, command.trim(), "« {kind} » : commande avec des espaces");
                    assert!(!command.is_empty(), "« {kind} » : ligne de commande vide");
                    assert!(
                        !command.contains(['“', '”', '‘', '’']),
                        "« {kind} » : guillemets typographiques dans « {command} »"
                    );
                }
            }
        }
    }

    /// Les pages `docs/devices/*.md` reprennent les notices mot pour mot : le
    /// tutoriel affiché dans l'application et celui de la documentation ne
    /// doivent pas diverger. La comparaison ignore les retours à la ligne et
    /// les accents de code Markdown.
    #[test]
    fn la_documentation_reprend_les_notices_mot_pour_mot() {
        let docs: &[(&str, &str)] = &[
            ("proxmox", include_str!("../../../../docs/devices/proxmox.md")),
            ("pbs", include_str!("../../../../docs/devices/pbs.md")),
            ("synology", include_str!("../../../../docs/devices/synology.md")),
            ("agent", include_str!("../../../../docs/devices/agent.md")),
            ("push", include_str!("../../../../docs/devices/push.md")),
        ];
        fn flatten(text: &str) -> String {
            text.replace('`', "").split_whitespace().collect::<Vec<_>>().join(" ")
        }
        for (kind, doc) in docs {
            let flat = flatten(doc);
            let view = describe(kind);
            for step in view.setup.steps {
                let mut lines = step.lines();
                let text = flatten(lines.next().unwrap_or_default());
                assert!(flat.contains(&text), "docs/devices/{kind}.md ne reprend pas : {text}");
                for command in lines {
                    assert!(
                        doc.contains(command),
                        "docs/devices/{kind}.md ne reprend pas : {command}"
                    );
                }
            }
            let warning = flatten(view.setup.warning);
            assert!(flat.contains(&warning), "docs/devices/{kind}.md ne reprend pas : {warning}");
        }
    }

    #[test]
    fn la_notice_du_ping_mentionne_la_capacite_reseau_necessaire() {
        let view = describe("ping");
        let texte = view.setup.steps.join(" ") + view.setup.warning;
        assert!(texte.contains("NET_RAW"), "l'utilisateur doit savoir quoi décommenter");
    }

    #[test]
    fn la_sonde_http_annonce_les_identifiants_quelle_sait_envoyer() {
        let view = describe("http");
        assert!(view.credential_types.contains(&"username_password"));
        assert!(view.credential_types.contains(&"api_token"));
        for kind in ["tcp", "dns", "ping", "tls", "push"] {
            assert_eq!(describe(kind).credential_types, &["none"], "« {kind} » n'envoie rien");
        }
    }
}
