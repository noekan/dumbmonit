/**
 * Copy text to the clipboard, working in non-secure contexts too.
 *
 * The async Clipboard API only exists on https / localhost. Everybody runs this
 * over plain http on a LAN IP, so we fall back to the legacy
 * `document.execCommand('copy')` on a hidden textarea. Resolves `true` only when
 * a method actually worked; never throws.
 */
export async function copyText(value: string): Promise<boolean> {
	if (typeof document === 'undefined') return false;
	if (typeof navigator !== 'undefined' && navigator.clipboard && window.isSecureContext) {
		try {
			await navigator.clipboard.writeText(value);
			return true;
		} catch {
			/* Permission denied or unsupported: try the legacy path. */
		}
	}
	return copyLegacy(value);
}

function copyLegacy(value: string): boolean {
	if (typeof document.execCommand !== 'function') return false;
	const active = document.activeElement;
	const area = document.createElement('textarea');
	area.value = value;
	area.readOnly = true;
	area.tabIndex = -1;
	area.setAttribute('aria-hidden', 'true');
	// Off-screen without affecting layout or scroll position.
	area.style.cssText = 'position:fixed;top:0;left:0;width:1px;height:1px;padding:0;border:0;opacity:0;';
	document.body.appendChild(area);
	let ok = false;
	try {
		area.focus({ preventScroll: true });
		area.select();
		area.setSelectionRange(0, value.length);
		ok = document.execCommand('copy');
	} catch {
		ok = false;
	} finally {
		area.remove();
		if (active instanceof HTMLElement) active.focus({ preventScroll: true });
	}
	return ok;
}

/** Select the whole content of an element so the user can Ctrl+C after a failed copy. */
export function selectContents(el: Element | null | undefined): void {
	if (!el || typeof document === 'undefined') return;
	const selection = document.getSelection();
	if (!selection) return;
	const range = document.createRange();
	range.selectNodeContents(el);
	selection.removeAllRanges();
	selection.addRange(range);
}
