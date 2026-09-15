/**
 * Session state: who is signed in, and what they may do.
 *
 * Accounts have two roles: `admin` (everything) and `viewer` (read only). This
 * store is the single source of truth about the session. The cookie set by the
 * server is `HttpOnly`: the interface can neither read nor forge it, and stores
 * no token. The state is therefore deduced only from `GET /api/auth/status`
 * and from the 401s received on the other routes. Hiding controls for viewers
 * is a convenience — the server enforces the roles.
 */
import {
	getAuthStatus,
	login as apiLogin,
	logout as apiLogout,
	setUnauthorizedHandler,
	setupAccount as apiSetupAccount,
	type OidcStatus,
	type User
} from '$lib/api';

/**
 * Minimum length required for a password.
 *
 * The server enforces the same rule; checking it here avoids a round trip and
 * an error message for an obvious mistake.
 */
export const PASSWORD_MIN_LENGTH = 12;

/**
 * Validates a password before sending it. Returns the message to display, or `null`.
 *
 * Code points are counted, as the server does: "éèàùçéèàùçéè" is indeed twelve
 * characters.
 */
export function validatePassword(value: string): string | null {
	if ([...value].length < PASSWORD_MIN_LENGTH) {
		return `The password must be at least ${PASSWORD_MIN_LENGTH} characters long. An easy-to-remember phrase works well.`;
	}
	return null;
}

/** Pages reachable without an open session. */
export const PUBLIC_ROUTES = ['/login', '/setup'];

/**
 * Prefixes of pages served to anyone, outside the app shell: the public status
 * pages (`/s/<slug>`). They never redirect to sign-in, and never bounce a
 * signed-in user away either.
 */
export const STANDALONE_PREFIXES = ['/s/'];

export function isStandaloneRoute(pathname: string): boolean {
	return STANDALONE_PREFIXES.some((prefix) => pathname.startsWith(prefix));
}

export function isPublicRoute(pathname: string): boolean {
	return PUBLIC_ROUTES.includes(pathname) || isStandaloneRoute(pathname);
}

/**
 * Validates the remembered destination before sending the user back to it.
 *
 * Only an internal path is accepted: a `redirect` parameter coming from outside
 * must never be used to bounce to another site. `//` is refused because
 * browsers interpret it as an absolute URL.
 */
export function safeDestination(value: string | null): string {
	if (!value) return '/';
	if (!value.startsWith('/') || value.startsWith('//')) return '/';
	if (isPublicRoute(new URL(value, 'http://local').pathname)) return '/';
	return value;
}

class AuthStore {
	/** False while we do not know whether the instance is protected (first response not received). */
	checked = $state(false);
	/** False if the server does not serve the auth routes yet. */
	available = $state(false);
	/** True as soon as an account exists on the instance. */
	configured = $state(false);
	/** True if the current session is valid. */
	authenticated = $state(true);
	/** The signed-in account, or `null` (open instance, or not signed in). */
	user = $state<User | null>(null);
	/** Single sign-on availability, read without a session for the sign-in screen. */
	oidc = $state<OidcStatus>({ enabled: false, provider_name: 'SSO', login_url: '/api/auth/oidc/start' });
	/** Error preventing us from knowing the session state (server unreachable). */
	error = $state<unknown>(null);

	#installed = false;

	/**
	 * True when the current user may change things. An unprotected instance
	 * (auth not available, or no account yet) has no roles: everything is allowed.
	 */
	get isAdmin(): boolean {
		if (!this.available || !this.configured) return true;
		return this.user?.role === 'admin';
	}

	/** Name to show for the signed-in account. */
	get displayName(): string {
		if (!this.user) return '';
		return this.user.display_name.trim() || this.user.username;
	}

	/** True when business API calls can succeed: open instance, or valid session. */
	get canUseApi(): boolean {
		return this.checked && (!this.available || (this.configured && this.authenticated));
	}

	/** True when a sign-out button makes sense. */
	get canSignOut(): boolean {
		return this.available && this.configured && this.authenticated;
	}

	/**
	 * Installs the global 401 reaction and queries the server.
	 *
	 * Called once from the root layout.
	 */
	async init(): Promise<void> {
		if (!this.#installed) {
			this.#installed = true;
			setUnauthorizedHandler(() => this.handleUnauthorized());
		}
		await this.refresh();
	}

	/** Re-reads the session state from the server. */
	async refresh(signal?: AbortSignal): Promise<void> {
		try {
			const state = await getAuthStatus(signal);
			this.available = state.available;
			this.configured = state.configured;
			this.authenticated = state.authenticated;
			this.user = state.user;
			this.oidc = state.oidc;
			this.error = null;
		} catch (cause) {
			if (cause instanceof DOMException && cause.name === 'AbortError') return;
			// The server is unreachable: nothing will work anyway, but we do not lock
			// the user behind a sign-in screen that would fail just as much. Pages
			// will display their own loading error.
			this.available = false;
			this.configured = false;
			this.authenticated = true;
			this.user = null;
			this.error = cause;
		} finally {
			this.checked = true;
		}
	}

	/**
	 * Reacts to a 401 received on any API route.
	 *
	 * A 401 alone proves that the instance is protected and that the session is
	 * worthless. We only flip the state: the layout guard deduces the redirect
	 * to the sign-in screen from it, remembering the requested page. A single
	 * place navigates, so there cannot be two concurrent redirects to different
	 * destinations.
	 */
	handleUnauthorized(): void {
		this.available = true;
		this.configured = true;
		this.authenticated = false;
		this.user = null;
		this.checked = true;
	}

	/** Creates the first admin account, then opens the session right after. */
	async setupAccount(username: string, password: string): Promise<void> {
		await apiSetupAccount(username, password);
		// Creating the account does not open a session: we sign in explicitly.
		// The state is only updated afterwards, by `login` — announcing "protected
		// instance" before having a cookie would flip the navigation guard to the
		// sign-in screen in the middle of the setup.
		await this.login(username, password);
	}

	/** Opens a session. Propagates the error so the screen can show the right message. */
	async login(username: string, password: string): Promise<void> {
		await apiLogin(username, password);
		this.available = true;
		this.configured = true;
		this.authenticated = true;
		this.error = null;
		this.checked = true;
		// The role decides what the pages show: read it before the guard navigates.
		await this.refresh();
	}

	/**
	 * Closes the session.
	 *
	 * No navigation is triggered here: the layout guard reacts to the state
	 * change and redirects by itself. Navigating on both sides would run two
	 * concurrent `goto`s, with a different destination depending on which one
	 * lands last.
	 */
	async logout(): Promise<void> {
		try {
			await apiLogout();
		} catch {
			// Even if the server refuses, the local session is considered closed:
			// the sign-in screen will take over and fix the state if needed.
		}
		this.authenticated = false;
		this.user = null;
	}
}

export const auth = new AuthStore();
