/**
 * Backup and restore of the whole instance. Mirrors
 * `crates/server/src/api/backup.rs`.
 *
 * The export is a `POST` even though it changes nothing: the passphrase travels
 * in the body, never in a URL that a proxy log or the browser history would
 * keep. The server answers with the bundle itself; `saveBundle` turns it into a
 * file the browser downloads, so no raw `fetch` is needed here.
 */
import { request } from './client';
import type { BackupEnvelope, BackupStatus, RestoreReport } from './types';

export function getBackupStatus(signal?: AbortSignal): Promise<BackupStatus> {
	return request<BackupStatus>('/backup', { signal });
}

/** Produces the bundle. `includeAccountSecrets` also carries password hashes and 2FA secrets. */
export function exportBackup(
	passphrase: string,
	includeAccountSecrets = false
): Promise<BackupEnvelope> {
	return request<BackupEnvelope>('/backup', {
		method: 'POST',
		body: { passphrase, include_account_secrets: includeAccountSecrets }
	});
}

/** Dry run by default: the report says what would happen, nothing is written. */
export function restoreBackup(
	bundle: BackupEnvelope,
	passphrase: string,
	apply = false
): Promise<RestoreReport> {
	return request<RestoreReport>('/backup/restore', {
		method: 'POST',
		body: { bundle, passphrase, apply }
	});
}

/** Writes one scheduled-style local backup right now. */
export function runLocalBackup(): Promise<BackupStatus['schedule']> {
	return request<BackupStatus['schedule']>('/backup/local', { method: 'POST' });
}

/** Hands the bundle to the browser as a file. */
export function saveBundle(bundle: BackupEnvelope): void {
	const stamp = new Date().toISOString().slice(0, 19).replace(/[-:]/g, '').replace('T', '-');
	const blob = new Blob([JSON.stringify(bundle, null, 2)], { type: 'application/json' });
	const url = URL.createObjectURL(blob);
	const anchor = document.createElement('a');
	anchor.href = url;
	anchor.download = `dumbmonit-backup-${stamp}.json`;
	document.body.append(anchor);
	anchor.click();
	anchor.remove();
	setTimeout(() => URL.revokeObjectURL(url), 1000);
}
