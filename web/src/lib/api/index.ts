/**
 * API surface exposed to components.
 *
 * Each function maps to a server route. Those marked "anticipated" target
 * endpoints that do not exist yet: they degrade gracefully (empty array,
 * `null`) instead of failing the whole page.
 */
import { request, ApiError } from './client';
import { normalizeCollector } from './types';
import type {
	AgentHost,
	AgentToken,
	AgentTokenPayload,
	RebindWindow,
	ApiToken,
	ApiTokenScope,
	AckPayload,
	Alert,
	AlertHistoryEntry,
	AlertRule,
	AlertRulePayload,
	Silence,
	SilencePayload,
	AuthState,
	Channel,
	ChannelKindInfo,
	ChannelPayload,
	ChannelTestReport,
	CreatedAgentToken,
	CreatedApiToken,
	AuthStatus,
	CreateUserPayload,
	OidcConfig,
	OidcConfigPayload,
	OidcTestReport,
	UpdateUserPayload,
	User,
	LoginOutcome,
	CollectorInfo,
	DiscoveredDevice,
	DiscoveryResult,
	Health,
	MetricSeries,
	ProbeReport,
	Target,
	TargetId,
	TargetPayload,
	ChannelMatcher,
	MatchPreview,
	NotificationPolicy,
	OnboardingState,
	OnboardingPatch,
	NotificationPolicyPayload,
	RuleOverride,
	RuleOverridePayload,
	StatusPage,
	StatusPagePayload,
	StatusPageItem,
	StatusPageItemPayload,
	Incident,
	IncidentPayload,
	IncidentUpdatePayload,
	PublicStatus,
	StatusSubscriber
} from './types';

export * from './types';
export { ApiError, toApiError, setUnauthorizedHandler } from './client';

// --- Health -----------------------------------------------------------------

export function getHealth(signal?: AbortSignal): Promise<Health> {
	return request<Health>('/health', { signal });
}

// --- Targets ----------------------------------------------------------------

export function listTargets(signal?: AbortSignal): Promise<Target[]> {
	return request<Target[]>('/targets', { signal });
}

export function getTarget(id: TargetId, signal?: AbortSignal): Promise<Target> {
	return request<Target>(`/targets/${id}`, { signal });
}

export function createTarget(payload: TargetPayload): Promise<Target> {
	return request<Target>('/targets', { method: 'POST', body: payload });
}

export function updateTarget(id: TargetId, payload: TargetPayload): Promise<Target> {
	return request<Target>(`/targets/${id}`, { method: 'PUT', body: payload });
}

export function deleteTarget(id: TargetId): Promise<void> {
	return request<void>(`/targets/${id}`, { method: 'DELETE' });
}

/** Probes the device right away. Can be slow: the caller shows an indicator. */
export function probeTarget(id: TargetId): Promise<ProbeReport> {
	return request<ProbeReport>(`/targets/${id}/probe`, { method: 'POST' });
}

// --- Metrics (anticipated) --------------------------------------------------

export interface QueryRangeParams {
	query: string;
	/** Bounds in milliseconds, as expected by the server. */
	start: number;
	end: number;
	/** Sampling step in seconds. */
	step: number;
}

/**
 * Queries VictoriaMetrics over a range. Returns an empty array while the route
 * is not deployed, so that a missing chart does not keep the page from living.
 */
export async function queryRange(
	params: QueryRangeParams,
	signal?: AbortSignal
): Promise<MetricSeries[]> {
	try {
		const series = await request<MetricSeries[]>('/metrics/query_range', {
			query: {
				query: params.query,
				start: Math.round(params.start),
				end: Math.round(params.end),
				step: Math.max(1, Math.round(params.step))
			},
			signal,
			anticipated: true
		});
		return Array.isArray(series) ? series : [];
	} catch (cause) {
		if (cause instanceof ApiError && cause.missing) return [];
		throw cause;
	}
}

/**
 * Queries VictoriaMetrics at the present instant (`GET /api/metrics/query`).
 *
 * Each returned series carries a single value in `values`. Like `queryRange`,
 * the function returns an empty array if the route is not deployed: an unknown
 * state beats a page in error.
 */
