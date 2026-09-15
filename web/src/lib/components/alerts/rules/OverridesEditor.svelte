<script lang="ts">
	/**
	 * Per-device overrides of one rule: "on backup-nas, fire at 97 % instead",
	 * or "not on the lab box at all". Lives inside the rule editor, saves each
	 * override on its own (an override is its own resource on the server), so
	 * the rule's Save button is not involved.
	 */
	import { untrack } from 'svelte';
	import { ChevronDown, ChevronRight } from 'lucide-svelte';
	import {
		deleteRuleOverride,
		listTargets,
		putRuleOverride,
		type AlertRule,
		type RuleOverride,
		type Target
	} from '$lib/api';
	import { Button, Field, Toggle } from '$lib/ui';
	import { clearLabel } from './options';

	interface Props {
		rule: AlertRule;
	}

	let { rule }: Props = $props();

	const prefix = $derived(`rule-${rule.id}-over`);
	const unit = $derived(rule.unit);

	// The list starts from what the rule carried when the editor opened; a
	// background refresh must not overwrite what is being edited (hence untrack).
	const initial = untrack(() => rule.overrides);
	let overrides = $state<RuleOverride[]>([...initial]);
	let open = $state(initial.length > 0);
	let targets = $state<Target[]>([]);
	let targetsError = $state<string | null>(null);

	$effect(() => {
		if (!open) return;
		const controller = new AbortController();
		listTargets(controller.signal)
			.then((list) => (targets = list))
			.catch((cause) => {
				if (cause instanceof DOMException && cause.name === 'AbortError') return;
				targetsError = cause instanceof Error ? cause.message : 'Could not load the devices.';
			});
		return () => controller.abort();
	});

	function deviceName(id: number): string {
		return targets.find((t) => t.id === id)?.name ?? `device ${id}`;
	}

	// --- Add form ---------------------------------------------------------------

	let adding = $state(false);
	let targetId = $state<string>('');
	let threshold = $state('');
	let clear = $state('');
	let disabled = $state(false);
	let saving = $state(false);
	let error = $state<string | null>(null);

	const candidates = $derived(targets.filter((t) => !overrides.some((o) => o.target_id === t.id)));

	function summary(o: RuleOverride): string {
		if (o.enabled === false) return 'rule off';
		const parts: string[] = [];
		if (o.threshold !== null) parts.push(`threshold ${o.threshold}${unit ? ` ${unit}` : ''}`);
		if (o.clear_threshold !== null) parts.push(`${clearLabel(rule.operator).toLowerCase()} ${o.clear_threshold}${unit ? ` ${unit}` : ''}`);
		return parts.join(' · ') || 'no change';
	}

	async function add(event: SubmitEvent) {
		event.preventDefault();
		error = null;
		const id = Number(targetId);
		if (!targetId || !Number.isFinite(id)) {
			error = 'Pick a device.';
			return;
		}
		const payload: { threshold?: number; clear_threshold?: number; enabled?: boolean } = {};
		if (disabled) {
			payload.enabled = false;
		} else {
			if (threshold.trim() !== '') {
				const n = Number(threshold);
				if (!Number.isFinite(n)) {
					error = 'The threshold must be a number.';
					return;
				}
				payload.threshold = n;
			}
			if (clear.trim() !== '') {
				const n = Number(clear);
				if (!Number.isFinite(n)) {
					error = 'The clear threshold must be a number.';
					return;
				}
				payload.clear_threshold = n;
			}
			if (payload.threshold === undefined && payload.clear_threshold === undefined) {
				error = 'Set a threshold, a clear threshold, or turn the rule off for this device.';
				return;
			}
		}
		saving = true;
		try {
			const saved = await putRuleOverride(rule.id, id, payload);
			overrides = [...overrides.filter((o) => o.target_id !== saved.target_id), saved];
			adding = false;
			targetId = '';
			threshold = '';
			clear = '';
			disabled = false;
		} catch (cause) {
			error = cause instanceof Error ? cause.message : 'Could not save the override.';
		} finally {
			saving = false;
		}
	}

	let removing = $state<number | null>(null);

	async function remove(o: RuleOverride) {
		removing = o.target_id;
		error = null;
		try {
			await deleteRuleOverride(rule.id, o.target_id);
			overrides = overrides.filter((x) => x.target_id !== o.target_id);
		} catch (cause) {
			error = cause instanceof Error ? cause.message : 'Could not remove the override.';
		} finally {
			removing = null;
		}
	}
