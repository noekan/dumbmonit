/**
 * The single HTTP client of the interface.
 *
 * Every network access goes through here: no `fetch` may be written inside a
 * component. This guarantees uniform error handling, consistent user-facing
 * messages, and a single place to change if the API evolves.
 */

/** Normalised error, always carrying a message that can be displayed as is. */
export class ApiError extends Error {
	/** HTTP status, or 0 if the request never completed (network down, server off). */
	readonly status: number;
	/** True when the route does not exist on the server yet (404 on an anticipated endpoint). */
	readonly missing: boolean;

	constructor(message: string, status: number, missing = false) {
		super(message);
		this.name = 'ApiError';
		this.status = status;
		this.missing = missing;
	}

	/** Concrete advice shown to the user below the error message. */
	get hint(): string {
		if (this.status === 0) {
			return 'Check that the DumbMonit server is running and reachable, then try again.';
		}
		if (this.status === 401) {
			return 'Your session has expired or the password is incorrect. Sign in again to continue.';
		}
		if (this.status === 403) {
			return 'You do not have sufficient permissions for this action.';
		}
		if (this.status === 404) {
			return 'The requested item no longer exists. Go back to the list to refresh it.';
		}
		if (this.status === 429) {
			return 'Too many failed attempts. Wait for the indicated delay before trying again.';
		}
		if (this.status === 409) {
			return 'A device already uses this address. Choose another one.';
		}
		if (this.status >= 500) {
			return 'The server ran into an error. Check its logs for details.';
		}
		return 'Fix the information you entered, then try again.';
	}
}

/** Turns any thrown value into a displayable `ApiError`. */
export function toApiError(cause: unknown): ApiError {
	if (cause instanceof ApiError) return cause;
	if (cause instanceof Error) return new ApiError(cause.message, 0);
	return new ApiError('An unexpected error occurred.', 0);
}

interface RequestOptions {
	method?: 'GET' | 'POST' | 'PUT' | 'DELETE';
	body?: unknown;
	query?: Record<string, string | number | undefined>;
	signal?: AbortSignal;
	/**
	 * Marks a route not yet implemented on the server. A 404 then becomes an
	 * `ApiError` with `missing = true`, which the caller can degrade gracefully
	 * instead of displaying as an outage.
	 */
	anticipated?: boolean;
	/**
	 * Prevents the automatic redirect to the sign-in screen on a 401.
	 *
	 * Reserved for routes where a 401 is an expected business response rather
	 * than an expired session: wrong password at sign-in, wrong current password
	 * when changing it. Without this, a simple typo would kick the user out of
	 * the page they are on.
	 */
	allowUnauthorized?: boolean;
}

/**
 * Single reaction to a 401 received on any route.
 *
 * The auth store registers itself here at application start-up. It is the only
 * place in the interface that decides what to do with a lost session: no
 * component has to watch HTTP status codes itself.
 */
type UnauthorizedHandler = () => void;

let unauthorizedHandler: UnauthorizedHandler | null = null;

/** Installs the 401 reaction. Called once, from the auth store. */
export function setUnauthorizedHandler(handler: UnauthorizedHandler | null): void {
	unauthorizedHandler = handler;
}

const BASE = '/api';

/** Builds the final URL, skipping query parameters that are not set. */
function buildUrl(path: string, query?: RequestOptions['query']): string {
	const url = `${BASE}${path}`;
	if (!query) return url;
	const params = new URLSearchParams();
	for (const [key, value] of Object.entries(query)) {
		if (value !== undefined && value !== '') params.set(key, String(value));
	}
	const encoded = params.toString();
	return encoded ? `${url}?${encoded}` : url;
}

/** Extracts the error message from the `{ "error": "..." }` body returned by the server. */
async function readErrorMessage(response: Response): Promise<string> {
	try {
		const text = await response.text();
		if (!text) return `The server responded with ${response.status}.`;
		const parsed: unknown = JSON.parse(text);
		if (parsed && typeof parsed === 'object' && 'error' in parsed) {
			const message = (parsed as { error: unknown }).error;
			if (typeof message === 'string' && message.trim()) return message;
		}
		return `The server responded with ${response.status}.`;
	} catch {
		return `The server responded with ${response.status}.`;
	}
}

/**
 * Executes a request and returns the decoded body.
 *
 * `T` is `void` for 204 responses (deletion), which have no body.
 */
export async function request<T>(path: string, options: RequestOptions = {}): Promise<T> {
	const {
		method = 'GET',
		body,
		query,
		signal,
		anticipated = false,
		allowUnauthorized = false
	} = options;

	let response: Response;
	try {
		response = await fetch(buildUrl(path, query), {
			method,
			signal,
			headers: {
				// Proof of origin for cookie-authenticated writes (anti-CSRF): a
				// third-party page cannot add this header without CORS consent.
				'x-requested-with': 'DumbMonit',
				...(body === undefined ? {} : { 'content-type': 'application/json' })
			},
			body: body === undefined ? undefined : JSON.stringify(body)
		});
	} catch (cause) {
		// A cancelled request is not an outage: propagate it as is so the caller
		// can ignore it silently.
		if (cause instanceof DOMException && cause.name === 'AbortError') throw cause;
		throw new ApiError('Could not reach the DumbMonit server. The connection failed.', 0);
	}

	if (!response.ok) {
		const message = await readErrorMessage(response);
		// The session has expired, or the instance has just been protected: notify
		// once, at the client level, rather than screen by screen.
		if (response.status === 401 && !allowUnauthorized) unauthorizedHandler?.();
		throw new ApiError(message, response.status, anticipated && response.status === 404);
	}

	if (response.status === 204) return undefined as T;

	const text = await response.text();
	if (!text) return undefined as T;
	try {
		return JSON.parse(text) as T;
	} catch {
		throw new ApiError('The server response could not be read.', response.status);
	}
}