export async function queryInstant(query: string, signal?: AbortSignal): Promise<MetricSeries[]> {
	try {
		const series = await request<MetricSeries[]>('/metrics/query', {
			query: { query },
			signal,
			anticipated: true
		});
		return Array.isArray(series) ? series : [];
	} catch (cause) {
		if (cause instanceof ApiError && cause.missing) return [];
		throw cause;
	}
}

// --- Alerts (anticipated) ---------------------------------------------------

/** Returns the active alerts, or an empty array if the route does not exist yet. */
export async function listAlerts(signal?: AbortSignal): Promise<Alert[]> {
	try {
		const alerts = await request<Alert[]>('/alerts', { signal, anticipated: true });
		return Array.isArray(alerts) ? alerts : [];
	} catch (cause) {
		if (cause instanceof ApiError && cause.missing) return [];
		throw cause;
	}
}

/** True if the alerts route is already served by the backend. */
export async function alertsAvailable(signal?: AbortSignal): Promise<boolean> {
	try {
		await request<Alert[]>('/alerts', { signal, anticipated: true });
		return true;
	} catch (cause) {
		if (cause instanceof ApiError && cause.missing) return false;
		return true;
	}
}

/**
 * Recent phase transitions, newest first. `limit` caps the count; `since` is an
 * RFC 3339 lower bound (the server defaults to the last seven days).
 */
export async function listAlertHistory(
	options: { limit?: number; since?: string } = {},
	signal?: AbortSignal
): Promise<AlertHistoryEntry[]> {
	try {
		const entries = await request<AlertHistoryEntry[]>('/alerts/history', {
			query: { limit: options.limit, since: options.since },
			signal,
			anticipated: true
		});
		return Array.isArray(entries) ? entries : [];
	} catch (cause) {
		if (cause instanceof ApiError && cause.missing) return [];
		throw cause;
	}
}

/** Lists the alert rules, or an empty array if the route does not exist yet. */
export async function listAlertRules(signal?: AbortSignal): Promise<AlertRule[]> {
	try {
		const rules = await request<AlertRule[]>('/alerts/rules', { signal, anticipated: true });
		return Array.isArray(rules) ? rules : [];
	} catch (cause) {
		if (cause instanceof ApiError && cause.missing) return [];
		throw cause;
	}
}

export function createAlertRule(payload: AlertRulePayload): Promise<AlertRule> {
	return request<AlertRule>('/alerts/rules', { method: 'POST', body: payload });
}

export function updateAlertRule(id: number, payload: AlertRulePayload): Promise<AlertRule> {
	return request<AlertRule>(`/alerts/rules/${id}`, { method: 'PUT', body: payload });
}

export function deleteAlertRule(id: number): Promise<void> {
	return request<void>(`/alerts/rules/${id}`, { method: 'DELETE' });
}

export function setAlertRuleEnabled(id: number, enabled: boolean): Promise<AlertRule> {
	return request<AlertRule>(`/alerts/rules/${id}/enable`, {
		method: 'POST',
		body: { enabled }
	});
}

/** Lists the maintenance windows, or an empty array if the route does not exist yet. */
export async function listSilences(signal?: AbortSignal): Promise<Silence[]> {
	try {
		const silences = await request<Silence[]>('/alerts/silences', { signal, anticipated: true });
		return Array.isArray(silences) ? silences : [];
	} catch (cause) {
		if (cause instanceof ApiError && cause.missing) return [];
		throw cause;
	}
}

export function createSilence(payload: SilencePayload): Promise<Silence> {
	return request<Silence>('/alerts/silences', { method: 'POST', body: payload });
}

export function deleteSilence(id: number): Promise<void> {
	return request<void>(`/alerts/silences/${id}`, { method: 'DELETE' });
}

/** Acknowledges an alert ("I know, stop reminding me"); returns the updated alert. */
export function ackAlert(fingerprint: string, payload: AckPayload = {}): Promise<Alert> {
	return request<Alert>(`/alerts/${encodeURIComponent(fingerprint)}/ack`, {
		method: 'POST',
		body: payload
	});
}

/** Lifts an acknowledgement; returns the updated alert. */
export function unackAlert(fingerprint: string): Promise<Alert> {
	return request<Alert>(`/alerts/${encodeURIComponent(fingerprint)}/ack`, { method: 'DELETE' });
}

// --- Network discovery ------------------------------------------------------

interface RawDiscoveredDevice {
	address?: unknown;
	sysname?: unknown;
	sysdescr?: unknown;
	sysobjectid?: unknown;
	suggested_profile?: unknown;
}

