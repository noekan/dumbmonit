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
    /// Formes de `credential` acceptées.
    pub credential_types: &'static [&'static str],
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
    PROBE_TIMEOUT,
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
            address_hint: "192.168.1.20",
            default_port: 8006,
            setup: Setup {
                title: "Create an API token in Proxmox",
                steps: &[
                    "In Proxmox, go to Datacenter → Permissions → API Tokens.",
                    "Click \"Add\", pick a user and name the token (for example \"dumbmonit\").",
                    "Untick \"Privilege Separation\" so the token inherits the user's rights.",
                    "Copy the secret shown right away: Proxmox will never show it again.",
                    "In Datacenter → Permissions, give this user the PVEAuditor role on \"/\".",
                    "Paste the full token here, as user@realm!name=secret.",
                ],
                warning: "Proxmox uses a self-signed certificate by default. If the connection is refused for that reason, tick \"Accept an unverifiable certificate\" in the options.",
                doc_url: "https://pve.proxmox.com/wiki/Proxmox_VE_API",
            },
            options: PROXMOX_OPTIONS,
        },
        "pbs" => CollectorView {
            kind,
            label: "Proxmox Backup Server",
            summary: "Backup server: datastores, backup age and verification, failed tasks.",
            examples: &["Proxmox Backup Server"],
            credential_types: &["api_token", "username_password"],
            address_hint: "pbs.lan",
            default_port: 8007,
            setup: Setup {
                title: "Create an API token in Proxmox Backup Server",
                steps: &[
                    "In PBS, go to Configuration → Access Control → Users and create a dedicated user, for example \"monitoring@pbs\".",
                    "In the API Tokens tab, add a token to this user (for example \"dumbmonit\") and copy the secret shown right away: PBS will never show it again.",
                    "In the Permissions tab, give the token the DatastoreAudit role on \"/datastore\" and the Audit role on \"/system\": that is the read-only minimum.",
                    "Paste the full token here, as user@pbs!name=secret.",
                    "Enter the server address, for example \"pbs.lan\" or \"pbs.lan:8007\".",
                ],
                warning: "PBS uses a self-signed certificate by default. If the connection is refused for that reason, tick \"Accept an unverifiable certificate\" in the options.",
                doc_url: "https://pbs.proxmox.com/docs/user-management.html#api-tokens",
            },
            options: PBS_OPTIONS,
        },
        "synology" => CollectorView {
            kind,
            label: "Synology DSM",
            summary: "Synology NAS: volumes, disk health, temperature and backups.",
            examples: &["DiskStation", "RackStation"],
            credential_types: &["username_password"],
            address_hint: "192.168.1.30",
            default_port: 5001,
            setup: Setup {
                title: "Prepare the Synology NAS",
                steps: &[
                    "In DSM, open Control Panel → User & Group.",
                    "Create a user dedicated to monitoring, without administration rights.",
                    "Give it read-only access; it needs no shared folder.",
                    "If two-step verification is enforced for everyone, exempt this account, otherwise the login will fail.",
                    "Enter the NAS address and this account's credentials here.",
                ],
                warning: "An administrator account would work, but would give DumbMonit far more rights than needed.",
                doc_url: "",
            },
            options: SYNOLOGY_OPTIONS,
        },
        "agent" => CollectorView {
            kind,
            label: "Server with agent",
            summary: "Detailed view of a server: CPU, memory, disks, services, containers.",
            examples: &["Linux server", "Windows server", "Raspberry Pi"],
            credential_types: &["none"],
            address_hint: "detected automatically",
            default_port: 0,
            setup: Setup {
                title: "Install the agent on the machine",
                steps: &[
                    "Create the device here: an enrollment token will be shown.",
                    "Copy the install command shown and run it on the machine to monitor.",
                    "The agent installs itself as a service and pushes its measurements to this server.",
                    "The machine shows up on its own within a few seconds.",
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
            address_hint: "192.168.1.1",
            default_port: 0,
            setup: Setup {
                title: "Monitor a host with ping",
                steps: &[
                    "In the address, write the host name or IP address: \"192.168.1.1\" or \"router.home.lan\".",
                    "On each poll, the check sends four echoes and measures the response time, along with the share of lost packets.",
                    "By default, only a total loss counts as down; partial loss stays visible in the charts. Lower \"Tolerated loss\" to be warned earlier.",
                    "If DumbMonit runs in Docker, add the NET_RAW capability to the container: in docker-compose.yml, uncomment the \"cap_add: - NET_RAW\" lines under the ezymonit service, then restart it.",
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
        "dummy" => CollectorView {
            kind,
            label: "Demo device",
            summary: "Fictional device producing measurements, to explore the tool without hardware.",
            examples: &["No hardware required"],
            credential_types: &["none"],
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
        "snmp", "proxmox", "pbs", "synology", "agent", "http", "tcp", "dns", "ping", "tls", "dummy",
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
                    "check_certificate",
                    "max_body_bytes",
                    "user_agent",
                    "timeout_seconds",
                ],
            ),
            ("tcp", &["port", "timeout_seconds"]),
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
            ("tls", &["server_name", "insecure_tls", "timeout_seconds"]),
            (
                "proxmox",
                &[
                    "port",
                    "insecure_tls",
                    "request_timeout_seconds",
                    "backup_lookback_days",
                    "scan_backup_storage",
                    "nodes",
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
                ],
            ),
            ("synology", &["scheme", "port", "insecure_tls", "request_timeout_seconds"]),
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
        for kind in ["tcp", "dns", "ping", "tls"] {
            assert_eq!(describe(kind).credential_types, &["none"], "« {kind} » n'envoie rien");
        }
    }
}
