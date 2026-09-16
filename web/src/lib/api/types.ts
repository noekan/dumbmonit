/**
 * Types of the DumbMonit server API contract.
 *
 * They mirror exactly what the Rust backend returns (`crates/server/src/api`).
 * Any divergence here turns into a silent bug at runtime: these types are not
 * validated at runtime, they describe a promise kept by the server.
 */

/** Identifier of a target: the SQLite `rowid` on the server side. */
export type TargetId = number;

// --- Health -----------------------------------------------------------------

export interface ComponentHealth {
	ok: boolean;
	error?: string;
	/** VictoriaMetrics only: true when the server runs it itself, false with DUMBMONIT_VM_URL. */
	embedded?: boolean;
}

export interface Health {
	status: 'ok' | 'degraded';
	version: string;
	database: ComponentHealth;
	victoria: ComponentHealth;
}

// --- Credentials ------------------------------------------------------------

export type SnmpV3AuthProtocol = 'md5' | 'sha1' | 'sha224' | 'sha256' | 'sha384' | 'sha512';
export type SnmpV3PrivacyProtocol = 'des' | 'aes128' | 'aes192' | 'aes256';

export type Credential =
	| { type: 'none' }
	| { type: 'snmp_community'; community: string }
	| {
			type: 'snmp_v3';
			username: string;
			auth?: { protocol: SnmpV3AuthProtocol; passphrase: string } | null;
			privacy?: { protocol: SnmpV3PrivacyProtocol; passphrase: string } | null;
			context?: string | null;
	  }
	| { type: 'api_token'; token: string }
	| { type: 'username_password'; username: string; password: string };

/** Credential families offered by the form, in display order. */
export const CREDENTIAL_KINDS = [
	{ value: 'snmp_community', label: 'SNMP v1 / v2c (community)' },
	{ value: 'snmp_v3', label: 'SNMP v3' },
	{ value: 'api_token', label: 'API token' },
	{ value: 'username_password', label: 'Username / password' },
	{ value: 'none', label: 'No authentication' }
] as const;

export type CredentialKind = (typeof CREDENTIAL_KINDS)[number]['value'];

// --- Targets ----------------------------------------------------------------

/**
 * A target as returned on read.
 *
 * `credential_kind` is a human label ("SNMP community"). The secret itself is
 * never exposed by the API, by design.
 */
export interface Target {
	id: TargetId;
	name: string;
	address: string;
	kind: string;
	profile_id: string | null;
	parent_id: TargetId | null;
	interval_secs: number;
	enabled: boolean;
	tags: Record<string, string>;
	credential_kind: string;
	last_probe_at: string | null;
	last_error: string | null;
}

/** Body sent to `POST /api/targets` and `PUT /api/targets/{id}`. */
export interface TargetPayload {
	name: string;
	address: string;
	kind: string;
	profile_id?: string | null;
	parent_id?: TargetId | null;
	interval_secs?: number;
	enabled?: boolean;
	tags?: Record<string, string>;
	credential?: Credential;
}

/** Result of an immediate probe — the main diagnostic tool. */
export interface ProbeReport {
	sample_count: number;
	series: string[];
}

// --- Metrics ----------------------------------------------------------------

/** A series in Prometheus format: `[timestamp_seconds, "value"]`. */
export interface MetricSeries {
	metric: Record<string, string>;
	values: [number, string][];
}

// --- Alerts -----------------------------------------------------------------

/** Severity ladder as declared by a rule (mapped to the weather words in the UI). */
export type AlertSeverity = 'info' | 'warning' | 'critical';

/** Where the state machine stands for one alert. */
export type AlertPhase = 'ok' | 'pending' | 'firing' | 'resolved';

/** What the user must read: `phase`, but with suppression made visible. */
export type AlertEffectivePhase = AlertPhase | 'suppressed';

/** How a rule watches its metric. */
export type RuleKind = 'threshold' | 'anomaly' | 'predict';

/** Comparison operator of a threshold rule. */
export type RuleOperator = '>' | '>=' | '<' | '<=';

/** Scope of a rule: everything, an explicit list, or a tag match. */
export type TargetSelector =
	| { kind: 'all' }
	| { kind: 'ids'; ids: TargetId[] }
	| { kind: 'labels'; labels: Record<string, string> };