function optionalText(value: unknown): string | null {
	return typeof value === 'string' && value.trim() ? value.trim() : null;
}

/**
 * Scans a CIDR looking for SNMP devices (`POST /api/discovery`, admin only).
 *
 * The parameters travel in the body, not the query string: the community is a
 * credential and must not land in server or proxy logs. The server answers
 * `{ devices, scanned }`; this folds each device into the UI shape and marks
 * the ones already monitored (by host, ignoring the port). Throws an
 * `ApiError` with `missing = true` if the route does not exist.
 */
export async function discover(
	cidr: string,
	options: { community?: string; signal?: AbortSignal } = {}
): Promise<DiscoveryResult> {
	const [raw, targets] = await Promise.all([
		request<unknown>('/discovery', {
			method: 'POST',
			body: { cidr: cidr.trim(), community: options.community?.trim() || undefined },
			signal: options.signal,
			anticipated: true
		}),
		// Only used to grey out known addresses: a failure must not hide the scan.
		listTargets(options.signal).catch(() => [] as Target[])
	]);

	const host = (address: string) => address.trim().toLowerCase().replace(/:\d+$/, '');
	const known = new Set(targets.map((t) => host(t.address)));
	const list: unknown[] = Array.isArray(raw)
		? raw
		: raw && typeof raw === 'object' && Array.isArray((raw as { devices?: unknown }).devices)
			? (raw as { devices: unknown[] }).devices
			: [];
	const scanned =
		raw && typeof raw === 'object' && typeof (raw as { scanned?: unknown }).scanned === 'number'
			? (raw as { scanned: number }).scanned
			: null;

	const devices = (list as RawDiscoveredDevice[])
		.filter((item) => item && typeof item.address === 'string' && item.address.trim())
		.map((item): DiscoveredDevice => {
			const address = (item.address as string).trim();
			return {
				address,
				name: optionalText(item.sysname),
				sysname: optionalText(item.sysname),
				description: optionalText(item.sysdescr),
				sysobjectid: optionalText(item.sysobjectid),
				kind: 'snmp',
				profile_id: optionalText(item.suggested_profile),
				already_added: known.has(host(address))
			};
		});

	return { devices, scanned };
}

// --- Device types -----------------------------------------------------------

/**
 * Lists the device types this server can collect, along with their setup notes.
 *
 * Nothing is hard-coded on the interface side: the add screen is built from
 * this response, including for a type this version of the interface does not
 * know yet.
 */
export async function listCollectors(signal?: AbortSignal): Promise<CollectorInfo[]> {
	const raw = await request<CollectorInfo[]>('/collectors', { signal, anticipated: true });
	if (!Array.isArray(raw)) return [];
	return raw.filter((item) => item && typeof item.kind === 'string').map(normalizeCollector);
}

// --- Authentication ---------------------------------------------------------

const NO_OIDC = { enabled: false, provider_name: 'SSO', login_url: '/api/auth/oidc/start' };

/**
 * Protection state of the instance.
 *
 * As long as authentication is not deployed on the server, the route answers
 * 404: the instance is then declared unprotected (`available: false`) and the
 * interface works normally, rather than locking itself entirely.
 */
export async function getAuthStatus(signal?: AbortSignal): Promise<AuthState> {
	try {
		const status = await request<AuthStatus>('/auth/status', {
			signal,
			anticipated: true,
			allowUnauthorized: true
		});
		return {
			available: true,
			configured: Boolean(status?.configured),
			authenticated: Boolean(status?.authenticated),
			user: status?.user ?? null,
			oidc: status?.oidc ?? NO_OIDC
		};
	} catch (cause) {
		if (cause instanceof DOMException && cause.name === 'AbortError') throw cause;
		if (cause instanceof ApiError && cause.missing) {
			return { available: false, configured: false, authenticated: true, user: null, oidc: NO_OIDC };
		}
		throw cause;
	}
}

/** Creates the first admin account. Answers 409 if one already exists. */
export function setupAccount(username: string, password: string): Promise<void> {
	return request<void>('/auth/setup', {
		method: 'POST',
		body: { username, password },
		allowUnauthorized: true
	});
}

