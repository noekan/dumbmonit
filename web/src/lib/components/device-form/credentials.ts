/**
 * Credential drafting: the flat form state and its translation to the API.
 *
 * The fields come from the server (`CollectorInfo.credentials`): one entry per
 * field key, always a string. `toCredential` recomposes the JSON the API
 * expects for the families it knows (nested SNMP v3, Proxmox token in two
 * pieces) and sends the flat keys as they are for a family it does not.
 *
 * The API never returns a secret, so on edit the fields start empty and the
 * form only sends a `credential` when the user typed one (or explicitly chose
 * "No authentication"). Omitting it keeps the saved secret on the server.
 */
import type {
	Credential,
	CredentialField,
	CredentialKind,
	CredentialView,
	SnmpV3AuthProtocol,
	SnmpV3PrivacyProtocol
} from '$lib/api';

export type CredentialDraft = Record<string, string>;

/** Empty draft for a family: selects start on their first choice. */
export function emptyDraft(view: CredentialView): CredentialDraft {
	const draft: CredentialDraft = {};
	for (const field of view.fields) {
		draft[field.key] = field.input === 'select' ? (field.choices[0] ?? '') : '';
	}
	return draft;
}

/**
 * Maps the human label returned in `Target.credential_kind` back to a family
 * kind. Older servers answer in French, newer ones in English: both are
 * accepted, case-insensitively, so edit works against either build.
 */
const KIND_BY_LABEL: Record<string, CredentialKind> = {
	'snmp community': 'snmp_community',
	'communauté snmp': 'snmp_community',
	'snmp v3': 'snmp_v3',
	'api token': 'api_token',
	"jeton d'api": 'api_token',
	'username / password': 'username_password',
	'utilisateur / mot de passe': 'username_password',
	none: 'none',
	aucune: 'none'
};

export function kindFromLabel(label: string | undefined, fallback: string): string {
	const key = label?.trim().toLowerCase().replace(/\s+/g, ' ') ?? '';
	return KIND_BY_LABEL[key] ?? fallback;
}

/** The family to show first: what the target uses, else the server's first. */
export function initialView(views: CredentialView[], savedLabel: string | undefined): CredentialView {
	const first = views[0] ?? { kind: 'none', label: 'No authentication', help: '', fields: [] };
	const kind = kindFromLabel(savedLabel, first.kind);
	return views.find((view) => view.kind === kind) ?? first;
}

/** True when the user typed something in any field (selects do not count). */
export function draftTouched(view: CredentialView, draft: CredentialDraft): boolean {
	return view.fields.some((field) => field.input !== 'select' && (draft[field.key] ?? '').trim() !== '');
}

/**
 * Cleans a pasted value: surrounding whitespace and line breaks, and the
 * quotes a shell snippet or a JSON export wraps a secret in.
 */
export function sanitizeSecret(value: string): string {
	let out = value.replace(/[\r\n]+/g, '').trim();
	for (const quote of ['"', "'"]) {
		if (out.length >= 2 && out.startsWith(quote) && out.endsWith(quote)) {
			out = out.slice(1, -1).trim();
			break;
		}
	}
	return out;
}

/**
 * A Proxmox token pasted whole (`user@realm!name=secret`) into the Token ID
 * field: the two halves, so the form can move the secret where it belongs.
 */
export function splitPastedToken(value: string): { token_id: string; secret: string } | null {
	const clean = sanitizeSecret(value);
	const at = clean.indexOf('=');
	if (at <= 0 || !clean.slice(0, at).includes('!')) return null;
	return { token_id: clean.slice(0, at), secret: clean.slice(at + 1) };
}

function value(draft: CredentialDraft, key: string): string {
	return (draft[key] ?? '').trim();
}

export function toCredential(view: CredentialView, draft: CredentialDraft): Credential {
	switch (view.kind) {
		case 'none':
			return { type: 'none' };
		case 'snmp_community':
			return { type: 'snmp_community', community: value(draft, 'community') };
		case 'snmp_v3': {
			const authPassphrase = draft.auth_passphrase ?? '';
			const privacyPassphrase = draft.privacy_passphrase ?? '';
			return {
				type: 'snmp_v3',
				username: value(draft, 'username'),
				auth: authPassphrase
					? { protocol: (draft.auth_protocol || 'sha256') as SnmpV3AuthProtocol, passphrase: authPassphrase }
					: null,
				privacy: privacyPassphrase
					? {
							protocol: (draft.privacy_protocol || 'aes128') as SnmpV3PrivacyProtocol,
							passphrase: privacyPassphrase
						}
					: null
			};
		}
		case 'api_token':
			// Two pieces (Proxmox VE, PBS) or one string (bearer token): the
			// server assembles the former and stores both the same way.
			if (view.fields.some((field) => field.key === 'token_id')) {
				return { type: 'api_token', token_id: value(draft, 'token_id'), secret: sanitizeSecret(draft.secret ?? '') };
			}
			return { type: 'api_token', token: sanitizeSecret(draft.token ?? '') };
		case 'username_password':
			return { type: 'username_password', username: value(draft, 'username'), password: draft.password ?? '' };
		default: {
			// A family this build does not know: send every field under its key.
			const out: Record<string, string> = { type: view.kind };
			for (const field of view.fields) out[field.key] = draft[field.key] ?? '';
			return out as Credential;
		}
	}
}

/** Field-level problems of a draft, by field key; empty when it can be sent. */
export type CredentialErrors = Record<string, string>;

function requiredMessage(field: CredentialField): string {
	return `Enter the ${field.label.toLowerCase()}.`;
}

export function validateCredential(view: CredentialView, draft: CredentialDraft): CredentialErrors {
	const errors: CredentialErrors = {};
	for (const field of view.fields) {
		const current = field.input === 'password' ? (draft[field.key] ?? '') : value(draft, field.key);
		if (field.required && !current) {
			errors[field.key] = requiredMessage(field);
		} else if (field.key === 'token_id' && current && !/^[^@!=\s]+@[^@!=\s]+![^@!=\s]+$/.test(current)) {
			errors[field.key] = 'Expected user@realm!name, exactly as Proxmox shows it.';
		}
	}
	return errors;
}