/** Anomaly-detection settings; only exposed for `kind === 'anomaly'` rules. */
export interface AnomalyParams {
	k: number;
	alpha: number;
	mad_floor_abs: number;
	mad_floor_rel: number;
	min_samples: number;
}

/**
 * One active alert, mirroring the server's `ActiveAlertView`.
 *
 * A `firing` alert can be `suppressed` (its parent device is unreachable): the
 * `phase` stays `firing` for the engine — so it is not re-notified when the
 * parent recovers — while `effective_phase` becomes `suppressed`, which is what
 * the interface displays. There is no `target_name` here: resolve it from the
 * targets list via `target_id`.
 */
export interface Alert {
	fingerprint: string;
	rule_uid: string;
	/** Current name of the rule; empty if the rule was deleted since. */
	rule_name: string;
	severity: AlertSeverity;
	target_id: TargetId | null;
	series_key: string;
	labels: Record<string, string>;
	phase: AlertPhase;
	effective_phase: AlertEffectivePhase;
	suppressed: boolean;
	/** Id of the unreachable parent device that masks this alert. */
	suppressed_by: TargetId | null;
	silenced: boolean;
	/** True while a baseline rule is still learning (silent for its first days). */
	learning: boolean;
	value: number | null;
	score: number | null;
	condition_since: string | null;
	firing_since: string | null;
	last_eval_at: string | null;
	last_notified_at: string | null;
	notify_count: number;
}

/** One recorded phase transition, mirroring the server's `HistoryEntryView`. */
export interface AlertHistoryEntry {
	id: number;
	fingerprint: string;
	rule_uid: string;
	target_id: TargetId | null;
	from_phase: AlertPhase;
	to_phase: AlertPhase;
	severity: AlertSeverity;
	value: number | null;
	/** False when nothing was sent; `reason` then says why (learning, suppression…). */
	notified: boolean;
	reason: string;
	/** RFC 3339 timestamp in UTC. */
	at: string;
}

/** An alert rule, mirroring the server's `RuleView`. */
export interface AlertRule {
	id: number;
	uid: string;
	name: string;
	description: string;
	kind: RuleKind;
	query: string;
	operator: RuleOperator;
	threshold: number;
	/** Hysteresis: the alert clears only past this value. `null` = none. */
	clear_threshold: number | null;
	for_secs: number;
	severity: AlertSeverity;
	selector: TargetSelector;
	channels: number[];
	params: AnomalyParams;
	unit: string;
	repeat_secs: number | null;
	escalate_after_secs: number | null;
	enabled: boolean;
	/** Shipped rule: it can be toggled but not deleted. */
	builtin: boolean;
	/** Per-device overrides of this rule. */
	overrides: RuleOverride[];
}

/**
 * Body of `POST /api/alerts/rules` and `PUT /api/alerts/rules/{id}`.
 *
 * Enumerations travel as free strings (`severity`, `operator`, `kind`); the
 * server validates them and rejects a misspelling explicitly.
 */
export interface AlertRulePayload {
	uid?: string | null;
	name: string;
	description?: string | null;
	kind?: RuleKind | string | null;
	query: string;
	operator?: RuleOperator | string | null;
	threshold?: number | null;
	clear_threshold?: number | null;
	for_secs?: number | null;
	severity?: AlertSeverity | string | null;
	selector?: TargetSelector | null;
	channels?: number[] | null;
	params?: Partial<AnomalyParams> | null;
	unit?: string | null;
	repeat_secs?: number | null;
	escalate_after_secs?: number | null;
	enabled?: boolean | null;
}

/** Maintenance-window schedule: a one-off window or a weekly recurrence. */
export type SilenceSchedule =
	| { kind: 'once'; starts_at: string; ends_at: string }
	| {
			kind: 'weekly';
			/** Days it covers, 0 = Monday … 6 = Sunday. */
			days: number[];
			/** Minutes since local midnight, 0…1439. */
			start_minute: number;
			end_minute: number;
			/** Offset of the local timezone from UTC, in minutes. */
			utc_offset_minutes: number;
	  };

/** A maintenance window, mirroring the server's `SilenceView`. */
export interface Silence {
	id: number;
	name: string;
	comment: string;
	target_id: TargetId | null;
	matchers: Record<string, string>;
	schedule: SilenceSchedule;
	enabled: boolean;
	/** True if the window covers the present instant (computed by the server). */
	active_now: boolean;
}

