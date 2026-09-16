<script lang="ts">
	/**
	 * Containers of an agent device: one compact row per container with its
	 * state, image, policy switches and the two actions the agent can carry out
	 * (restart, update). The command channel is asynchronous — the agent picks
	 * commands up after its next batch — so a fired action shows as "Queued",
	 * then "Running…", and the list polls faster until it settles.
	 *
	 * The whole panel folds (closed by default: thirty rows is a wall), and each
	 * row folds again: one status line, the switches, actions and the
	 * container's own charts only once it is opened.
	 */
	import type { Target } from '$lib/api';
	import { formatDuration, formatRelative } from '$lib/format';
	import { Button, Confirm, CopyBlock, ErrorNotice, Led, Plate, Skeleton, Toggle, type Tone } from '$lib/ui';
	import Chart from '$lib/components/Chart.svelte';
	import FoldSection from '../FoldSection.svelte';
	import FoldRow from '../FoldRow.svelte';
	import type { DeviceMetricGroup } from '../metrics';
	import {
		cancelCommand,
		commandContainer,
		commandLabel,
		isPending,
		restartContainer,
		updateContainer,
		type CommandStatus,
		type CommandView,
		type ContainerPolicy,
		type ContainerView
	} from './api';
	import { fleetFor } from './fleet.svelte';

	interface Props {
		target: Target;
		/** Charts per container name, over the page's range; drawn only inside an open row. */
		metrics?: Map<string, DeviceMetricGroup[]>;
		loadingMetrics?: boolean;
	}

	let { target, metrics = new Map(), loadingMetrics = false }: Props = $props();

	// The list itself lives in the shared fleet: the summary strip under the
	// header reads the same one, so both stay in step with a single poll.
	const fleet = $derived(fleetFor(target.id));

	/** Per container: an action in flight, an inline error, the result unfolded. */
	let acting = $state<Record<string, boolean>>({});
	let rowError = $state<Record<string, unknown>>({});
	let showResult = $state<Record<string, boolean>>({});
	let showAllCommands = $state(false);
	/** Rows unfolded by the user; nothing is remembered across visits. */
	let openRows = $state<Record<string, boolean>>({});

	const recent = $derived(showAllCommands ? fleet.commands : fleet.commands.slice(0, 5));

	/** Header summary: "31 · 30 running · 2 updates available". */
	const summary = $derived.by(() => {
		if (fleet.loading || fleet.error || fleet.containers.length === 0) return undefined;
		const total = fleet.containers.length;
		const parts = [`${total}`, `${fleet.running} running`];
		if (fleet.running < total) parts.push(`${total - fleet.running} stopped`);
		if (fleet.updates > 0) parts.push(`${fleet.updates} ${fleet.updates === 1 ? 'update' : 'updates'} available`);
		return parts.join(' · ');
	});

	/** The tiny status line of a folded row. */
	function statusLine(c: ContainerView): string {
		const parts: string[] = [];
		if (c.up && c.uptime_seconds !== null) parts.push(`up ${formatDuration(c.uptime_seconds)}`);
		parts.push(`${c.restart_count} ${c.restart_count === 1 ? 'restart' : 'restarts'}`);
		if (c.image_age_seconds !== null) parts.push(`image ${formatDuration(c.image_age_seconds)} old`);
		return parts.join(' · ');
	}

	// --- Presentation -----------------------------------------------------------

	function stateOf(c: ContainerView): { tone: 'signal' | 'advisory' | 'warning'; word: string; blink: boolean } {
		if (!c.up) return { tone: 'warning', word: 'Stopped', blink: true };
		if (c.health === 'unhealthy') return { tone: 'warning', word: 'Unhealthy', blink: true };
		if (c.health === 'starting') return { tone: 'advisory', word: 'Starting', blink: false };
		return { tone: 'signal', word: 'Running', blink: false };
	}

	const STATUS_TONE: Record<CommandStatus, Tone> = {
		queued: 'ghost',
		running: 'info',
		done: 'signal',
		failed: 'warning',
		cancelled: 'muted',
		expired: 'advisory'
	};
	const STATUS_WORD: Record<CommandStatus, string> = {
		queued: 'Queued',
		running: 'Running…',
		done: 'Done',
		failed: 'Failed',
		cancelled: 'Cancelled',
		expired: 'Expired'
	};

	/**
	 * The agent must have said it runs commands. An older binary says nothing,
	 * `commands: false` says no: either way a queued action would only expire.
	 */
	const canAct = $derived(fleet.commandsSupported);
	/** Why not, in one clause: an agent that said "no", or one that said nothing. */
	const whyNot = $derived(
		fleet.agent?.commands_supported === false && fleet.agent.agent_version
			? `agent ${fleet.agent.agent_version} reports actions disabled (commands: false) or predates the command channel`
			: 'no agent has reported its capabilities yet'
	);
	// The token is not known here: the placeholder points at Settings → Agents.
	const origin = typeof window === 'undefined' ? 'http://server:8080' : window.location.origin;
	const installHint = `curl -sSL ${origin}/install.sh | sh -s -- --token=<token> --url=${origin}`;

	// --- Loading ------------------------------------------------------------------

	$effect(() => fleet.retain());

	// A pending command is worth a closer look: 5 s instead of the 30 s cadence.
	$effect(() => {
		fleet.poll(fleet.busy ? 5_000 : 30_000);
	});

	// --- Actions --------------------------------------------------------------------

	async function savePolicy(c: ContainerView, patch: Partial<ContainerPolicy>) {
		rowError = { ...rowError, [c.name]: null };
		const failure = await fleet.setPolicy(c, patch);
		if (failure) rowError = { ...rowError, [c.name]: failure };
	}

	/** Per command id: a cancel in flight. */
	let cancelling = $state<Record<number, boolean>>({});

	async function cancel(command: CommandView, container?: ContainerView) {
		cancelling = { ...cancelling, [command.id]: true };
		if (container) rowError = { ...rowError, [container.name]: null };
		try {
			await cancelCommand(target.id, command.id);
			await fleet.load();
		} catch (cause) {
			if (container) rowError = { ...rowError, [container.name]: cause };
		} finally {
			cancelling = { ...cancelling, [command.id]: false };
		}
	}

	async function act(c: ContainerView, kind: 'restart' | 'update') {
		acting = { ...acting, [c.name]: true };
		rowError = { ...rowError, [c.name]: null };
		try {
			const command =
				kind === 'restart'
					? await restartContainer(target.id, c.name)
					: await updateContainer(target.id, c.name, c.policy.prune_old_image);
			c.last_command = command;
			showResult = { ...showResult, [c.name]: false };
			await fleet.load();
		} catch (cause) {
			rowError = { ...rowError, [c.name]: cause };
		} finally {
			acting = { ...acting, [c.name]: false };
		}
	}
