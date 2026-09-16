/**
 * Docker actions and Plakar backups for an agent device.
 *
 * Kept next to the panels that use them: the endpoints are specific to the
 * device page and mirror `crates/server/src/api/agent_commands.rs` exactly.
 * Every call goes through the shared `request` helper — no `fetch` here.
 */
import { ApiError, request } from '$lib/api/client';
import type { TargetId } from '$lib/api';
export { getAgentHost, type AgentHost } from '$lib/api';

export type ContainerHealth = 'none' | 'healthy' | 'unhealthy' | 'starting';

/**
 * `cancelled` is a person pulling a queued command back; `expired` is the
 * server giving up after ten minutes because no agent came to fetch it.
 */
export type CommandStatus = 'queued' | 'running' | 'done' | 'failed' | 'cancelled' | 'expired';

export type CommandKind = 'container.restart' | 'container.update';

export interface ContainerPolicy {
	auto_restart: boolean;
	auto_update: boolean;
	prune_old_image: boolean;
	only_in_maintenance: boolean;
}

export const DEFAULT_POLICY: ContainerPolicy = {
	auto_restart: false,
	auto_update: false,
	prune_old_image: true,
	only_in_maintenance: true
};

export interface CommandView {
	id: number;
	target_id: TargetId;
	kind: CommandKind | string;
	args: Record<string, unknown>;
	status: CommandStatus;
	requested_by: string | null;
	created_at: string;
	started_at: string | null;
	finished_at: string | null;
	result: string | null;
}

export interface ContainerView {
	name: string;
	image: string;
	up: boolean;
	health: ContainerHealth;
	restart_count: number;
	uptime_seconds: number | null;
	image_age_seconds: number | null;
	/** `null` when the registry could not be asked (private image, auth failure). */
	update_available: boolean | null;
	policy: ContainerPolicy;
	last_command: CommandView | null;
}

/** Routes may be absent on an older server: a 404 then reads as "no data". */
async function anticipated<T>(promise: Promise<T>, fallback: T): Promise<T> {
	try {
		return await promise;
	} catch (cause) {
		if (cause instanceof ApiError && cause.missing) return fallback;
		throw cause;
	}
}

export function listContainers(id: TargetId, signal?: AbortSignal): Promise<ContainerView[]> {
	return anticipated(
		request<ContainerView[]>(`/targets/${id}/containers`, { signal, anticipated: true }),
		[]
	);
}

export function listCommands(id: TargetId, signal?: AbortSignal): Promise<CommandView[]> {
	return anticipated(
		request<CommandView[]>(`/targets/${id}/commands`, { signal, anticipated: true }),
		[]
	);
}

export function setContainerPolicy(
	id: TargetId,
	name: string,
	policy: ContainerPolicy
): Promise<ContainerPolicy> {
	return request<ContainerPolicy>(`/targets/${id}/containers/${encodeURIComponent(name)}/policy`, {
		method: 'PUT',
		body: policy
	});
}

export function restartContainer(id: TargetId, name: string): Promise<CommandView> {
	return request<CommandView>(`/targets/${id}/containers/${encodeURIComponent(name)}/restart`, {
		method: 'POST'
	});
}

export function updateContainer(id: TargetId, name: string, prune: boolean): Promise<CommandView> {
	return request<CommandView>(`/targets/${id}/containers/${encodeURIComponent(name)}/update`, {
		method: 'POST',
		body: { prune }
	});
}

/** Pulls a queued command back before the agent picks it up. `409` once it is running or done. */
export function cancelCommand(id: TargetId, commandId: number): Promise<void> {
	return request<void>(`/targets/${id}/commands/${commandId}`, { method: 'DELETE' });
}

/** True while the agent still has to act on it. */
export function isPending(command: CommandView | null | undefined): boolean {
	return command?.status === 'queued' || command?.status === 'running';
}

export function commandLabel(kind: string): string {
	if (kind === 'container.restart') return 'Restart';
	if (kind === 'container.update') return 'Update';
	return kind;
}

/** Container named by a command, read from its arguments. */
export function commandContainer(command: CommandView): string {
	const name = command.args?.name;
	return typeof name === 'string' ? name : '';
}

/** "1.2 GB" style, binary units as Docker and Plakar print them. */
export function formatBytes(bytes: number | null | undefined): string {
	if (bytes === null || bytes === undefined || !Number.isFinite(bytes)) return '—';
	const units = ['B', 'KiB', 'MiB', 'GiB', 'TiB'];
	let value = bytes;
	let index = 0;
	while (value >= 1024 && index < units.length - 1) {
		value /= 1024;
		index += 1;
	}
	const digits = index === 0 || value >= 100 ? 0 : 1;
	return `${value.toFixed(digits)} ${units[index]}`;
}

/** Whole-unit age: "3 h", "5 d" — for plates, where a decimal reads as noise. */
export function formatAge(seconds: number): string {
	if (seconds < 60) return `${Math.round(seconds)} s`;
	if (seconds < 3600) return `${Math.round(seconds / 60)} min`;
	if (seconds < 86400) return `${Math.round(seconds / 3600)} h`;
	return `${Math.round(seconds / 86400)} d`;
}