/** Body of `POST /api/alerts/silences`. */
export interface SilencePayload {
	name: string;
	comment?: string | null;
	target_id?: TargetId | null;
	matchers?: Record<string, string>;
	schedule: SilenceSchedule;
	enabled?: boolean | null;
}

// --- Network discovery ------------------------------------------------------

/**
 * Device detected by an SNMP scan, as the UI works with it.
 *
 * The server (`api/discovery.rs`) answers `{ devices, scanned }` with
 * `sysname`, `sysdescr`, `sysobjectid` and `suggested_profile`; `discover()`
 * folds that into this shape. `already_added` is computed client-side.
 */
export interface DiscoveredDevice {
	address: string;
	name: string | null;
	sysname: string | null;
	description: string | null;
	sysobjectid: string | null;
	kind: string | null;
	profile_id: string | null;
	already_added: boolean;
}

/** Result of a scan: the devices found and how many addresses were probed. */
export interface DiscoveryResult {
	devices: DiscoveredDevice[];
	scanned: number | null;
}

// --- Device types (collectors) ----------------------------------------------

/**
 * Setup notes for a device type.
 *
 * They describe what must be done **on the device itself** before it can be
 * monitored: enable SNMP, create an API token... `warning` and `doc_url` may be
 * empty, in which case nothing is displayed.
 */
export interface CollectorSetup {
	title: string;
	steps: string[];
	warning: string;
	doc_url: string;
}

/** Input shapes a collector option can ask for. */
export type CollectorOptionInput = 'text' | 'number' | 'boolean' | 'select';

/**
 * Setting specific to a device type, described by the server.
 *
 * Each option is stored in `Target.tags[key]`, always as a string (`"true"` /
 * `"false"` for a boolean): the target model has no dedicated field, and tags
 * already travel with the target. The form uses it to display a typed field
 * rather than a key / value pair to guess.
 */
export interface CollectorOption {
	/** Key of the tag that carries the value. */
	key: string;
	label: string;
	help: string;
	placeholder: string;
	/** Value applied by the server when the tag is absent. */
	default: string;
	required: boolean;
	input: CollectorOptionInput;
	/** Values offered when `input === 'select'`. */
	choices: string[];
}

/**
 * A device type offered by this server.
 *
 * The list is built entirely from the server response: a later version that
 * adds a type sees it appear without any change here. A type unknown to the
 * interface therefore stays usable, with a neutral presentation (see
 * `normalizeCollector`).
 */
export interface CollectorInfo {
	kind: string;
	label: string;
	summary: string;
	examples: string[];
	credential_types: string[];
	address_hint: string;
	default_port: number;
	setup: CollectorSetup;
	/** Settings specific to this type. Empty for types that have none. */
	options: CollectorOption[];
}

/**
 * Fills in a collector description so that it is always displayable.
 *
 * The server returns empty fields for a type it cannot describe (`label` then
 * being the raw `kind` value). The interface must not display holes for all
 * that: we fill them here, once and for all, rather than multiplying `?.` and
 * `|| ''` in components.
 */
export function normalizeCollector(raw: Partial<CollectorInfo> & { kind: string }): CollectorInfo {
	const setup = raw.setup ?? { title: '', steps: [], warning: '', doc_url: '' };
	const options = Array.isArray(raw.options) ? raw.options : [];
	return {
		kind: raw.kind,
		label: raw.label?.trim() || raw.kind,
		summary: raw.summary?.trim() ?? '',
		examples: Array.isArray(raw.examples) ? raw.examples : [],
		credential_types: Array.isArray(raw.credential_types) ? raw.credential_types : [],
		address_hint: raw.address_hint?.trim() ?? '',
		default_port: typeof raw.default_port === 'number' ? raw.default_port : 0,
		setup: {
			title: setup.title?.trim() ?? '',
			steps: Array.isArray(setup.steps) ? setup.steps.filter((s) => s.trim()) : [],
			warning: setup.warning?.trim() ?? '',
			doc_url: setup.doc_url?.trim() ?? ''
		},
		options: options
			.filter((option) => option && typeof option.key === 'string' && option.key.trim())
			.map(normalizeOption)
	};
}

const OPTION_INPUTS: readonly CollectorOptionInput[] = ['text', 'number', 'boolean', 'select'];

/**
 * Fills in an option so that it is always displayable.
 *
 * An older server may omit fields, or announce an input shape this interface
 * does not know: we then fall back to a text field, which accepts anything the
 * server will be able to read.
 */