</script>

<FoldSection kind="containers" title="Containers" {summary} class="rise-in">
	{#if fleet.error}
		<div class="px-5 py-4">
			<ErrorNotice error={fleet.error} title="Could not load the containers" onretry={() => void fleet.load()} />
		</div>
	{:else if fleet.loading}
		<div class="flex flex-col gap-3 px-5 py-4" aria-busy="true" aria-label="Loading containers">
			<Skeleton class="h-12 w-full" rows={3} />
		</div>
	{:else if fleet.containers.length === 0}
		<p class="px-5 py-4 text-sm text-ink-2">No Docker on this machine, or the agent cannot reach its socket (add the agent to the docker group or run it as root).</p>
	{:else}
		{#if !canAct}
			<div class="mx-5 mt-4 rounded-lg border border-advisory/35 bg-advisory-soft px-3 py-2.5" role="status">
				<Plate tone="advisory" label="Actions unavailable" />
				<p class="mt-1.5 text-sm leading-relaxed text-ink">
					This agent cannot run commands — {whyNot}. Reinstall it with the current installer
					(token from Settings → Agents, <code class="font-mono text-[0.8125rem]">commands: true</code>);
					restart, update and the policies below stay off until it reports back.
				</p>
				<div class="mt-2">
					<CopyBlock value={installHint} label="Copy the install command" />
				</div>
			</div>
		{/if}
		<ul class="divide-y divide-line">
			{#each fleet.containers as c, i (c.name)}
				{@const s = stateOf(c)}
				{@const saving = fleet.saving[c.name] ?? false}
				{@const working = (acting[c.name] ?? false) || isPending(c.last_command)}
				{@const charts = metrics.get(c.name) ?? []}
				<FoldRow id={`container-${target.id}-${i}`} bind:open={openRows[c.name]} class="rise-in" style="--rise-delay: {Math.min(i, 8) * 40}ms">
					{#snippet header()}
						<span class="flex min-w-0 items-center gap-2">
							<Led tone={s.tone} blink={s.blink} label={s.word} />
							<span class="truncate font-semibold text-ink">{c.name}</span>
							<Plate tone={s.tone} label={s.word} bare />
							{#if c.update_available === true}
								<Plate tone="info" label="Update available" bare />
							{/if}
						</span>
						<span class="tnum truncate text-[0.8125rem] text-ink-2">{statusLine(c)}</span>
					{/snippet}
					{#snippet trailing()}
						{#if c.last_command && isPending(c.last_command)}
							<Plate tone={STATUS_TONE[c.last_command.status]} label={`${commandLabel(c.last_command.kind)} · ${STATUS_WORD[c.last_command.status]}`} pulse={c.last_command.status === 'running'} title={c.last_command.created_at} />
						{/if}
					{/snippet}

					<p class="text-sm text-ink-2 break-all">{c.image}</p>

					<!-- Policy switches and actions -->
					<div class="flex flex-col gap-3 md:flex-row md:items-center md:justify-between">
						<div class="flex flex-col gap-2 sm:flex-row sm:flex-wrap sm:gap-x-5">
							<div class="flex items-center gap-2">
								<Toggle id={`restart-${target.id}-${c.name}`} checked={c.policy.auto_restart} disabled={saving || !canAct} label="Restart if down" onchange={(v) => void savePolicy(c, { auto_restart: v })} />
								<label for={`restart-${target.id}-${c.name}`} class="text-sm text-ink">Restart if down</label>
							</div>
							<div class="flex items-center gap-2">
								<Toggle id={`update-${target.id}-${c.name}`} checked={c.policy.auto_update} disabled={saving || !canAct} label="Auto-update in maintenance windows" onchange={(v) => void savePolicy(c, { auto_update: v })} />
								<label for={`update-${target.id}-${c.name}`} class="text-sm text-ink">Auto-update in maintenance windows</label>
							</div>
						</div>
						<div class="flex flex-wrap items-center gap-2" aria-live="polite">
							{#if c.last_command}
								<Plate tone={STATUS_TONE[c.last_command.status]} label={`${commandLabel(c.last_command.kind)} · ${STATUS_WORD[c.last_command.status]}`} pulse={c.last_command.status === 'running'} title={c.last_command.created_at} />
								{#if c.last_command.result}
									<Button size="sm" variant="ghost" onclick={() => (showResult = { ...showResult, [c.name]: !showResult[c.name] })} aria-expanded={showResult[c.name] ?? false}>
										{showResult[c.name] ? 'Hide result' : 'Show result'}
									</Button>
								{/if}
							{/if}
							{#if c.last_command?.status === 'queued'}
								{@const queued = c.last_command}
								<Button size="sm" variant="ghost" onclick={() => void cancel(queued, c)} loading={cancelling[queued.id] ?? false} title="Pull the command back before the agent picks it up">Cancel</Button>
							{/if}
							{#if canAct}
								<Confirm size="sm" variant="secondary" confirmLabel="Restart now?" onconfirm={() => act(c, 'restart')} loading={acting[c.name] ?? false} disabled={working}>Restart</Confirm>
								<Confirm size="sm" variant="secondary" confirmLabel="Pull and replace?" onconfirm={() => act(c, 'update')} loading={acting[c.name] ?? false} disabled={working}>Update now</Confirm>
							{:else}
								<span class="text-sm text-ink-2">Restart and update need an agent that runs commands.</span>
							{/if}
						</div>
					</div>

					{#if c.last_command?.result && showResult[c.name]}
						<pre class="tnum max-h-64 overflow-auto rounded-lg bg-surface-2 px-3 py-2 text-xs whitespace-pre-wrap text-ink">{c.last_command.result}</pre>
					{/if}
					{#if rowError[c.name]}
						<ErrorNotice error={rowError[c.name]} title="The action could not be sent" />
					{/if}

					<!-- The container's own charts, over the page's range -->
					{#if loadingMetrics && charts.length === 0}
						<Skeleton class="h-32 w-full rounded-[var(--radius-card)]" />
					{:else if charts.length === 0}
						<p class="text-sm text-ink-2">No metric for this container in this range.</p>
					{:else}
						<div class="grid gap-3 lg:grid-cols-2">
							{#each charts as chart (chart.name)}
								<div class="rounded-lg border border-line px-3 pt-2 pb-1">
									<div class="flex items-center justify-between gap-2">
										<span class="text-sm font-semibold text-ink">{chart.title}</span>
										{#if chart.unit}<span class="label-tape">{chart.unit}</span>{/if}
									</div>
									<Chart series={chart.series} unit={chart.unit} height={140} />
								</div>
							{/each}
						</div>
					{/if}
				</FoldRow>
			{/each}
		</ul>

		<!-- History of what was asked of the agent -->
		<div class="graticule border-t border-line px-5 py-4">
			<h3 class="text-sm font-semibold text-ink">Recent actions</h3>
			{#if fleet.commands.length === 0}
				<p class="mt-1 text-sm text-ink-2">Nothing yet. Open a container and restart or update it.</p>
			{:else}
				<ul class="mt-2 flex flex-col gap-1.5" aria-live="polite">
					{#each recent as command (command.id)}
						<li class="flex flex-wrap items-center gap-x-2 gap-y-1 text-sm">
							<Plate tone={STATUS_TONE[command.status]} label={STATUS_WORD[command.status]} pulse={command.status === 'running'} />
							<span class="text-ink">{commandLabel(command.kind)} <span class="font-semibold break-all">{commandContainer(command)}</span></span>
							<span class="text-ink-2">by {command.requested_by ?? 'unknown'}</span>
							<span class="text-ink-3" aria-hidden="true">·</span>
							<span class="tnum text-ink-2" title={command.created_at}>{formatRelative(command.created_at)}</span>
							{#if command.status === 'queued'}
								<Button size="sm" variant="ghost" onclick={() => void cancel(command)} loading={cancelling[command.id] ?? false}>Cancel</Button>
							{/if}
							{#if command.result && !isPending(command)}
								<span class="min-w-0 basis-full truncate text-ink-2" title={command.result}>{command.result.split('\n').at(-1)}</span>
							{/if}
						</li>
					{/each}
				</ul>
				{#if fleet.commands.length > 5}
					<Button size="sm" variant="ghost" class="mt-2" onclick={() => (showAllCommands = !showAllCommands)}>
						{showAllCommands ? 'Show fewer' : `Show all (${fleet.commands.length})`}
					</Button>
				{/if}
			{/if}
		</div>
	{/if}
</FoldSection>
