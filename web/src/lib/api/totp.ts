/**
 * Two-factor authentication (TOTP) of the current account, the admin reset of
 * another account's second factor, and the security audit log.
 */
import { request } from './client';
import type { AuditEntry, TotpEnrolment, TotpStatus } from './types';

export function getTotpStatus(signal?: AbortSignal): Promise<TotpStatus> {
	return request<TotpStatus>('/auth/totp', { signal });
}

/** Proposes a new secret. A 401 means "wrong password", never "session expired". */
export function enrolTotp(password: string): Promise<TotpEnrolment> {
	return request<TotpEnrolment>('/auth/totp/enroll', {
		method: 'POST',
		body: { password },
		allowUnauthorized: true
	});
}

/** Confirms the enrolment with a first code; returns the recovery codes, shown once. */
export async function verifyTotp(code: string): Promise<string[]> {
	const reply = await request<{ recovery_codes: string[] }>('/auth/totp/verify', {
		method: 'POST',
		body: { code },
		allowUnauthorized: true
	});
	return reply.recovery_codes;
}

export function disableTotp(password: string): Promise<void> {
	return request<void>('/auth/totp', { method: 'DELETE', body: { password }, allowUnauthorized: true });
}

/** Admin: removes the second factor of another account (lost device). */
export function resetUserTotp(id: number): Promise<void> {
	return request<void>(`/users/${id}/totp`, { method: 'DELETE' });
}

export function listAuditLog(limit = 200, signal?: AbortSignal): Promise<AuditEntry[]> {
	return request<AuditEntry[]>('/auth/audit', { query: { limit }, signal });
}