function normalizeOption(raw: Partial<CollectorOption> & { key: string }): CollectorOption {
	const input = OPTION_INPUTS.includes(raw.input as CollectorOptionInput)
		? (raw.input as CollectorOptionInput)
		: 'text';
	return {
		key: raw.key.trim(),
		label: raw.label?.trim() || raw.key.trim(),
		help: raw.help?.trim() ?? '',
		placeholder: raw.placeholder?.trim() ?? '',
		default: typeof raw.default === 'string' ? raw.default : '',
		required: Boolean(raw.required),
		input,
		choices: Array.isArray(raw.choices) ? raw.choices.filter((c) => typeof c === 'string') : []
	};
}

// --- Authentication ---------------------------------------------------------

export type Role = 'admin' | 'viewer';

/** How an account signs in: with a local password, or through the identity provider. */
export type AuthMethod = 'password' | 'oidc';

/** An account, as returned by `/api/users` and `/api/auth/me`. Never carries a hash. */
export interface User {
	id: number;
	username: string;
	display_name: string;
	role: Role;
	auth: AuthMethod;
	disabled: boolean;
	created_at: string;
	last_login_at: string | null;
}

/** What the sign-in screen needs to know about single sign-on, without a session. */
export interface OidcStatus {
	enabled: boolean;
	/** Button label ("Continue with …"). */
	provider_name: string;
	/** Plain navigation target, not an API call. */
	login_url: string;
}

/** Raw response of `GET /api/auth/status`. */
export interface AuthStatus {
	/** True as soon as an account exists on the instance. */
	configured: boolean;
	/** True if the current session is valid. */
	authenticated: boolean;
	/** The signed-in account, when `authenticated`. */
	user?: User;
	oidc?: OidcStatus;
}

export interface CreateUserPayload {
	username: string;
	display_name?: string;
	role: Role;
	/** Optional only while single sign-on is enabled. */
	password?: string;
}

export interface UpdateUserPayload {
	display_name?: string;
	role?: Role;
	disabled?: boolean;
	/** Resets the password and closes the account's sessions. */
	password?: string;
}

/** Where the effective OIDC configuration comes from. */
export type OidcSource = 'settings' | 'env' | 'none';

/** `GET /api/auth/oidc/config` — the client secret never comes back. */
export interface OidcConfig {
	source: OidcSource;
	enabled: boolean;
	/** True when environment variables describe a provider (used when nothing is saved). */
	env_available: boolean;
	issuer: string;
	client_id: string;
	has_client_secret: boolean;
	provider_name: string;
	scopes: string;
	auto_create: boolean;
	admin_groups: string[];
	groups_claim: string;
	public_url: string;
	/** Computed from `public_url` (or the request origin): register it at the provider. */
	redirect_uri: string;
}

/** `PUT /api/auth/oidc/config` — an empty `client_secret` keeps the stored one. */
export interface OidcConfigPayload {
	issuer: string;
	client_id: string;
	client_secret?: string;
	provider_name: string;
	scopes: string;
	auto_create: boolean;
	admin_groups: string[];
	groups_claim: string;
	public_url: string;
}

/** Endpoints found by `POST /api/auth/oidc/test` (discovery only, no sign-in). */
export interface OidcDiscovery {
	issuer: string;
	authorization_endpoint: string;
	token_endpoint: string;
	jwks_uri: string;
	userinfo_endpoint?: string | null;
	end_session_endpoint?: string | null;
	scopes_supported?: string[] | null;
	id_token_signing_alg_values_supported?: string[] | null;
	code_challenge_methods_supported?: string[] | null;
}

export interface OidcTestReport {
	ok: boolean;
	error?: string;
	discovery?: OidcDiscovery;
}

/**
 * Authentication state as handled by the interface.
 *
 * `available` is not returned by the server: it is `false` when the auth route
 * does not exist yet (404). The instance is then considered unprotected, and
 * the interface stays fully usable.
 */
export interface AuthState {
	available: boolean;
	configured: boolean;
	authenticated: boolean;
	user: User | null;
	oidc: OidcStatus;
}

// --- Notification channels --------------------------------------------------