/**
 * Opens a session. The cookie set by the server is `HttpOnly`: the interface
 * never reads it. When the account has two-factor authentication, the server
 * answers `200 { totp_required: true, pending }` instead of `204`: the session
 * is only opened by `loginTotp`.
 */
export async function login(username: string, password: string): Promise<LoginOutcome> {
	const reply = await request<{ totp_required?: boolean; pending?: string } | undefined>('/auth/login', {
		method: 'POST',
		body: { username, password },
		allowUnauthorized: true
	});
	return { totp_required: Boolean(reply?.totp_required), pending: reply?.pending ?? null };
}

/** Second step of the sign-in: a six-digit code or a recovery code. */
export function loginTotp(pending: string, code: string): Promise<void> {
	return request<void>('/auth/login/totp', {
		method: 'POST',
		body: { pending, code },
		allowUnauthorized: true
	});
}

/** Closes the current session. */
export function logout(): Promise<void> {
	return request<void>('/auth/logout', { method: 'POST', allowUnauthorized: true });
}

/**
 * Changes the current account's password.
 *
 * A 401 here means "wrong current password", not "expired session": it must
 * never send the user back to the sign-in screen.
 */
export function changePassword(currentPassword: string, newPassword: string): Promise<void> {
	return request<void>('/auth/password', {
		method: 'POST',
		body: { current_password: currentPassword, new_password: newPassword },
		allowUnauthorized: true
	});
}

// --- Users (admin only) -----------------------------------------------------

export function listUsers(signal?: AbortSignal): Promise<User[]> {
	return request<User[]>('/users', { signal });
}

export function createUser(payload: CreateUserPayload): Promise<User> {
	return request<User>('/users', { method: 'POST', body: payload });
}

export function updateUser(id: number, payload: UpdateUserPayload): Promise<User> {
	return request<User>(`/users/${id}`, { method: 'PUT', body: payload });
}

export function deleteUser(id: number): Promise<void> {
	return request<void>(`/users/${id}`, { method: 'DELETE' });
}

// --- Single sign-on (admin only) --------------------------------------------

export function getOidcConfig(signal?: AbortSignal): Promise<OidcConfig> {
	return request<OidcConfig>('/auth/oidc/config', { signal });
}

export function saveOidcConfig(payload: OidcConfigPayload): Promise<OidcConfig> {
	return request<OidcConfig>('/auth/oidc/config', { method: 'PUT', body: payload });
}

/** Forgets the saved settings: environment variables apply again, or SSO turns off. */
export function clearOidcConfig(): Promise<OidcConfig> {
	return request<OidcConfig>('/auth/oidc/config', { method: 'DELETE' });
}

/** Runs discovery against `issuer` (or the saved one) and reports the endpoints found. */
export function testOidcDiscovery(issuer?: string): Promise<OidcTestReport> {
	return request<OidcTestReport>('/auth/oidc/test', { method: 'POST', body: { issuer } });
}

// --- Notification channels --------------------------------------------------

export function listChannelKinds(signal?: AbortSignal): Promise<ChannelKindInfo[]> {
	return request<ChannelKindInfo[]>('/notify/kinds', { signal });
}

export function listChannels(signal?: AbortSignal): Promise<Channel[]> {
	return request<Channel[]>('/notify/channels', { signal });
}

export function createChannel(payload: ChannelPayload): Promise<Channel> {
	return request<Channel>('/notify/channels', { method: 'POST', body: payload });
}

export function updateChannel(id: number, payload: ChannelPayload): Promise<Channel> {
	return request<Channel>(`/notify/channels/${id}`, { method: 'PUT', body: payload });
}

export function deleteChannel(id: number): Promise<void> {
	return request<void>(`/notify/channels/${id}`, { method: 'DELETE' });
}

/** Sends a test message. Can take a few seconds: the caller shows an indicator. */
export function testChannel(id: number): Promise<ChannelTestReport> {
	return request<ChannelTestReport>(`/notify/channels/${id}/test`, { method: 'POST' });
}

// --- Agents -----------------------------------------------------------------

export function listAgentTokens(signal?: AbortSignal): Promise<AgentToken[]> {
	return request<AgentToken[]>('/agent/tokens', { signal });
}

/**
 * Creates an enrolment token. `baseUrl` is the address through which machines
 * will reach this server — in practice `location.origin`.
 *
 * Without a scope the token is single use: it enrols one machine and then
 * enrols nothing more. Pass `reusable` for a fleet.
 */
