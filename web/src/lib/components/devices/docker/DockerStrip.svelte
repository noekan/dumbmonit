<script lang="ts">
	/**
	 * The Docker line under an agent's header: how many containers, how many
	 * run, how many could be updated, and what the policies cover — with the
	 * two things a person comes here for: reach the container list, or set the
	 * restart / auto-update policies for the whole fleet at once. The bulk
	 * editor unfolds inline; each switch saves its own row, so a failure only
	 * ever concerns one container.
	 *
	 * Rendered only once the fleet is known and not empty: without Docker there
	 * is nothing to summarise, and the Containers section says so below.
	 */
	import { Container } from 'lucide-svelte';
	import type { Target } from '$lib/api';
	import { formatRelative } from '$lib/format';
	import { Button, Plate, Toggle, type Tone } from '$lib/ui';
	import { openFold } from '../fold.svelte';
	import { toApiError } from '$lib/api/client';
	import { commandContainer, commandLabel, type CommandStatus, type ContainerPolicy, type ContainerView } from './api';
	import { fleetFor } from './fleet.svelte';

	interface Props {
		target: Target;
	}

	let { target }: Props = $props();

	const fleet = $derived(fleetFor(target.id));

	$effect(() => fleet.retain());
	$effect(() => {
		fleet.poll(fleet.busy ? 5_000 : 30_000);
	});

	let editing = $state(false);
	/** Errors of the bulk editor, per container; the row keeps its previous value. */
	let rowError = $state<Record<string, string>>({});
	let applyingAll = $state<'auto_restart' | 'auto_update' | null>(null);

	const total = $derived(fleet.containers.length);
	const stopped = $derived(total - fleet.running);
	const lastCommand = $derived(fleet.commands[0] ?? null);

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

	function plural(n: number, word: string): string {
		return `${n} ${word}${n === 1 ? '' : 's'}`;
	}

	function manage() {
		openFold('containers');
	}

	async function save(c: ContainerView, patch: Partial<ContainerPolicy>) {
		const { [c.name]: _dropped, ...rest } = rowError;
		rowError = rest;
		const failure = await fleet.setPolicy(c, patch);
		if (failure) rowError = { ...rowError, [c.name]: toApiError(failure).message };
	}

	/** Header switch: every container gets the value; rows already there are left alone. */
	async function applyAll(key: 'auto_restart' | 'auto_update', value: boolean) {
		applyingAll = key;
		try {
			await Promise.all(fleet.containers.filter((c) => c.policy[key] !== value).map((c) => save(c, { [key]: value })));
		} finally {
			applyingAll = null;
		}
	}

	const allRestart = $derived(total > 0 && fleet.autoRestart === total);
	const allUpdate = $derived(total > 0 && fleet.autoUpdate === total);

	function onkeydown(event: KeyboardEvent) {
		if (event.key === 'Escape' && editing) {
			editing = false;
			event.stopPropagation();
		}
	}
</script>

<svelte:window {onkeydown} />

