/**
 * Channel-kind helpers for the settings screens.
 *
 * The server describes every channel kind (`GET /api/notify/kinds`). Besides
 * the fields declared in `ChannelField`, it also sends `shape` and `default`
 * per field: a `list` field must be sent as a JSON array (one entry per line
 * in the textarea) and an `object` field as a JSON object, otherwise the
 * notifier reads "no recipient". The API types do not declare them yet, so
 * they are read here, defensively, and defaulted for older servers.
 */
import type { ChannelField, ChannelKindInfo } from '$lib/api';
import type { Icon as LucideIcon } from 'lucide-svelte';
import {
	Bell,
	BellRing,
	Hash,
	House,
	Mail,
	MessageCircle,
	MessageSquare,
	Send,
	Siren,
	Smartphone,
	Webhook
} from 'lucide-svelte';

export type FieldShape = 'scalar' | 'list' | 'object';

export interface KindField extends ChannelField {
	shape: FieldShape;
	/** Value the server applies when the field is left empty; '' when none. */
	default: string;
}

export interface KindInfo extends ChannelKindInfo {
	settings: KindField[];
	secrets: KindField[];
}

const INPUTS: ChannelField['input'][] = ['text', 'url', 'number', 'boolean', 'select', 'textarea', 'password'];

export function normalizeField(raw: Partial<ChannelField> & { key: string; shape?: unknown; default?: unknown }): KindField {
	const input = INPUTS.includes(raw.input as ChannelField['input']) ? (raw.input as ChannelField['input']) : 'text';
	const shape: FieldShape = raw.shape === 'list' || raw.shape === 'object' ? raw.shape : 'scalar';
	return {
		key: raw.key,
		label: raw.label?.trim() || raw.key,
		required: raw.required === true,
		input,
		help: typeof raw.help === 'string' ? raw.help : '',
		placeholder: typeof raw.placeholder === 'string' ? raw.placeholder : '',
		options: Array.isArray(raw.options) ? raw.options.filter((o): o is string => typeof o === 'string') : [],
		shape,
		default: typeof raw.default === 'string' ? raw.default : ''
	};
}

/** Completes a catalogue entry so it can always be displayed, even from an older server. */
export function normalizeKind(raw: Partial<ChannelKindInfo> & { kind: string }): KindInfo {
	const fields = (list: unknown): KindField[] =>
		Array.isArray(list)
			? list.filter((f): f is ChannelField & { key: string } => !!f && typeof f.key === 'string').map(normalizeField)
			: [];
	return {
		kind: raw.kind,
		label: raw.label?.trim() || raw.kind,
		summary: raw.summary?.trim() ?? '',
		doc_url: raw.doc_url?.trim() ?? '',
		settings: fields(raw.settings),
		secrets: fields(raw.secrets)
	};
}

const KIND_ICON: Record<string, typeof LucideIcon> = {
	discord: MessageCircle,
	slack: Hash,
	telegram: Send,
	email: Mail,
	smtp: Mail,
	webhook: Webhook,
	ntfy: BellRing,
	gotify: BellRing,
	pushover: BellRing,
	pushbullet: BellRing,
	bark: BellRing,
	pagerduty: Siren,
	opsgenie: Siren,
	teams: MessageSquare,
	matrix: MessageSquare,
	mattermost: MessageSquare,
	rocketchat: MessageSquare,
	googlechat: MessageSquare,
	zulip: MessageSquare,
	signal: MessageCircle,
	twilio: Smartphone,
	homeassistant: House
};

/** The icon for a channel kind; a bell for a kind this build does not know. */
export function kindIcon(kind: string): typeof LucideIcon {
	return KIND_ICON[kind] ?? Bell;
}

/** Documentation anchor of a kind: the server's `doc_url`, or the catalogue page. */
export function kindDocUrl(info: KindInfo | undefined): string {
	return info?.doc_url || 'https://dumbmonit.readthedocs.io/en/latest/notifications/';
}