export function createAgentToken(payload: AgentTokenPayload): Promise<CreatedAgentToken> {
	return request<CreatedAgentToken>('/agent/tokens', { method: 'POST', body: payload });
}

export function revokeAgentToken(id: number): Promise<void> {
	return request<void>(`/agent/tokens/${id}`, { method: 'DELETE' });
}

/**
 * The agent of a device. `null` when no agent has reported yet, or on an older
 * server without the route: the caller then treats commands as unsupported.
 */
export async function getAgentHost(id: TargetId, signal?: AbortSignal): Promise<AgentHost | null> {
	try {
		return await request<AgentHost>(`/targets/${id}/agent`, { signal, anticipated: true });
	} catch (cause) {
		if (cause instanceof ApiError && cause.missing) return null;
		throw cause;
	}
}

/**
 * Lets this machine bind itself again at its next batch, for a limited window.
 * The way back in after a reinstall took the agent's own secret with it.
 */
export function allowAgentRebind(id: TargetId): Promise<RebindWindow> {
	return request<RebindWindow>(`/targets/${id}/agent/rebind`, { method: 'POST' });
}

// --- API tokens (assistants, MCP) -------------------------------------------

export function listApiTokens(signal?: AbortSignal): Promise<ApiToken[]> {
	return request<ApiToken[]>('/tokens', { signal });
}

/** Creates an API token. The secret is only ever returned here. */
export function createApiToken(name: string, scope: ApiTokenScope): Promise<CreatedApiToken> {
	return request<CreatedApiToken>('/tokens', { method: 'POST', body: { name, scope } });
}

export function revokeApiToken(id: number): Promise<void> {
	return request<void>(`/tokens/${id}`, { method: 'DELETE' });
}

// --- Notification policy and per-device overrides --------------------------

export function getNotificationPolicy(signal?: AbortSignal): Promise<NotificationPolicy> {
	return request<NotificationPolicy>('/notify/policy', { signal });
}

export function updateNotificationPolicy(payload: NotificationPolicyPayload): Promise<NotificationPolicy> {
	return request<NotificationPolicy>('/notify/policy', { method: 'PUT', body: payload });
}

/**
 * Which of the current devices a routing filter selects.
 *
 * The server runs the very filter the alerting engine runs, so the preview and
 * the delivered notifications can never disagree.
 */
export function previewMatcher(matcher: ChannelMatcher, signal?: AbortSignal): Promise<MatchPreview> {
	return request<MatchPreview>('/notify/match-preview', {
		method: 'POST',
		body: { matcher },
		signal
	});
}

/** Overrides of every rule, optionally only those of one device. */
export function listRuleOverrides(targetId?: TargetId, signal?: AbortSignal): Promise<RuleOverride[]> {
	const query = targetId === undefined ? '' : `?target_id=${targetId}`;
	return request<RuleOverride[]>(`/alerts/overrides${query}`, { signal });
}

/** Sets a rule's override for a device. An empty payload removes it. */
export function putRuleOverride(
	ruleId: number,
	targetId: TargetId,
	payload: RuleOverridePayload
): Promise<RuleOverride> {
	return request<RuleOverride>(`/alerts/rules/${ruleId}/overrides/${targetId}`, {
		method: 'PUT',
		body: payload
	});
}

export function deleteRuleOverride(ruleId: number, targetId: TargetId): Promise<void> {
	return request<void>(`/alerts/rules/${ruleId}/overrides/${targetId}`, { method: 'DELETE' });
}

// --- Status pages -----------------------------------------------------------

export function listStatusPages(signal?: AbortSignal): Promise<StatusPage[]> {
	return request<StatusPage[]>('/status-pages', { signal });
}

export function getStatusPage(id: number, signal?: AbortSignal): Promise<StatusPage> {
	return request<StatusPage>(`/status-pages/${id}`, { signal });
}

export function createStatusPage(payload: StatusPagePayload): Promise<StatusPage> {
	return request<StatusPage>('/status-pages', { method: 'POST', body: payload });
}

export function updateStatusPage(id: number, payload: StatusPagePayload): Promise<StatusPage> {
	return request<StatusPage>(`/status-pages/${id}`, { method: 'PUT', body: payload });
}

export function deleteStatusPage(id: number): Promise<void> {
	return request<void>(`/status-pages/${id}`, { method: 'DELETE' });
}

