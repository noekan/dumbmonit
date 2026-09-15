/**
 * Light / dark theme.
 *
 * Three states: `auto` follows `prefers-color-scheme`, `light` and `dark`
 * force the user's choice and are remembered. The effective theme is applied
 * to `<html>` as a `dark` class, as Tailwind expects.
 */
import { browser } from '$app/environment';

export type ThemePreference = 'auto' | 'light' | 'dark';

const STORAGE_KEY = 'ezymonit-theme';

function readPreference(): ThemePreference {
	if (!browser) return 'auto';
	try {
		const stored = localStorage.getItem(STORAGE_KEY);
		if (stored === 'light' || stored === 'dark') return stored;
	} catch {
		// Storage unavailable (strict private browsing): stay in automatic mode.
	}
	return 'auto';
}

function systemIsDark(): boolean {
	return browser && window.matchMedia('(prefers-color-scheme: dark)').matches;
}

class Theme {
	preference = $state<ThemePreference>('auto');
	/** True when the system is dark; tracked live for automatic mode. */
	#systemDark = $state(false);

	constructor() {
		this.preference = readPreference();
		this.#systemDark = systemIsDark();
	}

	/** Theme actually applied, once the preference is resolved. */
	get resolved(): 'light' | 'dark' {
		if (this.preference === 'auto') return this.#systemDark ? 'dark' : 'light';
		return this.preference;
	}

	/**
	 * Listens for system theme changes. Call once from the root layout; returns
	 * the unsubscribe function.
	 */
	watchSystem(): () => void {
		if (!browser) return () => {};
		const query = window.matchMedia('(prefers-color-scheme: dark)');
		const onChange = (event: MediaQueryListEvent) => {
			this.#systemDark = event.matches;
		};
		query.addEventListener('change', onChange);
		return () => query.removeEventListener('change', onChange);
	}

	/** Applies the theme to the document. Called by an effect in the root layout. */
	apply() {
		if (!browser) return;
		document.documentElement.classList.toggle('dark', this.resolved === 'dark');
	}

	set(preference: ThemePreference) {
		this.preference = preference;
		if (!browser) return;
		try {
			if (preference === 'auto') localStorage.removeItem(STORAGE_KEY);
			else localStorage.setItem(STORAGE_KEY, preference);
		} catch {
			// The theme will simply not be remembered from one visit to the next.
		}
	}

	/** Toggles between light and dark, starting from the theme currently displayed. */
	toggle() {
		this.set(this.resolved === 'dark' ? 'light' : 'dark');
	}
}

export const theme = new Theme();
