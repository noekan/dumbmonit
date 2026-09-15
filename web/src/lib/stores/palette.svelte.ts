/**
 * Command palette state (Ctrl/⌘ K).
 *
 * Kept in a store so the nav bar hint, the global shortcut and the palette
 * itself agree on one open flag. The palette component is mounted once in the
 * root layout; anything can call `palette.open()`.
 */
import { browser } from '$app/environment';

class Palette {
	isOpen = $state(false);
	/** Element that had focus before the palette opened; focus returns to it. */
	returnTo: HTMLElement | null = null;

	open() {
		if (this.isOpen) return;
		if (browser && document.activeElement instanceof HTMLElement) {
			this.returnTo = document.activeElement;
		}
		this.isOpen = true;
	}

	close() {
		if (!this.isOpen) return;
		this.isOpen = false;
		const target = this.returnTo;
		this.returnTo = null;
		if (target && target.isConnected) target.focus();
	}

	toggle() {
		if (this.isOpen) this.close();
		else this.open();
	}

	/** True on Apple platforms, where the shortcut is ⌘K rather than Ctrl K. */
	get isMac(): boolean {
		if (!browser) return false;
		return /Mac|iPhone|iPad|iPod/.test(navigator.platform ?? '');
	}

	/** Short label for hints: "⌘K" or "Ctrl K". */
	get shortcutLabel(): string {
		return this.isMac ? '⌘K' : 'Ctrl K';
	}

	/** True when a keyboard event is the palette shortcut. */
	matches(event: KeyboardEvent): boolean {
		if (event.key.toLowerCase() !== 'k') return false;
		if (event.altKey || event.shiftKey) return false;
		// Either modifier works: a Mac user on a PC keyboard should not have to guess.
		return event.metaKey || event.ctrlKey;
	}
}

export const palette = new Palette();
