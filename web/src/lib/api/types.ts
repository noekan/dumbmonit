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
	/**
	 * A Proxmox VE / PBS token in the two pieces the product shows: the server
	 * assembles `token_id=secret` and stores the combined form.
	 */
	| { type: 'api_token'; token_id: string; secret: string }
	| { type: 'username_password'; username: string; password: string }
	/** A family this build does not know: sent as the server described it. */
	| ({ type: string } & Record<string, string>);

/** Credential families this build knows how to label, in display order. */
export const CREDENTIAL_KINDS = [
	{ value: 'snmp_community', label: 'SNMP v1 / v2c (community)' },
	{ value: 'snmp_v3', label: 'SNMP v3' },
	{ value: 'api_token', label: 'API token' },
	{ value: 'username_password', label: 'Username / password' },
	{ value: 'none', label: 'No authentication' }
] as const;

export type CredentialKind = (typeof CREDENTIAL_KINDS)[number]['value'];

/** Input shapes a credential field can ask for. */
export type CredentialFieldInput = 'text' | 'password' | 'select';

/**
 * One field of a credential form, described by the server.
 *
 * `key` is the property sent in the `credential` object; the server knows how
 * to recompose what it needs from those keys (a Proxmox token from `token_id`
 * + `secret`, an SNMP v3 user from its protocols and passphrases).
 */
export interface CredentialField {
	key: string;
	label: string;
	help: string;
	placeholder: string;
	input: CredentialFieldInput;
	/** Values offered when `input === 'select'`, the first one by default. */
	choices: string[];
	required: boolean;
}

/** A credential family a device type accepts, with the fields to fill in. */
export interface CredentialView {
	/** Value of `credential.type`. */
	kind: string;
	label: string;
	help: string;
	fields: CredentialField[];
}

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
	/** Agent that probes this device from its own network; `null` when the server does. */
	via_agent: TargetId | null;
	interval_secs: number;
	enabled: boolean;
	tags: Record<string, string>;
	credential_kind: string;
	last_probe_at: string | null;
	last_error: string | null;
	/** What `last_error` means: the device is `down`, or our `config` is wrong. */
	error_kind: 'down' | 'config' | null;
}

/** Body sent to `POST /api/targets` and `PUT /api/targets/{id}`. */
export interface TargetPayload {
	name: string;
	address: string;
	kind: string;
	profile_id?: string | null;
	parent_id?: TargetId | null;
	via_agent?: TargetId | null;
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
	/**
	 * True while someone has acknowledged this alert: reminders and escalations
	 * pause until `acked_until`, the resolution is still notified. The phase
	 * does not move — an acked alert is still a problem, just a known one.
	 */
	acked: boolean;
	acked_until: string | null;
	/** Account that acknowledged, or `token:name` for an API token. */
	acked_by: string | null;
	ack_note: string | null;
	value: number | null;
	score: number | null;
	condition_since: string | null;
	firing_since: string | null;
	last_eval_at: string | null;
	last_notified_at: string | null;
	notify_count: number;
}

/**
 * Body of `POST /api/alerts/{fingerprint}/ack`. One of `until` (RFC 3339) or
 * `duration_secs`; neither means four hours. `until: null` lifts the
 * acknowledgement, like `DELETE`.
 */