{#if !fleet.loading && !fleet.error && total > 0}
	<section
		class="rise-in mt-4 rounded-[var(--radius-card)] border border-line bg-surface px-4 py-3 shadow-lift sm:px-5"
		aria-label="Docker"
		style="--rise-delay: 60ms"
	>
		<div class="flex flex-wrap items-center gap-x-4 gap-y-2">
			<span class="inline-flex items-center gap-2 font-semibold text-ink">
				<Container class="size-4 text-ink-3" aria-hidden="true" />
				Docker
			</span>
			<p class="tnum min-w-0 flex-1 basis-60 text-sm text-ink-2">
				{plural(total, 'container')} · {fleet.running} running{#if stopped > 0}
					· {stopped} stopped{/if}{#if fleet.updates > 0}
					· {plural(fleet.updates, 'update')} available{/if}
				· policies: {fleet.autoRestart} auto-restart, {fleet.autoUpdate} auto-update
			</p>
			<div class="flex flex-wrap items-center gap-2">
				<Button size="sm" variant="secondary" onclick={manage}>Manage containers</Button>
				<Button size="sm" variant={editing ? 'secondary' : 'ghost'} onclick={() => (editing = !editing)} aria-expanded={editing} aria-controls={`docker-policies-${target.id}`}>
					{editing ? 'Close policies' : 'Policies…'}
				</Button>
			</div>
		</div>

		<!-- Last thing the agent was asked to do -->
		<p class="mt-2 flex flex-wrap items-center gap-x-2 gap-y-1 text-sm text-ink-2" aria-live="polite">
			<span>Last action:</span>
			{#if lastCommand}
				<Plate tone={STATUS_TONE[lastCommand.status]} label={STATUS_WORD[lastCommand.status]} pulse={lastCommand.status === 'running'} />
				<span class="text-ink">{commandLabel(lastCommand.kind)} <span class="font-semibold break-all">{commandContainer(lastCommand)}</span></span>
				<span class="tnum" title={lastCommand.created_at}>{formatRelative(lastCommand.created_at)}</span>
			{:else}
				<span>none yet — policies act on their own, or restart and update from the container list.</span>
			{/if}
		</p>

		{#if editing}
			<div id={`docker-policies-${target.id}`} class="mt-3 border-t border-line pt-3">
				{#if !fleet.commandsSupported}
					<p class="mb-3 rounded-lg border border-advisory/35 bg-advisory-soft px-3 py-2 text-sm text-ink" role="status">
						<Plate tone="advisory" label="Actions unavailable" class="mr-1" />
						This agent cannot run commands (too old, or <code class="font-mono text-[0.8125rem]">commands: false</code>): policies are kept but nothing runs until it is reinstalled with the current installer.
					</p>
				{/if}
				<p class="text-sm text-ink-2">
					Auto-update only runs inside a maintenance window for this device —
					<a href="/alerts#scheduled" class="text-ink underline decoration-line-strong underline-offset-2 hover:text-signal-ink">schedule one on the Alerts page</a>.
					Updates pull the same tag, recreate the container, wait for its healthcheck and roll back if it fails; the old image is removed afterwards.
				</p>

				<div class="mt-3 grid grid-cols-[minmax(0,1fr)_auto_auto] items-center gap-x-4 gap-y-2 sm:gap-x-8" role="table" aria-label="Container policies">
					<div class="contents" role="row">
						<span class="label-tape" role="columnheader">Container</span>
						<span class="label-tape text-center leading-tight" role="columnheader">Restart<span class="hidden sm:inline">&nbsp;if down</span></span>
						<span class="label-tape text-center leading-tight" role="columnheader">Auto-update<span class="hidden sm:inline">&nbsp;in maintenance windows</span></span>
					</div>

					<!-- Apply to all -->
					<div class="contents" role="row">
						<span class="text-sm font-semibold text-ink" role="cell">Apply to all</span>
						<span class="flex justify-center" role="cell">
							<Toggle id={`policy-all-restart-${target.id}`} checked={allRestart} disabled={applyingAll !== null} label="Restart if down, all containers" onchange={(v) => void applyAll('auto_restart', v)} />
						</span>
						<span class="flex justify-center" role="cell">
							<Toggle id={`policy-all-update-${target.id}`} checked={allUpdate} disabled={applyingAll !== null} label="Auto-update in maintenance windows, all containers" onchange={(v) => void applyAll('auto_update', v)} />
						</span>
					</div>
					<div class="col-span-3 graticule" aria-hidden="true"></div>

					{#each fleet.containers as c (c.name)}
						{@const saving = (fleet.saving[c.name] ?? false) || applyingAll !== null}
						<div class="contents" role="row">
							<span class="flex min-w-0 flex-col gap-0.5" role="cell">
								<span class="flex min-w-0 items-center gap-2">
									<span class="min-w-0 text-sm leading-tight text-ink break-all">{c.name}</span>
									{#if !c.up}<Plate tone="warning" label="Stopped" bare />{/if}
									{#if c.update_available === true}<Plate tone="info" label="Update available" bare />{/if}
								</span>
								{#if rowError[c.name]}
									<span class="text-xs text-warning-ink" role="alert">{rowError[c.name]}</span>
								{/if}
							</span>
							<span class="flex justify-center" role="cell">
								<Toggle id={`policy-restart-${target.id}-${c.name}`} checked={c.policy.auto_restart} disabled={saving} label={`Restart ${c.name} if down`} onchange={(v) => void save(c, { auto_restart: v })} />
							</span>
							<span class="flex justify-center" role="cell">
								<Toggle id={`policy-update-${target.id}-${c.name}`} checked={c.policy.auto_update} disabled={saving} label={`Auto-update ${c.name} in maintenance windows`} onchange={(v) => void save(c, { auto_update: v })} />
							</span>
						</div>
					{/each}
				</div>

				<div class="mt-3 flex justify-end">
					<Button size="sm" variant="ghost" onclick={() => (editing = false)}>Done</Button>
				</div>
			</div>
		{/if}
	</section>
{/if}
