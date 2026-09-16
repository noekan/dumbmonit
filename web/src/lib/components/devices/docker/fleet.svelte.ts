/**
 * Containers of one agent device, shared by the summary strip under the
 * header and the folded Containers section below: one poll, one list, both
 * views in step. The first subscriber starts the poll, the last stops it;
 * a pending command tightens the cadence to five seconds.
 */
import type { TargetId } from '$lib/api';
import {
	getAgentHost,
	isPending,
	listCommands,
	listContainers,
	setContainerPolicy,
	type AgentHost,
	type CommandView,
	type ContainerPolicy,
	type ContainerView
} from './api';

export class ContainerFleet {
	containers = $state<ContainerView[]>([]);
	commands = $state<CommandView[]>([]);
	/** The agent as it last described itself; `null` until it has reported (or on an older server). */
	agent = $state<AgentHost | null>(null);
	loading = $state(true);
	error = $state<unknown>(null);
	/** Container names whose policy is being saved right now. */
	saving = $state<Record<string, boolean>>({});

	readonly busy = $derived(this.containers.some((c) => isPending(c.last_command)));
	/** False for an agent too old for the command channel, or with `commands: false`: no Restart/Update then. */
	readonly commandsSupported = $derived(this.agent?.commands_supported === true);
	readonly running = $derived(this.containers.filter((c) => c.up).length);
	readonly updates = $derived(this.containers.filter((c) => c.update_available === true).length);
	readonly autoRestart = $derived(this.containers.filter((c) => c.policy.auto_restart).length);
	readonly autoUpdate = $derived(this.containers.filter((c) => c.policy.auto_update).length);

	#id: TargetId;
	#subscribers = 0;
	#controller: AbortController | null = null;
	#timer: ReturnType<typeof setInterval> | null = null;
	#period = 0;

	constructor(id: TargetId) {
		this.#id = id;
	}

	get id(): TargetId {
		return this.#id;
	}

	async load(signal?: AbortSignal) {
		this.error = null;
		try {
			const [list, history, agent] = await Promise.all([
				listContainers(this.#id, signal),
				listCommands(this.#id, signal),
				getAgentHost(this.#id, signal)
			]);
			this.containers = list;
			this.commands = history;
			this.agent = agent;
		} catch (cause) {
			if (cause instanceof DOMException && cause.name === 'AbortError') return;
			this.error = cause;
		} finally {
			this.loading = false;
		}
	}

	/** Saves one container's policy; the row keeps its previous value on failure. Returns the error, if any. */
	async setPolicy(c: ContainerView, patch: Partial<ContainerPolicy>): Promise<unknown> {
		const previous = c.policy;
		const next = { ...previous, ...patch };
		this.saving = { ...this.saving, [c.name]: true };
		c.policy = next;
		try {
			c.policy = await setContainerPolicy(this.#id, c.name, next);
			return null;
		} catch (cause) {
			c.policy = previous;
			return cause;
		} finally {
			this.saving = { ...this.saving, [c.name]: false };
		}
	}

	/** Call from a `$effect`; returns the release function. */
	retain(): () => void {
		this.#subscribers += 1;
		if (this.#subscribers === 1) {
			this.#controller = new AbortController();
			void this.load(this.#controller.signal);
		}
		return () => {
			this.#subscribers -= 1;
			if (this.#subscribers === 0) this.#stop();
		};
	}

	/** (Re)starts the poll at the given period; a no-op when already at that period. */
	poll(period: number) {
		if (this.#subscribers === 0 || this.#period === period) return;
		this.#period = period;
		if (this.#timer) clearInterval(this.#timer);
		this.#timer = setInterval(() => void this.load(this.#controller?.signal), period);
	}

	#stop() {
		this.#controller?.abort();
		this.#controller = null;
		if (this.#timer) clearInterval(this.#timer);
		this.#timer = null;
		this.#period = 0;
		this.loading = true;
		this.containers = [];
		this.commands = [];
		this.agent = null;
	}
}

const fleets = new Map<TargetId, ContainerFleet>();

/** The fleet of a device — the same instance for every component of the page. */
export function fleetFor(id: TargetId): ContainerFleet {
	let fleet = fleets.get(id);
	if (!fleet) {
		fleet = new ContainerFleet(id);
		fleets.set(id, fleet);
	}
	return fleet;
}