export interface AckPayload {
	until?: string | null;
	duration_secs?: number;
	note?: string | null;
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

/** "First Sunday", "last Friday": `nth` is 1…5, or -1 for the last one. */
export interface NthWeekday {
	nth: number;
	/** 0 = Monday … 6 = Sunday. */
	weekday: number;
}

/** Maintenance-window schedule: one-off, weekly or monthly. */
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
			/** IANA time zone ("Europe/Paris"). Wins over the fixed offset: only a
			 *  named zone knows where the daylight-saving changes fall. */
			timezone?: string | null;
	  }
	| {
			kind: 'monthly';
			/** Days of the month, 1…31. A month without that day is skipped. */
			days: number[];
			/** "First Sunday", "last Friday"… Added to `days` (OR). */
			nth_weekdays: NthWeekday[];
			/** Minutes since local midnight, 0…1439. */
			start_minute: number;
			/** How long the window lasts, in real minutes. */
			duration_minutes: number;
			utc_offset_minutes: number;
			timezone?: string | null;
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
	/** End of the occurrence in progress, when there is one. */
	active_until: string | null;
	/** Start of the next occurrence; the server unrolls the calendar. */
	next_start_at: string | null;
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
	/** The same families as `credential_types`, field by field, in order. */
	credentials: CredentialView[];
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
	const credential_types = Array.isArray(raw.credential_types)
		? raw.credential_types.filter((t) => typeof t === 'string')
		: [];
	// A server predating field descriptions only names the families: rebuild
	// the fields it would have sent, so the form looks the same against both.
	const credentials = Array.isArray(raw.credentials)
		? raw.credentials
				.filter((c) => c && typeof c.kind === 'string' && c.kind.trim())
				.map(normalizeCredentialView)
		: credential_types.map(fallbackCredentialView);
	return {
		kind: raw.kind,
		label: raw.label?.trim() || raw.kind,
		summary: raw.summary?.trim() ?? '',
		examples: Array.isArray(raw.examples) ? raw.examples : [],
		credential_types,
		credentials,
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

const CREDENTIAL_INPUTS: readonly CredentialFieldInput[] = ['text', 'password', 'select'];

function normalizeCredentialView(raw: Partial<CredentialView> & { kind: string }): CredentialView {
	const fields = Array.isArray(raw.fields) ? raw.fields : [];
	return {
		kind: raw.kind.trim(),
		label: raw.label?.trim() || fallbackCredentialView(raw.kind.trim()).label,
		help: raw.help?.trim() ?? '',
		fields: fields
			.filter((field) => field && typeof field.key === 'string' && field.key.trim())
			.map((field) => {
				const input = CREDENTIAL_INPUTS.includes(field.input as CredentialFieldInput)
					? (field.input as CredentialFieldInput)
					: 'text';
				return {
					key: field.key.trim(),
					label: field.label?.trim() || field.key.trim(),
					help: field.help?.trim() ?? '',
					placeholder: field.placeholder?.trim() ?? '',
					input,
					choices: Array.isArray(field.choices) ? field.choices.filter((c) => typeof c === 'string') : [],
					required: Boolean(field.required)
				};
			})
	};
}

function credentialField(
	key: string,
	label: string,
	input: CredentialFieldInput,
	required: boolean,
	extra: Partial<CredentialField> = {}
): CredentialField {
	return { key, label, help: '', placeholder: '', input, choices: [], required, ...extra };
}

/** The fields a family asks for, when the server does not say (older builds). */
function fallbackCredentialView(kind: string): CredentialView {
	const label = CREDENTIAL_KINDS.find((option) => option.value === kind)?.label ?? kind;
	switch (kind) {
		case 'snmp_community':
			return {
				kind,
				label,
				help: '',
				fields: [
					credentialField('community', 'SNMP community', 'password', true, {
						help: 'Most devices ship with "public". A read-only community is enough.',
						placeholder: 'public'
					})
				]
			};
		case 'snmp_v3':
			return {
				kind,
				label,
				help: '',
				fields: [
					credentialField('username', 'User name', 'text', true),
					credentialField('auth_protocol', 'Authentication protocol', 'select', false, {
						choices: ['sha256', 'sha512', 'sha384', 'sha224', 'sha1', 'md5']
					}),
					credentialField('auth_passphrase', 'Authentication passphrase', 'password', true),
					credentialField('privacy_protocol', 'Privacy protocol', 'select', false, {
						choices: ['aes128', 'aes192', 'aes256', 'des']
					}),
					credentialField('privacy_passphrase', 'Privacy passphrase', 'password', false, {
						help: 'Leave blank for authentication without encryption (authNoPriv).'
					})
				]
			};
		case 'api_token':
			return {
				kind,
				label,
				help: '',
				fields: [
					credentialField('token', 'API token', 'password', true, {
						help: 'A read-only token is enough. The server stores it encrypted and never shows it again.'
					})
				]
			};
		case 'username_password':
			return {
				kind,
				label,
				help: '',
				fields: [
					credentialField('username', 'User name', 'text', true),
					credentialField('password', 'Password', 'password', true)
				]
			};
		default:
			return { kind, label, help: '', fields: [] };
	}
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
	/** Two-factor authentication is active on this account. */
	totp_enabled: boolean;
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
	/** How many machines this token may enrol in total. `null` means a fleet token, with no limit. */
	max_uses: number | null;
	/** How many machines it has already enrolled. */
	uses: number;
	/**
	 * When it stops enrolling. Machines already enrolled keep reporting after
	 * this date — only revoking cuts them off.
	 */
	expires_at: string | null;
}

/** Body of `POST /api/agent/tokens`. Omitting everything gives a single-use token. */
export interface AgentTokenPayload {
	name: string;
	base_url: string;
	/** Reusable for a fleet. Left out, the token enrols exactly one machine. */
	reusable?: boolean;
	/** Enrolments allowed for a reusable token. Left out, there is no limit. */
	max_uses?: number | null;
	/** Days before the token stops enrolling. Left out, there is no deadline. */
	expires_in_days?: number | null;
}

/** Response of `POST /api/agent/tokens`: the only chance to see the token in clear. */
export interface CreatedAgentToken extends AgentToken {
	secret: string;
	install_linux: string;
	install_windows: string;
}

/**
 * `GET /api/targets/{id}/agent`: the machine as its agent last described it.
 * Mirrors `AgentHostView` in `crates/server/src/api/agent_commands.rs`.
 */
export interface AgentHost {
	hostname: string;
	os: string;
	os_version: string | null;
	arch: string | null;
	agent_version: string;
	/**
	 * True only when the agent said it runs commands. An agent older than the
	 * command channel, or configured with `commands: false`, never picks up a
	 * Restart/Update — the UI must not offer them.
	 */
	commands_supported: boolean;
	/** True when the agent declared `relay: true`: it runs probes for other devices. */
	relay: boolean;
	/** Site declared by the agent, if any. */
	site: string | null;
	/** Devices reached through this agent (`via_agent`). */
	relayed: number;
	last_seen_at: string | null;
	/**
	 * Where this machine stands on binding:
	 *
	 * - `bound` — the agent holds a secret of its own, so no other machine can
	 *   push in its name or take its container commands;
	 * - `pending` — the binary knows how to be bound; its next batch will do it;
	 * - `unsupported` — an agent older than binding: reinstall it.
	 */
	binding: 'bound' | 'pending' | 'unsupported';
	/** True for `bound`. */
	bound: boolean;
	bound_at: string | null;
	/** End of a re-enrolment window someone opened, while it still runs. */
	rebind_until: string | null;
}

/** Response of `POST /api/targets/{id}/agent/rebind`. */
export interface RebindWindow {
	rebind_until: string;
	minutes: number;
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

/**
 * One condition of a channel's routing filter.
 *
 * Conditions on the same field (the same tag key, or `kind`, or `rule`) read as
 * OR; different fields read as AND. Exclusions always win.
 */
export type MatchCondition =
	| { field: 'tag'; key: string; value: string }
	| { field: 'kind'; value: string }
	| { field: 'rule'; value: string };

/** What a channel accepts. Both lists empty: it receives everything. */
export interface ChannelMatcher {
	include: MatchCondition[];
	exclude: MatchCondition[];
}

/** One device as `POST /api/notify/match-preview` returns it. */
export interface MatchPreviewDevice {
	id: TargetId;
	name: string;
	kind: string;
	tags: Record<string, string>;
	matched: boolean;
}

/** Response of `POST /api/notify/match-preview`. */
export interface MatchPreview {
	devices: MatchPreviewDevice[];
	matched: number;
	total: number;
	/** Rule uids the filter requires; a device list carries no rule, so the UI
	 *  says them out loud instead of pretending they are ignored. */
	rules: string[];
	excluded_rules: string[];
}

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
	/** What the channel accepts. Empty lists: everything, as before. */
	matcher: ChannelMatcher;
}

/** Body of the `policy` field on a channel: every field optional. */
export interface ChannelPolicyPayload {
	min_severity?: AlertSeverity | null;
	notify_resolved?: boolean | null;
	min_interval_secs?: number | null;
	/** `null` clears the quiet hours; omitted keeps them. */
	quiet_hours?: QuietHours | null;
	/** `null` clears the routing filter; omitted keeps it. */
	matcher?: ChannelMatcher | null;
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
	/** Seconds without an acknowledgement after which the alert is also sent to
	 *  `escalate_channel`; 0 = no escalation. */
	escalate_after_secs: number;
	/** Channel told second. `null` = no escalation, whatever the delay. */
	escalate_channel: number | null;
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

// --- Proxmox Backup Server (crates/server/src/api/pbs.rs) --------------------
//
// Times are Unix seconds, as PBS reports them (not server date strings).

export interface PbsSnapshot {
	time: number;
	size: number | null;
	/** `true` verified, `false` verification failed, `null` never verified. */
	verified: boolean | null;
	protected: boolean;
}

export type PbsDayState = 'ok' | 'verify_failed' | 'failed' | 'running' | 'none';

export interface PbsCalendarRun {
	upid: string;
	start: number;
	end: number | null;
	/** `null` while the task is still running. */
	ok: boolean | null;
	status: string | null;
}

export interface PbsCalendarDay {
	/** `YYYY-MM-DD` in the timezone the calendar was asked for. */
	date: string;
	state: PbsDayState;
	/** Backup tasks of that day, newest first. */
	runs: PbsCalendarRun[];
	snapshot: PbsSnapshot | null;
}

export interface PbsCalendarFailure {
	time: number;
	upid: string;
	error: string;
}

export interface PbsCalendarGroup {
	datastore: string;
	namespace: string;
	backup_type: string;
	backup_id: string;
	/** Guest name from the latest snapshot's notes, when PVE wrote one. */
	name: string | null;
	count: number;
	last_time: number | null;
	last_size: number | null;
	last_verified: boolean | null;
	last_success: number | null;
	last_failure: PbsCalendarFailure | null;
	retention: string | null;
	/** One entry per day, oldest first, today last. */
	days: PbsCalendarDay[];
}

export interface PbsCalendar {
	/** `null` before the first successful probe. */
	probed_at: number | null;
	days: number;
	offset_minutes: number;
	groups: PbsCalendarGroup[];
}

export type PbsTaskKind = 'backup' | 'sync' | 'verify' | 'prune' | 'gc' | 'other';

export interface PbsFailure {
	upid: string;
	worker_type: string;
	kind: PbsTaskKind;
	worker_id: string;
	datastore: string | null;
	object: string | null;
	user: string | null;
	start: number;
	end: number | null;
	/** Error message without the `TASK ERROR:` prefix. */
	error: string;
}

export interface PbsJob {
	/** `sync`, `verify`, `prune` or `gc`. */
	kind: string;
	id: string;
	datastore: string;
	namespace: string | null;
	/** Sync jobs only: `remote:remote-store`, or `local`. */
	remote: string | null;
	enabled: boolean;
	schedule: string | null;
	comment: string | null;
	/** Prune jobs only: `last 3, daily 7, weekly 4`. */
	retention: string | null;
	next_run: number | null;
	last_run_state: string | null;
	last_run_end: number | null;
	last_run_upid: string | null;
	/** `null` when the job never ran. */
	last_run_ok: boolean | null;
	error: string | null;
}

export interface PbsJobs {
	probed_at: number | null;
	jobs: PbsJob[];
}

export interface PbsGc {
	last_run_state: string | null;
	last_run_end: number | null;
	last_run_upid: string | null;
	schedule: string | null;
	next_run: number | null;
	removed_bytes: number | null;
	pending_bytes: number | null;
	duration_seconds: number | null;
	disk_chunks: number | null;
	pending_chunks: number | null;
	removed_chunks: number | null;
	/** Chunks the GC could not read and left in place: corruption, not space. */
	bad_chunks: number | null;
}

export interface PbsTypeCount {
	/** `vm`, `ct`, `host` or `other`. */
	backup_type: string;
	groups: number;
	snapshots: number;
}

export interface PbsDatastore {
	name: string;
	available: boolean;
	error: string | null;
	total_bytes: number | null;
	used_bytes: number | null;
	avail_bytes: number | null;
	/** PBS's own fill-up estimate, only when it lies in the future. */
	estimated_full_at: number | null;
	dedup_factor: number | null;
	gc: PbsGc | null;
	/** `nonremovable`, `mounted`, `notmounted`, `unknown`. */
	mount_status: string | null;
	/** `filesystem` or `s3`. */
	backend: string | null;
	/** Maintenance type in force (`offline`, `read-only`…), if any. */
	maintenance: string | null;
	counts: PbsTypeCount[];
	/** Measured growth from PBS's own history; negative when pruning wins. */
	growth_bytes_per_day: number | null;
	/** Days of history the growth and PBS's forecast are computed over. */
	history_days: number | null;
	active_reads: number | null;
	active_writes: number | null;
}

export interface PbsService {
	service: string;
	description: string | null;
	/** `running`, `dead`, `failed`… */
	state: string | null;
	/** `enabled`, `disabled`, `static`, `masked`… */
	unit_state: string | null;
	running: boolean;
	/** `null` for a unit pulled in by another one, which is neither. */
	enabled: boolean | null;
}

export interface PbsPackage {
	package: string;
	title: string | null;
	installed: string | null;
	available: string | null;
	/** Version actually running, for the two packages that report one. */
	running: string | null;
	upgradable: boolean;
	/** `true` when the upgraded package is installed but not yet running. */
	restart_pending: boolean | null;
}

export interface PbsCertificate {
	filename: string;
	subject: string | null;
	issuer: string | null;
	fingerprint: string | null;
	not_after: number | null;
	san: string[];
}

export interface PbsTrafficRule {
	name: string;
	comment: string | null;
	networks: string[];
	timeframe: string[];
	limit_in_bytes: number | null;
	limit_out_bytes: number | null;
	rate_in_bytes: number | null;
	rate_out_bytes: number | null;
}

export interface PbsTapeJob {
	id: string;
	datastore: string;
	namespace: string | null;
	pool: string | null;
	drive: string | null;
	comment: string | null;
	schedule: string | null;
	next_run: number | null;
	next_media_label: string | null;
	last_run_state: string | null;
	last_run_end: number | null;
	last_run_upid: string | null;
}

export interface PbsTapeDrive {
	name: string;
	path: string | null;
	vendor: string | null;
	model: string | null;
	serial: string | null;
	changer: string | null;
	state: string | null;
}

export interface PbsTapeChanger {
	name: string;
	path: string | null;
	vendor: string | null;
	model: string | null;
	serial: string | null;
	export_slots: string | null;
}

export interface PbsMediaPool {
	name: string;
	allocation: string | null;
	retention: string | null;
	comment: string | null;
	encrypted: boolean;
	media_total: number;
	media_expired: number;
	bytes_used: number | null;
}

export interface PbsTapeMedia {
	label: string;
	pool: string | null;
	/** `full`, `writable`, `unknown`, `damaged`, `retired`. */
	status: string | null;
	location: string | null;
	media_set: string | null;
	expired: boolean;
	bytes_used: number | null;
}

export interface PbsTape {
	jobs: PbsTapeJob[];
	drives: PbsTapeDrive[];
	changers: PbsTapeChanger[];
	pools: PbsMediaPool[];
	media: PbsTapeMedia[];
}

export interface PbsDisk {
	name: string;
	devpath: string | null;
	model: string | null;
	serial: string | null;
	size_bytes: number | null;
	disk_type: string | null;
	used: string | null;
	/** `passed`, `failed` or `unknown`. */
	status: string | null;
	/** Endurance used, in percent (SSD only). */
	wearout_percent: number | null;
}

export interface PbsZpool {
	name: string;
	health: string;
	size_bytes: number | null;
	alloc_bytes: number | null;
	free_bytes: number | null;
	fragmentation_percent: number | null;
}

export interface PbsHealth {
	probed_at: number | null;
	version: string | null;
	datastores: PbsDatastore[];
	disks: PbsDisk[];
	zpools: PbsZpool[];
	services: PbsService[];
	packages: PbsPackage[];
	certificates: PbsCertificate[];
	traffic: PbsTrafficRule[];
	/** `null` when the server has no tape tier. */
	tape: PbsTape | null;
}

export interface PbsTaskLog {
	upid: string;
	total: number;
	lines: string[];
}

export interface PbsSmartAttribute {
	id: number | null;
	name: string | null;
	raw: string | null;
	normalized: number | null;
	threshold: number | null;
	worst: number | null;
	flags: string | null;
}

export interface PbsDiskSmart {
	disk: string;
	health: string | null;
	wearout_percent: number | null;
	kind: string | null;
	attributes: PbsSmartAttribute[];
	text: string | null;
}

// --- Relay agents (remote sites) --------------------------------------------

/**
 * `GET /api/relays`: an agent as the device form offers it under "Reached
 * through". Mirrors `RelayView` in `crates/server/src/api/relay.rs`.
 */
export interface RelayAgent {
	/** Id of the agent's target. */
	id: TargetId;
	name: string;
	site: string | null;
	/**
	 * True when the agent declared `relay: true` at its last batch. An agent
	 * picked as relay without it never fetches the probes: the UI warns.
	 */
	relay: boolean;
	last_seen_at: string | null;
	/** Devices reached through this agent. */
	relayed: number;
}

// --- Two-factor authentication (TOTP) and audit log ---------------------------

/** Outcome of `POST /auth/login`: either the session is open, or a second step is needed. */
export interface LoginOutcome {
	totp_required: boolean;
	/** Short-lived token to present with the code on `/auth/login/totp`. */
	pending: string | null;
}

/** Mirrors `api::totp::TotpStatus`. */
export interface TotpStatus {
	enabled: boolean;
	pending: boolean;
	recovery_codes_left: number;
}

/** Mirrors `api::totp::Enrolment`. */
export interface TotpEnrolment {
	secret: string;
	otpauth_uri: string;
	issuer: string;
	account: string;
}

/** Mirrors `auth::audit::Entry`. */
export interface AuditEntry {
	id: number;
	at: string;
	actor: string | null;
	action: string;
	subject: string | null;
	ip: string | null;
}

// --- Proxmox VE guests (`crates/server/src/api/proxmox.rs`) -----------------

export type ProxmoxGuestStatus = 'running' | 'stopped' | 'paused' | 'suspended' | 'template' | 'unknown';

/**
 * One row of the Guests panel, as the last probe left it. Every unknown value
 * is `null`, never zero: a VM without guest agent has a disk size but no
 * usage; a stopped guest keeps its sizes and loses its measurements.
 */
export interface ProxmoxGuest {
	vmid: number;
	name: string;
	node: string;
	kind: 'qemu' | 'lxc' | string;
	status: ProxmoxGuestStatus | string;
	/** Of the cores allocated to the guest. */
	cpu_percent: number | null;
	cpu_count: number | null;
	memory_used_bytes: number | null;
	memory_total_bytes: number | null;
	memory_percent: number | null;
	/**
	 * Memory the guest's process occupies on the host: what the guest sees plus
	 * the emulation overhead. It regularly exceeds the allocated memory.
	 */
	memory_host_bytes: number | null;
	balloon_bytes: number | null;
	disk_used_bytes: number | null;
	disk_total_bytes: number | null;
	disk_percent: number | null;
	/** `true` guest agent answered, `false` enabled but silent, `null` none (or a container). */
	agent: boolean | null;
	network_in_bps: number | null;
	network_out_bps: number | null;
	disk_read_bps: number | null;
	disk_write_bps: number | null;
	uptime_seconds: number | null;
	last_backup_age_seconds: number | null;
	ha_state: string | null;
	/** Cluster pool the guest belongs to, `null` when it is in none. */
	pool: string | null;
	/** Lock held right now (`backup`, `migrate`, …); left on, it blocks every operation. */
	lock: string | null;
	/** Operating system seen from inside, `null` without a guest agent. */
	os: string | null;
	/** First routable address reported from inside, `null` when none is known. */
	ip: string | null;
}

/** One LVM thin pool of a node: a full one puts every guest on it read-only. */
export interface ProxmoxThinPool {
	name: string;
	vg: string;
	used_percent: number | null;
	/** The metadata volume is far smaller and often fills first. */
	metadata_used_percent: number | null;
	size_bytes: number | null;
}

export interface ProxmoxVolumeGroup {
	name: string;
	used_percent: number | null;
	size_bytes: number | null;
}

/** One node of the cluster: is it there, does it hold up, and what is wrong with it. */
export interface ProxmoxNode {
	name: string;
	up: boolean;
	cpu_percent: number | null;
	memory_percent: number | null;
	rootfs_percent: number | null;
	uptime_seconds: number | null;
	version: string | null;
	/** Share of CPU time spent waiting on storage. */
	cpu_iowait_percent: number | null;
	/** Memory reclaimed by KSM by sharing identical pages between guests. */
	ksm_shared_bytes: number | null;
	/** Proxmox packages for which a newer version is available. */
	packages_upgradable: number | null;
	/** A newer kernel than the running one is installed: the node awaits a reboot. */
	reboot_required: boolean | null;
	kernel_running: string | null;
	kernel_installed: string | null;
	/** Core Proxmox daemons that are not running on this node. */
	services_down: string[];
	/** Interfaces set to start at boot that are not up. */
	interfaces_offline: string[];
	thin_pools: ProxmoxThinPool[];
	volume_groups: ProxmoxVolumeGroup[];
}

/** The cluster's nodes, plus what belongs to the cluster rather than to a node. */
export interface ProxmoxNodes {
	nodes: ProxmoxNode[];
	/** Proxmox's own word for the HA watchdog: `armed`, `standby`… `null` before PVE 9. */
	fencing_state: string | null;
	/** `true` when the watchdog will really fence a lost node. */
	fencing_armed: boolean | null;
}

export interface ProxmoxCephOsd {
	name: string;
	host: string;
	device_class: string;
	up: boolean;
	in: boolean;
	used_percent: number | null;
	used_bytes: number | null;
	total_bytes: number | null;
	apply_latency_ms: number | null;
	commit_latency_ms: number | null;
}

export interface ProxmoxCephPool {
	name: string;
	used_percent: number | null;
	used_bytes: number | null;
	size: number | null;
	min_size: number | null;
	pg_num: number | null;
	pg_num_optimal: number | null;
	autoscale: string | null;
}

/**
 * Ceph, when there is one. `available` is false both when the cluster has no
 * Ceph and when it has not been probed yet — the panel simply stays hidden.
 */
export interface ProxmoxCeph {
	available: boolean;
	/** 0 OK, 1 WARN, 2 ERR, 3 unknown. */
	health: number | null;
	health_status: string | null;
	bytes_used: number | null;
	bytes_total: number | null;
	used_percent: number | null;
	osds_total: number | null;
	osds_up: number | null;
	osds_in: number | null;
	osds: ProxmoxCephOsd[];
	pools: ProxmoxCephPool[];
	filesystems: string[];
	/** OSD flags left set (`noout`, `norebalance`, …). */
	flags: string[];
	/** Health checks muted: what `HEALTH_OK` no longer tells you. */
	muted_checks: string[];
}

// --- Synology DSM -------------------------------------------------------------
// Mirrors `crates/server/src/api/synology.rs`: the device panel reads the last
// `dumbmonit_synology_*` series and the Active Backup history the probe stored.

export interface SynologySystem {
	model: string | null;
	dsm_version: string | null;
	uptime_seconds: number | null;
	temperature_celsius: number | null;
	temperature_warning: boolean | null;
	cpu_percent: number | null;
	memory_used_bytes: number | null;
	memory_total_bytes: number | null;
	memory_percent: number | null;
	/** Worst state seen: 0 normal, 1 attention, 2 critical. */
	storage_health: number | null;
	system_crashed: boolean | null;
	system_need_repair: boolean | null;
}

export interface SynologyVolume {
	id: string;
	name: string;
	fs_type: string;
	raid_type: string;
	/** DSM's own word: `normal`, `degrade`, `crashed`, `repairing`… */
	status: string;
	/** 0 normal, 1 attention, 2 critical. */
	severity: number;
	total_bytes: number | null;
	used_bytes: number | null;
	used_percent: number | null;
}

export interface SynologyDisk {
	id: string;
	name: string;
	model: string;
	serial: string;
	vendor: string;
	firmware: string;
	kind: string;
	ssd: boolean;
	status: string;
	severity: number;
	smart_status: string;
	smart_severity: number;
	temperature_celsius: number | null;
	size_bytes: number | null;
	bad_sector_exceeded: boolean | null;
	life_below_threshold: boolean | null;
	remaining_life_percent: number | null;
	/** DSM's `unc` counter: unreadable sectors. */
	unc_count: number | null;
}

/** A storage pool or an SSD cache: DSM gives both the same shape. */
export interface SynologyPool {
	id: string;
	name: string;
	/** DSM's own word: `shr_1`, `raid_5`, `basic`… */
	raid_type: string;
	/** DSM's own word: `normal`, `degrade`, `crashed`, `repairing`… */
	status: string;
	/** 0 normal, 1 attention, 2 critical. */
	severity: number;
	/** Disks of the group DSM reports as failed. */
	failed_disks: number | null;
	total_bytes: number | null;
	/** Share already handed to volumes; DSM gives none for an SSD cache. */
	used_bytes: number | null;
}

export interface SynologyOverview {
	system: SynologySystem;
	volumes: SynologyVolume[];
	/** Storage pools: redundancy lives here, not in the volume. */
	pools: SynologyPool[];
	/** SSD caches; empty on a NAS without one. */
	ssd_caches: SynologyPool[];
	disks: SynologyDisk[];
	/** Unix seconds of the newest reading, `null` before the first probe. */
	sampled_at: number | null;
}

export type AbbDeviceState = 'ok' | 'idle' | 'learning' | 'running' | 'overdue' | 'failing' | 'never';

export type AbbDayOutcome = 'success' | 'failure' | 'cancelled' | 'running' | 'none';

export interface AbbDayCell {
	/** Local day, `YYYY-MM-DD`. */
	day: string;
	outcome: AbbDayOutcome;
	runs: number;
}

export interface AbbDevice {
	device_id: number;
	device_name: string;
	task_id: number;
	task_name: string;
	state: AbbDeviceState;
	last_success_s: number | null;
	last_run_s: number | null;
	last_outcome: string | null;
	runs_30d: number;
	successes_30d: number;
	failures_30d: number;
	consecutive_failures: number;
	typical_interval_s: number | null;
	p90_gap_s: number | null;
	/** Active time tolerated since the last success before "overdue". */
	allowance_s: number;
	active_elapsed_s: number | null;
	/** Monday first. */
	active_weekdays: [boolean, boolean, boolean, boolean, boolean, boolean, boolean];
	usual_hour: number | null;
	/** The rhythm in words: "weekdays, around 20:00". */
	rhythm: string;
	/** Thirty cells, oldest first. */
	calendar: AbbDayCell[];
}

export interface AbbTask {
	task_id: string;
	name: string;
	source_type: string;
	result: string;
	/** 1 success, 0 failed, 2 running, -1 unknown. */
	last_status: number | null;
	enabled: boolean | null;
	device_count: number | null;
	last_success_seconds: number | null;
}

export interface SynologyAbb {
	tasks: AbbTask[];
	devices: AbbDevice[];
	/** Offset, in seconds, in which the rhythm's days and hours are expressed (the server's local time). */
	utc_offset_s: number;
	min_allowance_s: number;
	learning_allowance_s: number;
	failing_streak: number;
}

// --- Heartbeat (push) monitors (`crates/server/src/api/push.rs`) -------------

/**
 * `GET /api/targets/{id}/push`: the secret URL a heartbeat device is called
 * on, and what its last call said. Mirrors `PushMonitorView`. The token is
 * returned on purpose: it only lets a caller say "the job ran", and it must be
 * copied into a crontab long after the device was created.
 */
export interface PushMonitor {
	target_id: TargetId;
	token: string;
	/** Path relative to the server: `/api/push/<token>`. The UI prepends its origin. */
	path: string;
	last_seen_at: string | null;
	/** Age of the last call in seconds, `null` until the first one. */
	last_seen_age_secs: number | null;
	/** What the last call declared. */
	last_status: 'up' | 'down';
	last_message: string;
	received_total: number;
	created_at: string;
	/** Effective settings, `null` when the options are unreadable (`settings_error`). */
	expected_interval_secs: number | null;
	grace_secs: number | null;
	settings_error: string | null;
	verdict: 'waiting' | 'on_time' | 'missed' | 'reported_down';
}

// --- Proxmox Datacenter Manager (crates/server/src/api/pdm.rs) --------------
//
// Times are Unix seconds, as PDM reports them (not server date strings).
// Every measurement is nullable: an instance the console cannot reach says
// nothing, and nothing is not zero.

/** Totals the console computes for the whole estate (`/resources/status`). */
export interface PdmEstate {
	remotes: number | null;
	remotes_failed: number | null;
	nodes_online: number | null;
	nodes_offline: number | null;
	qemu_running: number | null;
	qemu_stopped: number | null;
	lxc_running: number | null;
	lxc_stopped: number | null;
	cpu_used_cores: number | null;
	cpu_total_cores: number | null;
	memory_used_bytes: number | null;
	memory_total_bytes: number | null;
	storage_used_bytes: number | null;
	storage_total_bytes: number | null;
	datastores: number | null;
}

/** One federated Proxmox VE cluster or Proxmox Backup Server. */
export interface PdmRemote {
	id: string;
	/** `pve` or `pbs`, when the console says so. */
	kind: string | null;
	reachable: boolean;
	/** What the console got back when it failed, verbatim. */
	error: string | null;
	version: string | null;
	/** Another instance of the same product runs a newer version. */
	version_behind: boolean;
	nodes: string[];
	nodes_online: number | null;
	nodes_offline: number | null;
	guests_running: number | null;
	guests_stopped: number | null;
	cpu_used_cores: number | null;
	cpu_total_cores: number | null;
	memory_used_bytes: number | null;
	memory_total_bytes: number | null;
	storage_used_bytes: number | null;
	storage_total_bytes: number | null;
	datastores: number | null;
	/** `none`, `unknown`, `mixed` or `active`. */
	subscription: string | null;
	last_collection: number | null;
	updates_pending: number | null;
	tasks_failed: number;
	memory_used_percent: number | null;
	storage_used_percent: number | null;
}

export interface PdmRemotes {
	probed_at: number | null;
	version: string | null;
	estate: PdmEstate;
	remotes: PdmRemote[];
}

export type PdmTaskKind =
	| 'backup'
	| 'migrate'
	| 'sync'
	| 'verify'
	| 'prune'
	| 'gc'
	| 'replication'
	| 'update'
	| 'other';

export interface PdmFailure {
	upid: string;
	/** Federated instance the task ran on; empty for a task of the console itself. */
	remote: string;
	worker_type: string;
	kind: PdmTaskKind;
	worker_id: string;
	node: string | null;
	user: string | null;
	start: number;
	end: number | null;
	/** Error message, without the `TASK ERROR:` prefix. */
	error: string;
}

export interface PdmCertificate {
	filename: string;
	subject: string | null;
	issuer: string | null;
	not_after: number | null;
}

export interface PdmSubscription {
	status: string | null;
	message: string | null;
	active_nodes: number | null;
	total_nodes: number | null;
}

export interface PdmNode {
	cpu_percent: number | null;
	cpu_count: number | null;
	cpu_model: string | null;
	load1: number | null;
	memory_used_bytes: number | null;
	memory_total_bytes: number | null;
	swap_used_bytes: number | null;
	swap_total_bytes: number | null;
	rootfs_used_bytes: number | null;
	rootfs_total_bytes: number | null;
	uptime_seconds: number | null;
	kernel: string | null;
	updates_pending: number | null;
	certificates: PdmCertificate[];
	subscription: PdmSubscription | null;
}

export interface PdmHealth {
	probed_at: number | null;
	version: string | null;
	/** `null` when the console host is not read (option off, or missing privilege). */
	node: PdmNode | null;
	memory_used_percent: number | null;
	rootfs_used_percent: number | null;
	/** Soonest expiry first. */
	certificates: PdmCertificate[];
	subscription: PdmSubscription | null;
}

// ---------------------------------------------------------------------------
// Proxmox Mail Gateway (`crates/server/src/api/pmg.rs`)
// ---------------------------------------------------------------------------

export interface PmgQueueDomain {
	domain: string;
	messages: number;
}

export interface PmgQueue {
	/** `incoming`, `active`, `deferred` or `hold`. */
	queue: string;
	messages: number;
	domains: number;
	/**
	 * Lower bound on the age of the oldest message, in seconds: qshape reports
	 * age brackets, so this is the floor of the highest occupied one. `null`
	 * when the queue is empty.
	 */
	oldest_age_seconds: number | null;
	top_domains: PmgQueueDomain[];
	/** True when the oldest message has been waiting for more than four hours. */
	stuck: boolean;
}

export interface PmgQueues {
	probed_at: number | null;
	/** In reading order: incoming, active, deferred, hold. */
	queues: PmgQueue[];
	total_messages: number;
	stuck: boolean;
}

/** Totals since local midnight on the gateway, as PMG aggregates them. */
export interface PmgMail {
	count_in: number | null;
	count_out: number | null;
	bytes_in: number | null;
	bytes_out: number | null;
	spam_in: number | null;
	spam_out: number | null;
	virus_in: number | null;
	virus_out: number | null;
	bounces_in: number | null;
	bounces_out: number | null;
	junk_in: number | null;
	junk_out: number | null;
	greylisted: number | null;
	spf_rejects: number | null;
	rbl_rejects: number | null;
	pregreet_rejects: number | null;
	avg_processing_seconds: number | null;
}

export interface PmgRecentPoint {
	/** Start of the slice, Unix seconds. */
	time: number;
	timespan: number;
	count_in: number;
	count_out: number;
	spam_in: number;
	virus_in: number;
}

export interface PmgSpamScore {
	/** `0` to `10`; `10` aggregates everything above. */
	level: string;
	count: number;
	ratio_percent: number | null;
}

export interface PmgVirus {
	name: string;
	count: number;
}

/** Counts only: no subject, sender or message content ever leaves the gateway. */
export interface PmgQuarantine {
	spam_count: number | null;
	spam_bytes: number | null;
	spam_avg_level: number | null;
	virus_count: number | null;
	virus_bytes: number | null;
	/** `null` unless the attachment quarantine option is on. */
	attachment_count: number | null;
}

export interface PmgTraffic {
	probed_at: number | null;
	mail: PmgMail | null;
	/** Oldest slice first. */
	recent: PmgRecentPoint[];
	spam_scores: PmgSpamScore[];
	viruses: PmgVirus[];
	quarantine: PmgQuarantine | null;
}

export interface PmgService {
	service: string;
	description: string | null;
	state: string | null;
	unit_state: string | null;
	running: boolean;
}

export interface PmgSignature {
	/** `main`, `daily`, `bytecode` for ClamAV; the channel for SpamAssassin. */
	name: string;
	version: string | null;
	updated_at: number | null;
	signatures: number | null;
	update_available: boolean | null;
	/** `virus` (ClamAV) or `spam` (SpamAssassin). */
	family: string;
	/** `null` when the database has never been dated. */
	age_seconds: number | null;
	stale: boolean;
}

export interface PmgCertificate {
	filename: string;
	subject: string | null;
	issuer: string | null;
	not_after: number | null;
	san: string[];
}

export interface PmgSubscription {
	status: string;
	level: string | null;
	next_due_date: string | null;
}

export interface PmgNode {
	name: string;
	uptime_seconds: number | null;
	cpu_percent: number | null;
	cpu_count: number | null;
	loadavg: number[];
	memory_used_bytes: number | null;
	memory_total_bytes: number | null;
	swap_used_bytes: number | null;
	swap_total_bytes: number | null;
	rootfs_used_bytes: number | null;
	rootfs_total_bytes: number | null;
	kernel: string | null;
	/** API version on this node: in a cluster, the one left behind shows here. */
	version: string | null;
	insync: boolean | null;
	clock_offset_seconds: number | null;
	services: PmgService[];
	virus_databases: PmgSignature[];
	spam_rules: PmgSignature[];
	certificates: PmgCertificate[];
	subscription: PmgSubscription | null;
	updates_pending: number | null;
	updates_security_pending: number | null;
	/** Virus databases then spam rules, each dated and judged. */
	signatures: PmgSignature[];
	/** Certificates expiring in less than fourteen days. */
	expiring_certificates: PmgCertificate[];
}

export interface PmgClusterNode {
	name: string;
	ip: string | null;
	/** `master` or `node`. */
	role: string | null;
	insync: boolean | null;
	error: string | null;
}

export interface PmgStoppedService {
	node: string;
	service: string;
	description: string | null;
	state: string | null;
}

export interface PmgHealth {
	probed_at: number | null;
	version: string | null;
	nodes: PmgNode[];
	/** Empty on a standalone gateway: no cluster, not a degraded one. */
	cluster: PmgClusterNode[];
	stopped_services: PmgStoppedService[];
}

// --- First-run guide --------------------------------------------------------

/**
 * Where the instance stands on the three first-run steps, as the server sees
 * it. `complete` is latched — set when the three steps are settled, or on the
 * very first read of an instance that already had a device and a channel — and
 * never goes back to false, so the guide never returns.
 */
export interface OnboardingState {
	/** The guide was skipped, for every browser and every user. */
	skipped: boolean;
	complete: boolean;
	/** UTC without suffix; `null` until the guide completes. */
	completed_at: string | null;
	/** Ids of the steps dismissed one by one: `device`, `channel`, `test`. */
	dismissed: OnboardingStep[];
	has_target: boolean;
	has_channel: boolean;
	/** A message really left on some channel — a test counts. */
	notification_confirmed: boolean;
}

export type OnboardingStep = 'device' | 'channel' | 'test';

export interface OnboardingPatch {
	skipped?: boolean;
	dismissed?: OnboardingStep[];
}

// --- Backup and restore -----------------------------------------------------

/** One section of the backup bundle. Mirrors `Section` in `crates/server/src/api/backup.rs`. */
export interface BackupSection {
	section: string;
	description: string;
	count: number;
}

/** A backup file sitting in the local backup directory. */
export interface BackupFile {
	name: string;
	bytes: number;
	/** UTC without suffix, read from the file name. */
	at: string;
	/** The matching `secret.key` copy is next to it. */
	with_secret: boolean;
}

/** What one scheduled run did. `null` fields mean it never ran. */
export interface BackupRun {
	at: string;
	ok: boolean;
	file: string;
	bytes: number;
	duration_ms: number;
	with_secret: boolean;
	error: string | null;
}

/** State of the scheduled local backups. Mirrors `local::Status`. */
export interface BackupSchedule {
	enabled: boolean;
	directory: string;
	interval_hours: number;
	keep: number;
	/** False when the instance secret comes from `DUMBMONIT_SECRET`: nothing to copy. */
	includes_secret_key: boolean;
	last_run: BackupRun | null;
	files: BackupFile[];
	total_bytes: number;
	directory_error: string | null;
}

/** Response of `GET /api/backup`. */
export interface BackupStatus {
	bundle_version: number;
	min_passphrase_length: number;
	contents: BackupSection[];
	/** `file` when the secret lives in `secret.key`, `environment` otherwise. */
	secret_source: 'file' | 'environment';
	secret_path: string;
	schedule: BackupSchedule;
}

/** The cleartext header of a bundle: readable without the passphrase. */
export interface BackupEnvelope {
	format: string;
	version: number;
	created_at: string;
	source_version: string;
	summary: Record<string, number>;
	cipher: string;
	payload: string;
	[key: string]: unknown;
}

/** What one section of a restore produced. Mirrors `SectionReport`. */
export interface RestoreSection {
	section: string;
	created: number;
	updated: number;
	skipped: number;
	notes: string[];
}

/** Response of `POST /api/backup/restore`. Mirrors `RestoreReport`. */
export interface RestoreReport {
	/** False for a dry run: nothing was written. */
	applied: boolean;
	created_at: string;
	source_version: string;
	sections: RestoreSection[];
	created: number;
	updated: number;
	skipped: number;
	warnings: string[];
}