/** A form field described by the server (`GET /api/notify/kinds`). */
export interface ChannelField {
	/** Key in `settings` or `secrets`. */
	key: string;
	label: string;
	required: boolean;
	input: 'text' | 'url' | 'number' | 'boolean' | 'select' | 'textarea' | 'password';
	help: string;
	placeholder: string;
	/** Values offered when `input === 'select'`. */
	options: string[];
	/**
	 * How the value is stored: `list` is one entry per line (recipients),
	 * `object` is a JSON object (headers, extra data). Scalar otherwise.
	 */
	shape: 'scalar' | 'list' | 'object';
	/** Value applied by the server when the field is left empty. */
	default: string;
}

/** A channel type (Discord, Telegram...) and the fields it expects. */
export interface ChannelKindInfo {
	kind: string;
	label: string;
	summary: string;
	/** Documentation link, for example `https://dumbmonit.readthedocs.io/en/latest/notifications/#discord`. */
	doc_url: string;
	settings: ChannelField[];
	secrets: ChannelField[];
}

/** A channel as returned on read: never its secrets. */
export interface Channel {
	id: number;
	name: string;
	kind: string;
	enabled: boolean;
	settings: Record<string, unknown>;
	has_secret: boolean;
	last_error: string | null;
	last_sent_at: string | null;
	policy: ChannelPolicy;
}

/**
 * Body of `POST /api/notify/channels` and `PUT /api/notify/channels/{id}`.
 *
 * Omitting `secrets` on an update keeps the stored secrets; `{}` explicitly
 * clears them.
 */
export interface ChannelPayload {
	name: string;
	kind: string;
	enabled?: boolean;
	settings?: Record<string, unknown>;
	secrets?: Record<string, unknown>;
	/** Omitted on an update keeps the stored policy; fields absent keep their value. */
	policy?: ChannelPolicyPayload;
}

export interface ChannelTestReport {
	ok: boolean;
	message: string;
}

// --- Agents (enrolment tokens) ----------------------------------------------

export interface AgentToken {
	id: number;
	name: string;
	/** Start of the token, to identify it in a list. */
	prefix: string;
	created_at: string;
	last_used_at: string | null;
	revoked_at: string | null;
}

/** Response of `POST /api/agent/tokens`: the only chance to see the token in clear. */
export interface CreatedAgentToken extends AgentToken {
	secret: string;
	install_linux: string;
	install_windows: string;
}

// --- API tokens (assistants, MCP) -------------------------------------------

/** What a token may do. `read` can never change anything. */
export type ApiTokenScope = 'read' | 'write';

export interface ApiToken {
	id: number;
	name: string;
	/** Start of the token (`dmt_` + a few characters), to identify it in a list. */
	prefix: string;
	scope: ApiTokenScope;
	created_at: string;
	last_used_at: string | null;
	revoked_at: string | null;
}

/** Response of `POST /api/tokens`: the only chance to see the token in clear. */
export interface CreatedApiToken extends ApiToken {
	secret: string;
}

// --- Notification policy ----------------------------------------------------

/** Weekly quiet-hours window, same shape as a weekly maintenance schedule. */
export type QuietHours = Extract<SilenceSchedule, { kind: 'weekly' }>;

/** Per-channel policy, mirroring the server's `ChannelPolicy`. */
export interface ChannelPolicy {
	/** Alerts below this severity never reach the channel. */
	min_severity: AlertSeverity;
	/** False: the channel only hears about problems, never about recoveries. */
	notify_resolved: boolean;
	/** Minimum seconds between two messages about the same alert; 0 = none. */
	min_interval_secs: number;
	/** During quiet hours only `critical` goes through; the rest waits for a digest. */
	quiet_hours: QuietHours | null;
}

/** Body of the `policy` field on a channel: every field optional. */
export interface ChannelPolicyPayload {
	min_severity?: AlertSeverity | null;
	notify_resolved?: boolean | null;
	min_interval_secs?: number | null;
	/** `null` clears the quiet hours; omitted keeps them. */
	quiet_hours?: QuietHours | null;
}

/** Global policy, `GET/PUT /api/notify/policy`. */
export interface NotificationPolicy {
	/** Alerts firing within this window leave as one message per channel; 0 = immediately. */
	batch_window_secs: number;
	/** Messages per channel per hour; 0 = unlimited. */
	max_per_hour: number;
	/** State changes in `flap_window_secs` after which an alert is held; 0 = off. */
	flap_events: number;
	flap_window_secs: number;
	flap_hold_secs: number;
	/** Public URL used for device links in messages; empty falls back to DUMBMONIT_PUBLIC_URL. */
	public_url: string;
}

