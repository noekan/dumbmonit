/**
 * Presentation knowledge about collector kinds.
 *
 * The list of kinds itself comes from the server; only the icon and the
 * grouping are decided here, with a neutral fallback for a kind this build
 * does not know.
 */
import type { Icon as LucideIcon } from 'lucide-svelte';
import {
	Antenna,
	Archive,
	AtSign,
	Boxes,
	BrickWall,
	Cable,
	Cpu,
	Database,
	Globe,
	HardDrive,
	HeartPulse,
	LayoutGrid,
	Mail,
	MailCheck,
	Network,
	Plug,
	Radio,
	Server,
	ServerCog,
	ShieldCheck
} from 'lucide-svelte';
import type { CollectorInfo } from '$lib/api';
import { isUptimeKind, PUSH_KIND } from '$lib/format';

const KIND_ICON: Record<string, typeof LucideIcon> = {
	snmp: Network,
	proxmox: Boxes,
	pbs: Archive,
	synology: HardDrive,
	agent: Cpu,
	http: Globe,
	tcp: Plug,
	dns: AtSign,
	ping: Radio,
	tls: ShieldCheck,
	smtp: Mail,
	postgres: Database,
	mysql: Database,
	mqtt: Antenna,
	websocket: Cable,
	push: HeartPulse,
	pdm: LayoutGrid,
	pmg: MailCheck,
	opnsense: BrickWall,
	truenas: HardDrive,
	redfish: ServerCog
};

export function kindIcon(kind: string): typeof LucideIcon {
	return KIND_ICON[kind] ?? Server;
}

/** Kinds that describe a machine the collector polls, in display order. */
const DEVICE_KINDS = [
	'snmp',
	'proxmox',
	'pbs',
	'pdm',
	'pmg',
	'synology',
	'truenas',
	'opnsense',
	'redfish',
	'agent'
];

export interface KindGroup {
	id: 'devices' | 'services' | 'other';
	title: string;
	collectors: CollectorInfo[];
}

/**
 * Splits the server's kinds into "Devices", "Services" and "Other".
 *
 * Known device kinds keep a fixed order so the picker reads the same on every
 * instance; services are recognised by `isUptimeKind`; anything else (a demo
 * collector, a kind added by a newer server) lands in "Other" untouched.
 */
export function groupCollectors(collectors: CollectorInfo[]): KindGroup[] {
	const devices = collectors
		.filter((c) => DEVICE_KINDS.includes(c.kind))
		.sort((a, b) => DEVICE_KINDS.indexOf(a.kind) - DEVICE_KINDS.indexOf(b.kind));
	// Heartbeats sit with the services: they watch a job, not a machine.
	const isService = (kind: string) => isUptimeKind(kind) || kind === PUSH_KIND;
	const services = collectors.filter((c) => isService(c.kind));
	const other = collectors.filter((c) => !DEVICE_KINDS.includes(c.kind) && !isService(c.kind));
	const groups: KindGroup[] = [
		{ id: 'devices', title: 'Devices', collectors: devices },
		{ id: 'services', title: 'Services', collectors: services },
		{ id: 'other', title: 'Other', collectors: other }
	];
	return groups.filter((group) => group.collectors.length > 0);
}

/** The kind that enrols itself: no address form, an install command instead. */
export const AGENT_KIND = 'agent';
/** The only kind a network scan can find. */
export const SNMP_KIND = 'snmp';

/** Check intervals offered by the form. The server default is 60 s. */
export const INTERVALS = [
	{ value: 30, label: '30 s' },
	{ value: 60, label: '1 min' },
	{ value: 120, label: '2 min' },
	{ value: 300, label: '5 min' },
	{ value: 900, label: '15 min' }
] as const;

export const DEFAULT_INTERVAL = 60;

/**
 * Suggests a display name from an address: the host without scheme, port or
 * path. `https://example.org/health` becomes `example.org`.
 */
export function suggestName(address: string): string {
	const trimmed = address.trim();
	if (!trimmed) return '';
	const withoutScheme = trimmed.replace(/^[a-z][a-z0-9+.-]*:\/\//i, '');
	const host = withoutScheme.split(/[/?#]/)[0] ?? '';
	// IPv6 literals keep their brackets; anything else drops a trailing :port.
	if (host.startsWith('[')) return host.slice(0, host.indexOf(']') + 1);
	return host.replace(/:\d+$/, '') || trimmed;
}
