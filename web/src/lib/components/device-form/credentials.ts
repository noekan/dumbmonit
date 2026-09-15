/**
 * Credential drafting: the flat form state and its translation to the API.
 *
 * The API never returns a secret, so on edit the fields start empty and the
 * form only sends a `credential` when the user typed one (or explicitly chose
 * "No authentication"). Omitting it keeps the saved secret on the server.
 */
import {
	CREDENTIAL_KINDS,
	type Credential,
	type CredentialKind,
	type SnmpV3AuthProtocol,
	type SnmpV3PrivacyProtocol
} from '$lib/api';

export interface CredentialDraft {
	community: string;
	username: string;
	authProtocol: SnmpV3AuthProtocol;
	authPassphrase: string;
	privacyProtocol: SnmpV3PrivacyProtocol;
	privacyPassphrase: string;
	token: string;
	password: string;
}

export function emptyDraft(): CredentialDraft {
	return {
		community: '',
		username: '',
		authProtocol: 'sha256',
		authPassphrase: '',
		privacyProtocol: 'aes128',
		privacyPassphrase: '',
		token: '',
		password: ''
	};
}

const KNOWN_KINDS: readonly string[] = CREDENTIAL_KINDS.map((option) => option.value);

/**
 * Keeps only the credential families this build can render, in the server's
 * order (the first one is the recommended default). Empty means "offer all".
 */
export function allowedKinds(announced: string[]): CredentialKind[] {
	const kept = announced.filter((value): value is CredentialKind => KNOWN_KINDS.includes(value));
	return kept.length > 0 ? kept : CREDENTIAL_KINDS.map((option) => option.value);
}

/**
 * Maps the human label returned in `Target.credential_kind` back to a form
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

export function kindFromLabel(label: string | undefined, fallback: CredentialKind): CredentialKind {
	const key = label?.trim().toLowerCase().replace(/\s+/g, ' ') ?? '';
	return KIND_BY_LABEL[key] ?? fallback;
}

/** True when the user typed something in any secret field of the given kind. */
export function draftTouched(kind: CredentialKind, draft: CredentialDraft): boolean {
	switch (kind) {
		case 'snmp_community':
			return draft.community.trim() !== '';
		case 'snmp_v3':
			return (
				draft.username.trim() !== '' ||
				draft.authPassphrase.trim() !== '' ||
				draft.privacyPassphrase.trim() !== ''
			);
		case 'api_token':
			return draft.token.trim() !== '';
		case 'username_password':
			return draft.username.trim() !== '' || draft.password.trim() !== '';
		case 'none':
			return false;
	}
}

export function toCredential(kind: CredentialKind, draft: CredentialDraft): Credential {
	switch (kind) {
		case 'snmp_community':
			return { type: 'snmp_community', community: draft.community.trim() };
		case 'snmp_v3':
			return {
				type: 'snmp_v3',
				username: draft.username.trim(),
				auth: draft.authPassphrase
					? { protocol: draft.authProtocol, passphrase: draft.authPassphrase }
					: null,
				privacy: draft.privacyPassphrase
					? { protocol: draft.privacyProtocol, passphrase: draft.privacyPassphrase }
					: null
			};
		case 'api_token':
			return { type: 'api_token', token: draft.token.trim() };
		case 'username_password':
			return {
				type: 'username_password',
				username: draft.username.trim(),
				password: draft.password
			};
		case 'none':
			return { type: 'none' };
	}
}

/** Field-level problems of a draft; empty when it can be sent. */
export type CredentialErrors = Partial<Record<keyof CredentialDraft, string>>;

export function validateCredential(kind: CredentialKind, draft: CredentialDraft): CredentialErrors {
	const errors: CredentialErrors = {};
	switch (kind) {
		case 'snmp_community':
			if (!draft.community.trim()) errors.community = 'Enter the SNMP community.';
			break;
		case 'snmp_v3':
			if (!draft.username.trim()) errors.username = 'Enter the SNMP v3 user name.';
			if (!draft.authPassphrase.trim())
				errors.authPassphrase = 'Enter the authentication passphrase.';
			break;
		case 'api_token':
			if (!draft.token.trim()) errors.token = 'Enter the API token.';
			break;
		case 'username_password':
			if (!draft.username.trim()) errors.username = 'Enter the user name.';
			if (!draft.password) errors.password = 'Enter the password.';
			break;
		case 'none':
			break;
	}
	return errors;
}
