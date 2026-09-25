/**
 * Presentation helpers for the firewall panels: Unix seconds and raw counters
 * in, words out. The API speaks Unix seconds because OPNsense does, while the
 * rest of the interface speaks server date strings — hence these local
 * variants. A missing measurement prints nothing rather than a zero.
 */
import { formatDateTime } from '$lib/format';
import type { Tone } from '$lib/ui';
import type { OpnsenseGatewayRow, OpnsenseTunnelRow } from '$lib/api';

export function formatUnix(seconds: number | null | undefined): string {
	if (seconds === null || seconds === undefined || !Number.isFinite(seconds)) return 'never';
	return formatDateTime(new Date(seconds * 1000));
}

/** Whole-unit span: "40 s", "12 min", "3 h", "5 d". */
export function formatSpan(seconds: number): string {
	if (seconds < 60) return `${Math.round(seconds)} s`;
	if (seconds < 3600) return `${Math.round(seconds / 60)} min`;
	if (seconds < 86400) {
		const hours = Math.floor(seconds / 3600);
		const minutes = Math.round((seconds % 3600) / 60);
		return minutes > 0 && hours < 10 ? `${hours} h ${minutes} min` : `${hours} h`;
	}
	return `${Math.round(seconds / 86400)} d`;
}

/** "3 h ago", or "never". Negative ages (the future) read as "in 2 h". */
export function formatAgo(seconds: number | null | undefined): string {
	if (seconds === null || seconds === undefined || !Number.isFinite(seconds)) return 'never';
	const delta = Math.round(Date.now() / 1000 - seconds);
	if (delta < 0) return `in ${formatSpan(-delta)}`;
	if (delta < 45) return 'just now';
	return `${formatSpan(delta)} ago`;
}

/** A plain count, grouped: "1 204". */
export function formatCount(value: number | null | undefined): string {
	if (value === null || value === undefined || !Number.isFinite(value)) return '—';
	return Math.round(value).toLocaleString('en-US').replace(/,/g, ' ');
}

/** Binary sizes, as the interface counters report them. */
export function formatBytes(bytes: number | null | undefined): string {
	if (bytes === null || bytes === undefined || !Number.isFinite(bytes)) return '—';
	const units = ['B', 'KiB', 'MiB', 'GiB', 'TiB', 'PiB'];
	let value = bytes;
	let index = 0;
	while (value >= 1024 && index < units.length - 1) {
		value /= 1024;
		index += 1;
	}
	const digits = index === 0 || value >= 100 ? 0 : 1;
	return `${value.toFixed(digits)} ${units[index]}`;
}

/** Percent of a total, or `null` when the total is missing or zero. */
export function percentOf(used: number | null, total: number | null): number | null {
	if (used === null || total === null || !Number.isFinite(used) || !Number.isFinite(total)) {
		return null;
	}
	if (total <= 0) return null;
	return Math.min(100, Math.max(0, (used / total) * 100));
}

/** A finite number, or `null`: the API sends `null` for what it could not read. */
export function reading(value: number | null | undefined): number | null {
	return value === null || value === undefined || !Number.isFinite(value) ? null : value;
}

/**
 * Gateway plate. Only `down` and `force_down` are outages — dpinger's `loss`
 * and `delay` mean the link still answers, badly, which is an advisory and not
 * a reason to wake anyone. A gateway nobody watches says so plainly rather
 * than passing for healthy.
 */
export function gatewayTone(gateway: OpnsenseGatewayRow): { tone: Tone; label: string } {
	switch (gateway.status) {
		case 'online':
			return { tone: 'signal', label: 'Online' };
		case 'down':
			return { tone: 'warning', label: 'Down' };
		case 'force_down':
			return { tone: 'warning', label: 'Forced down' };
		case 'loss':
			return { tone: 'advisory', label: 'Packet loss' };
		case 'delay':
			return { tone: 'advisory', label: 'Latency' };
		case 'delay+loss':
		case 'loss+delay':
			return { tone: 'advisory', label: 'Loss and latency' };
		default:
			return gateway.monitored
				? { tone: 'ghost', label: gateway.status || 'Unknown' }
				: { tone: 'ghost', label: 'Not monitored' };
	}
}

/** Tunnel plate: carrying a session, silent, or with no session at all. */
export function tunnelTone(tunnel: OpnsenseTunnelRow): { tone: Tone; label: string } {
	if (tunnel.down) return { tone: 'warning', label: 'Down' };
	if (tunnel.up === null) return { tone: 'ghost', label: 'Unknown' };
	if (tunnel.silent) return { tone: 'advisory', label: 'Silent' };
	return { tone: 'signal', label: 'Up' };
}

/** How each VPN technology is written on screen. */
export const TUNNEL_KIND_LABEL: Record<string, string> = {
	wireguard: 'WireGuard',
	openvpn: 'OpenVPN',
	ipsec: 'IPsec'
};

export function tunnelKindLabel(kind: string): string {
	return TUNNEL_KIND_LABEL[kind] ?? kind;
}

/** CARP plate: the master, a standby, or a VIP that is not carrying anything. */
export function carpTone(status: string): { tone: Tone; label: string } {
	switch (status.toUpperCase()) {
		case 'MASTER':
			return { tone: 'signal', label: 'MASTER' };
		case 'BACKUP':
			return { tone: 'info', label: 'BACKUP' };
		case 'DISABLED':
			return { tone: 'ghost', label: 'DISABLED' };
		default:
			return { tone: 'advisory', label: status.toUpperCase() || 'UNKNOWN' };
	}
}

/** Fill colours of the little usage bars, by how full they are. */
export const FILL: Record<'signal' | 'advisory' | 'warning', string> = {
	signal: 'bg-signal',
	advisory: 'bg-advisory',
	warning: 'bg-warning'
};

/** A bar turns advisory past three quarters, warning past nine tenths. */
export function fillTone(percent: number | null): 'signal' | 'advisory' | 'warning' {
	if (percent === null) return 'signal';
	if (percent >= 90) return 'warning';
	if (percent >= 75) return 'advisory';
	return 'signal';
}