</script>

<div class="grid gap-2">
	<button
		type="button"
		class="inline-flex items-center gap-1.5 text-sm font-semibold text-ink"
		aria-expanded={open}
		aria-controls={`${prefix}-panel`}
		onclick={() => (open = !open)}
	>
		{#if open}
			<ChevronDown class="size-4" aria-hidden="true" />
		{:else}
			<ChevronRight class="size-4" aria-hidden="true" />
		{/if}
		Per-device overrides
		<span class="tnum font-normal text-ink-2">({overrides.length})</span>
	</button>

	{#if open}
		<div id={`${prefix}-panel`} class="grid gap-3 pl-5">
			{#if overrides.length === 0}
				<p class="text-[0.8125rem] text-ink-2">
					None. Overrides change the threshold, the clear threshold, or turn this rule off for one device.
				</p>
			{:else}
				<ul class="grid gap-1.5" role="list">
					{#each overrides as o (o.target_id)}
						<li class="flex flex-wrap items-center gap-x-3 gap-y-1 text-sm">
							<span class="font-medium text-ink">{deviceName(o.target_id)}</span>
							<span class="tnum text-ink-2">{summary(o)}</span>
							<Button variant="ghost" size="sm" loading={removing === o.target_id} onclick={() => void remove(o)}>
								Remove
							</Button>
						</li>
					{/each}
				</ul>
			{/if}

			{#if targetsError}
				<p class="text-[0.8125rem] text-warning-ink">{targetsError}</p>
			{:else if adding}
				<form class="grid gap-3 rounded-lg border border-line bg-surface-2 p-3" onsubmit={add} aria-label="Add override">
					<div class="grid gap-3 sm:grid-cols-3">
						<Field label="Device" for={`${prefix}-target`} required>
							<select id={`${prefix}-target`} class="input" bind:value={targetId}>
								<option value="">Pick a device</option>
								{#each candidates as target (target.id)}
									<option value={String(target.id)}>{target.name}</option>
								{/each}
							</select>
						</Field>
						<Field label="Threshold" for={`${prefix}-threshold`} help={unit ? `In ${unit}.` : undefined}>
							<input id={`${prefix}-threshold`} type="number" step="any" class="input tnum" bind:value={threshold} placeholder={String(rule.threshold)} disabled={disabled} />
						</Field>
						<Field label={clearLabel(rule.operator)} for={`${prefix}-clear`}>
							<input id={`${prefix}-clear`} type="number" step="any" class="input tnum" bind:value={clear} placeholder={rule.clear_threshold === null ? 'None' : String(rule.clear_threshold)} disabled={disabled} />
						</Field>
					</div>
					<Field label="Turn this rule off for the device" for={`${prefix}-off`} inline>
						<Toggle id={`${prefix}-off`} bind:checked={disabled} label="Turn this rule off for the device" />
					</Field>
					{#if error}
						<p class="text-[0.8125rem] font-medium text-warning-ink" role="alert">{error}</p>
					{/if}
					<div class="flex items-center gap-2">
						<Button type="submit" variant="secondary" size="sm" loading={saving}>Save override</Button>
						<Button type="button" variant="ghost" size="sm" disabled={saving} onclick={() => (adding = false)}>Cancel</Button>
					</div>
				</form>
			{:else}
				{#if error}
					<p class="text-[0.8125rem] font-medium text-warning-ink" role="alert">{error}</p>
				{/if}
				<div>
					<Button variant="ghost" size="sm" disabled={candidates.length === 0 && targets.length > 0} onclick={() => (adding = true)}>
						Add override
					</Button>
				</div>
			{/if}
		</div>
	{/if}
</div>