export type NotificationPolicyPayload = Partial<NotificationPolicy>;

/** A rule's per-device override; a `null` field keeps the rule's value. */
export interface RuleOverride {
	rule_uid: string;
	target_id: TargetId;
	threshold: number | null;
	clear_threshold: number | null;
	enabled: boolean | null;
}

export interface RuleOverridePayload {
	threshold?: number | null;
	clear_threshold?: number | null;
	enabled?: boolean | null;
}

// --- Status pages -----------------------------------------------------------

export type StatusPageTheme = 'auto' | 'light' | 'dark';

/** A service shown on a status page, as stored (admin view). */
export interface StatusPageItem {
	id: number;
	page_id: number;
	target_id: TargetId;
	/** Public name of the service. */
	label: string;
	/** Empty string means "no group". */
	group_name: string;
	position: number;
}

export interface StatusPage {
	id: number;
	slug: string;
	title: string;
	description: string;
	published: boolean;
	theme: StatusPageTheme;
	show_uptime_days: number;
	created_at: string;
	updated_at: string;
	items: StatusPageItem[];
}

export interface StatusPagePayload {
	title: string;
	/** Omit to derive it from the title. */
	slug?: string;
	description?: string;
	published?: boolean;
	theme?: StatusPageTheme;
	show_uptime_days?: number;
}

export interface StatusPageItemPayload {
	target_id: TargetId;
	/** Empty falls back to the device name. */
	label?: string;
	group_name?: string;
}

export type IncidentKind = 'incident' | 'maintenance';
export type IncidentSeverity = 'minor' | 'major';
export type IncidentStatus =
	| 'investigating'
	| 'identified'
	| 'monitoring'
	| 'resolved'
	| 'scheduled'
	| 'in_progress'
	| 'completed';

export interface IncidentUpdate {
	id: number;
	incident_id: number;
	status: IncidentStatus;
	body: string;
	created_at: string;
}

export interface Incident {
	id: number;
	/** `null`: shown on every page. */
	page_id: number | null;
	title: string;
	kind: IncidentKind;
	status: IncidentStatus;
	severity: IncidentSeverity;
	starts_at: string;
	ends_at: string | null;
	created_at: string;
	updated_at: string;
	updates: IncidentUpdate[];
}

export interface IncidentPayload {
	title: string;
	kind?: IncidentKind;
	status?: IncidentStatus;
	severity?: IncidentSeverity;
	page_id?: number | null;
	starts_at?: string;
	ends_at?: string;
	/** First message of the timeline (creation only). */
	body?: string;
}

export interface IncidentUpdatePayload {
	status?: IncidentStatus;
	body: string;
}

// Public document of `GET /api/public/status/{slug}`: no ids, no addresses.

export type PublicOverall = 'operational' | 'degraded' | 'major' | 'maintenance';
export type PublicItemState = 'up' | 'down' | 'degraded' | 'maintenance' | 'unknown';

export interface PublicDayBucket {
	/** `YYYY-MM-DD`, UTC. */
	date: string;
	/** `null` without any measurement that day. */
	uptime_pct: number | null;
	incidents: number;
}

export interface PublicStatusItem {
	label: string;
	state: PublicItemState;
	uptime_24h: number | null;
	uptime_7d: number | null;
	uptime_90d: number | null;
	latency_ms: number | null;
	history: PublicDayBucket[];
}

export interface PublicStatusGroup {
	name: string;
	items: PublicStatusItem[];
}

export interface PublicIncidentUpdate {
	status: IncidentStatus;
	body: string;
	created_at: string;
}

export interface PublicIncident {
	title: string;
	kind: IncidentKind;
	status: IncidentStatus;
	severity: IncidentSeverity;
	starts_at: string;
	ends_at: string | null;
	updates: PublicIncidentUpdate[];
}

export interface PublicStatus {
	page: {
		slug: string;
		title: string;
		description: string;
		theme: StatusPageTheme;
		show_uptime_days: number;
		updated_at: string;
	};
	overall: PublicOverall;
	generated_at: string;
	groups: PublicStatusGroup[];
	/** Open incidents and those of the last 30 days. */
	incidents: PublicIncident[];
	/** Scheduled or in-progress maintenance windows. */
	maintenance: PublicIncident[];
}
