/**
 * Presentation helpers shared by the Proxmox VE panels.
 *
 * Proxmox prints binary units everywhere in its own interface; matching it
 * means a number read here and a number read there are the same number.
 */
/** The tones a Led accepts — a narrower set than Plate's. */
type SignalTone = 'signal' | 'advisory' | 'warning' | 'info' | 'ghost';

/** "1.2 GiB" style, binary units as Proxmox prints them. */
export function formatBytes(bytes: number | null | undefined): string {
	if (bytes === null || bytes === undefined || !Number.isFinite(bytes)) return '—';
	const units = ['B', 'KiB', 'MiB', 'GiB', 'TiB', 'PiB'];
	let value = bytes;
	let index = 0;
	while (value >= 1024 && index < units.length - 1) {
		value /= 1024;
		index += 1;
	}
	return `${value.toFixed(index === 0 || value >= 100 ? 0 : 1)} ${units[index]}`;
}

export function formatPercent(value: number | null | undefined): string {
	if (value === null || value === undefined || !Number.isFinite(value)) return '—';
	return `${value >= 10 ? Math.round(value) : Math.round(value * 10) / 10} %`;
}

export type Fill = 'signal' | 'advisory' | 'warning';

/** Meter tone by fill: past 90 % is a warning, past 75 % an advisory. */
export function fillTone(percent: number | null | undefined, warn = 90, advise = 75): Fill {
	if (percent === null || percent === undefined || !Number.isFinite(percent)) return 'signal';
	if (percent >= warn) return 'warning';
	if (percent >= advise) return 'advisory';
	return 'signal';
}

export const FILL: Record<Fill, string> = {
	signal: 'bg-signal',
	advisory: 'bg-advisory',
	warning: 'bg-warning'
};

/** Ceph health as a word and a tone: 0 OK, 1 WARN, 2 ERR, anything else unknown. */
export function cephHealth(level: number | null, status: string | null): { tone: SignalTone; word: string } {
	if (level === 0) return { tone: 'signal', word: status ?? 'HEALTH_OK' };
	if (level === 1) return { tone: 'advisory', word: status ?? 'HEALTH_WARN' };
	if (level === 2) return { tone: 'warning', word: status ?? 'HEALTH_ERR' };
	return { tone: 'ghost', word: status ?? 'Unknown' };
}