/** Replaces the services of a page; the array order is the display order. */
export function setStatusPageItems(
	id: number,
	items: StatusPageItemPayload[]
): Promise<StatusPageItem[]> {
	return request<StatusPageItem[]>(`/status-pages/${id}/items`, { method: 'PUT', body: items });
}

export function listIncidents(signal?: AbortSignal): Promise<Incident[]> {
	return request<Incident[]>('/incidents', { signal });
}

export function createIncident(payload: IncidentPayload): Promise<Incident> {
	return request<Incident>('/incidents', { method: 'POST', body: payload });
}

export function updateIncident(id: number, payload: IncidentPayload): Promise<Incident> {
	return request<Incident>(`/incidents/${id}`, { method: 'PUT', body: payload });
}

export function deleteIncident(id: number): Promise<void> {
	return request<void>(`/incidents/${id}`, { method: 'DELETE' });
}

/** Posts a message and moves the incident to `status` (or keeps it). */
export function addIncidentUpdate(id: number, payload: IncidentUpdatePayload): Promise<Incident> {
	return request<Incident>(`/incidents/${id}/updates`, { method: 'POST', body: payload });
}

/** Uploads a logo (PNG, JPEG or WebP, as a base64 or `data:` URL). */
export function uploadStatusPageLogo(id: number, data: string): Promise<void> {
	return request<void>(`/status-pages/${id}/logo`, { method: 'PUT', body: { data } });
}

export function deleteStatusPageLogo(id: number): Promise<void> {
	return request<void>(`/status-pages/${id}/logo`, { method: 'DELETE' });
}

/** Admin preview URL of a page's logo (works for drafts too). */
export function statusPageLogoUrl(id: number, version: string): string {
	return `/api/status-pages/${id}/logo?v=${encodeURIComponent(version)}`;
}

export function listStatusSubscribers(id: number, signal?: AbortSignal): Promise<StatusSubscriber[]> {
	return request<StatusSubscriber[]>(`/status-pages/${id}/subscribers`, { signal });
}

export function deleteStatusSubscriber(id: number, subscriber: number): Promise<void> {
	return request<void>(`/status-pages/${id}/subscribers/${subscriber}`, { method: 'DELETE' });
}

/** Public: asks for a confirmation email. Same answer whatever the address's state. */
export function subscribeToStatus(slug: string, email: string): Promise<{ status: string; message: string }> {
	return request(`/public/status/${encodeURIComponent(slug)}/subscribe`, { method: 'POST', body: { email } });
}

/** Public: confirms a subscription with the token from the email. */
export function confirmStatusSubscription(slug: string, token: string): Promise<{ status: string }> {
	return request(`/public/status/${encodeURIComponent(slug)}/confirm`, { method: 'POST', query: { token } });
}

/** Public: removes a subscription with the token from any update email. */
export function unsubscribeFromStatus(slug: string, token: string): Promise<{ status: string }> {
	return request(`/public/status/${encodeURIComponent(slug)}/unsubscribe`, { method: 'POST', query: { token } });
}

/** Public: base URL of a page's badges (append `badge.svg`, `uptime.svg?days=30`, `response.svg`). */
export function statusBadgeBase(slug: string, componentKey?: string): string {
	const base = `/api/public/status/${encodeURIComponent(slug)}`;
	return componentKey ? `${base}/components/${encodeURIComponent(componentKey)}` : base;
}

/** Public document of a status page: no session, no cookie needed. */
export function getPublicStatus(slug: string, signal?: AbortSignal): Promise<PublicStatus> {
	return request<PublicStatus>(`/public/status/${encodeURIComponent(slug)}`, { signal });
}

// --- First-run guide (anticipated) ------------------------------------------

/**
 * Reads the first-run guide's state. Anticipated: a server without the route
 * answers 404, and the caller then treats the guide as finished rather than
 * showing a guide it could never dismiss.
 */
export function getOnboarding(signal?: AbortSignal): Promise<OnboardingState> {
	return request<OnboardingState>('/onboarding', { signal, anticipated: true });
}

/** Dismisses a step, or the whole guide. Returns the state the server kept. */
export function updateOnboarding(patch: OnboardingPatch): Promise<OnboardingState> {
	return request<OnboardingState>('/onboarding', { method: 'PUT', body: patch });
}
