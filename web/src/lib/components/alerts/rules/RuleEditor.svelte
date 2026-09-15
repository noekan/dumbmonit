<script lang="ts">
	/**
	 * Inline editor for one rule: the knobs an operator actually turns —
	 * threshold, hold, severity, who gets told, how often — never the query
	 * builder. A shipped rule keeps its name and words; a rule you wrote is
	 * yours to rename.
	 *
	 * Save sends the rule back whole (`payloadFrom`) with the edited fields
	 * overridden, so the server never zeroes a field the editor does not show.
	 */
	import { untrack } from 'svelte';
	import type { AlertRule, Channel, RuleOperator } from '$lib/api';
	import { ApiError, updateAlertRule } from '$lib/api';
	import { Button, Field } from '$lib/ui';
	import {
		ESCALATE_OPTIONS,
		HOLD_OPTIONS,
		REPEAT_OPTIONS,
		SEVERITY_OPTIONS,
		anomalySummary,
		clearLabel,
		fromSeverityWord,
		payloadFrom,
		toSeverityWord,
		withCurrent,
		type SeverityWord
	} from './options';
	import OverridesEditor from './OverridesEditor.svelte';

	interface Props {
		rule: AlertRule;
		channels: Channel[];
		/** Null while the channel list is loading or failed: the picker hides. */
		channelsError?: string | null;
		onsaved: (rule: AlertRule) => void;
		oncancel: () => void;
	}

	let { rule, channels, channelsError = null, onsaved, oncancel }: Props = $props();

	const OPERATORS: { id: RuleOperator; label: string }[] = [
		{ id: '>', label: '> above' },
		{ id: '>=', label: '≥ at least' },
		{ id: '<', label: '< below' },
		{ id: '<=', label: '≤ at most' }
	];

	const anomaly = $derived(rule.kind === 'anomaly');
	const prefix = $derived(`rule-${rule.id}`);

	// Drafts snapshot the rule as the server held it when the editor opened;
	// a background refresh must not overwrite what the operator is typing.
	const initial = untrack(() => rule);
	let name = $state(initial.name);
	let description = $state(initial.description);
	let query = $state(initial.query);
	let threshold = $state(String(initial.threshold));
	// Empty string: no hysteresis. The field is a text input so "no value" stays distinct from 0.
	let clearThreshold = $state(initial.clear_threshold === null ? '' : String(initial.clear_threshold));
	let operator = $state<RuleOperator>(initial.operator);
	let forSecs = $state(initial.for_secs);
	let severity = $state<SeverityWord>(toSeverityWord(initial.severity));
	let selected = $state<number[]>([...initial.channels]);
	let repeatSecs = $state(initial.repeat_secs ?? 0);
	let escalateSecs = $state(initial.escalate_after_secs ?? 0);

	let saving = $state(false);
	let error = $state<string | null>(null);
	let hint = $state<string | null>(null);

	const holdOptions = $derived(withCurrent(HOLD_OPTIONS, rule.for_secs));
	const repeatOptions = $derived(withCurrent(REPEAT_OPTIONS, rule.repeat_secs ?? 0));
	const escalateOptions = $derived(withCurrent(ESCALATE_OPTIONS, rule.escalate_after_secs ?? 0));

	const enabledChannels = $derived(channels.filter((c) => c.enabled));
	/** What "all" means today, in plain words. */
	const allChannelsLabel = $derived(
		enabledChannels.length === 0
			? 'All enabled channels — none is enabled right now, so nothing is sent'
			: enabledChannels.length === 1
				? `All enabled channels (currently ${enabledChannels[0].name})`
				: `All enabled channels (currently ${enabledChannels.length})`
	);

	function toggleChannel(id: number, on: boolean) {
		selected = on ? [...selected, id] : selected.filter((c) => c !== id);
	}

	async function submit(event: SubmitEvent) {
		event.preventDefault();
		error = null;
		hint = null;
		const trimmedName = name.trim();
		if (!trimmedName) {
			error = 'The rule needs a name.';
			return;
		}
		const trimmedQuery = query.trim();
		if (!trimmedQuery) {
			error = 'The query is required — without it the rule watches nothing.';
			return;
		}
		const thresholdValue = anomaly ? rule.threshold : Number(threshold);
		if (!Number.isFinite(thresholdValue)) {
			error = 'The threshold must be a number.';
			return;
		}
		let clearValue: number | null = null;
		if (!anomaly && clearThreshold.trim() !== '') {
			clearValue = Number(clearThreshold);
			if (!Number.isFinite(clearValue)) {
				error = 'The clear threshold must be a number, or left empty.';
				return;
			}
			const firesAbove = operator === '>' || operator === '>=';
			if (firesAbove ? clearValue >= thresholdValue : clearValue <= thresholdValue) {
				error = firesAbove
					? 'The clear threshold must be below the threshold: fire above one, clear under the other.'
					: 'The clear threshold must be above the threshold: fire below one, clear over the other.';
				return;
			}
		}
		saving = true;
		try {
			const saved = await updateAlertRule(rule.id, {
				...payloadFrom(rule),
				name: rule.builtin ? rule.name : trimmedName,
				description: rule.builtin ? rule.description : description.trim(),
				query: rule.builtin ? rule.query : trimmedQuery,
				operator,
				threshold: thresholdValue,
				clear_threshold: clearValue,
				for_secs: forSecs,
				severity: fromSeverityWord(severity),
				channels: selected,
				repeat_secs: repeatSecs > 0 ? repeatSecs : null,
				escalate_after_secs: escalateSecs > 0 ? escalateSecs : null
			});
			onsaved(saved);
		} catch (cause) {
			error = cause instanceof Error ? cause.message : 'Could not save the rule.';
			if (cause instanceof ApiError && cause.hint) hint = cause.hint;
		} finally {
			saving = false;
		}
	}

	function onkeydown(event: KeyboardEvent) {
		if (event.key === 'Escape' && !saving) {
			event.stopPropagation();
			oncancel();
		}
	}
</script>

<!-- Escape leaves the editor, from anywhere in it. -->
<svelte:window {onkeydown} />

<form class="mt-3 grid gap-4 border-t border-line pt-4" onsubmit={submit} aria-label={`Edit ${rule.name}`}>
	{#if !rule.builtin}
		<div class="grid gap-4 sm:grid-cols-2">
			<Field label="Name" for={`${prefix}-name`} required>
				<input id={`${prefix}-name`} class="input" bind:value={name} />
			</Field>
			<Field label="Description" for={`${prefix}-description`}>
				<input id={`${prefix}-description`} class="input" bind:value={description} />
			</Field>
		</div>
		<Field label="Query" for={`${prefix}-query`} required help="MetricsQL, evaluated per device.">
			<input
				id={`${prefix}-query`}
				class="input font-mono text-[0.8125rem]"
				bind:value={query}
				autocomplete="off"
				spellcheck="false"
			/>
		</Field>
	{/if}

	{#if anomaly}
		<Field label="Detection" for={`${prefix}-params`} help="Baseline anomaly settings are shipped with the rule and not editable here.">
			<p id={`${prefix}-params`} class="tnum text-[0.8125rem] text-ink-2">{anomalySummary(rule)}</p>
		</Field>
	{:else}
		<div class="grid gap-4 sm:grid-cols-3">
			<Field label="Operator" for={`${prefix}-op`}>
				<select id={`${prefix}-op`} class="input" bind:value={operator}>
					{#each OPERATORS as op (op.id)}
						<option value={op.id}>{op.label}</option>
					{/each}
				</select>
			</Field>
			<Field label="Threshold" for={`${prefix}-threshold`} required>
				<div class="relative">
					<input
						id={`${prefix}-threshold`}
						type="number"
						step="any"
						class={`input tnum ${rule.unit ? 'pr-12' : ''}`}
						bind:value={threshold}
					/>
					{#if rule.unit}
						<span class="label-tape pointer-events-none absolute top-1/2 right-3 -translate-y-1/2" aria-hidden="true">{rule.unit}</span>
					{/if}
				</div>
			</Field>
			<Field label="Hold for" for={`${prefix}-for`} help="How long the condition must last before firing.">
				<select id={`${prefix}-for`} class="input" bind:value={forSecs}>
					{#each holdOptions as option (option.value)}
						<option value={option.value}>{option.label}</option>
					{/each}
				</select>
			</Field>
		</div>
		<div class="grid gap-4 sm:grid-cols-3">
			<Field
				label={clearLabel(operator)}
				for={`${prefix}-clear`}
				help="Optional. Once firing, the alert only clears past this value, so a reading hovering at the threshold does not flap."
			>
				<div class="relative">
					<input
						id={`${prefix}-clear`}
						type="number"
						step="any"
						class={`input tnum ${rule.unit ? 'pr-12' : ''}`}
						bind:value={clearThreshold}
						placeholder="None"
					/>
					{#if rule.unit}
						<span class="label-tape pointer-events-none absolute top-1/2 right-3 -translate-y-1/2" aria-hidden="true">{rule.unit}</span>
					{/if}
				</div>
			</Field>
		</div>
	{/if}

	<div class="grid gap-4 sm:grid-cols-3">
		<Field label="Severity" for={`${prefix}-severity`}>
			<select id={`${prefix}-severity`} class="input" bind:value={severity}>
				{#each SEVERITY_OPTIONS as option (option.id)}
					<option value={option.id}>{option.label}</option>
				{/each}
			</select>
		</Field>
		<Field label="Repeat every" for={`${prefix}-repeat`} help="Reminder while it keeps firing.">
			<select id={`${prefix}-repeat`} class="input" bind:value={repeatSecs}>
				{#each repeatOptions as option (option.value)}
					<option value={option.value}>{option.label}</option>
				{/each}
			</select>
		</Field>
		<Field label="Escalate after" for={`${prefix}-escalate`} help="Raise the severity one rung if still firing.">
			<select id={`${prefix}-escalate`} class="input" bind:value={escalateSecs}>
				{#each escalateOptions as option (option.value)}
					<option value={option.value}>{option.label}</option>
				{/each}
			</select>
		</Field>
		{#if anomaly}
			<Field label="Hold for" for={`${prefix}-for`} help="How long the anomaly must last before firing.">
				<select id={`${prefix}-for`} class="input" bind:value={forSecs}>
					{#each holdOptions as option (option.value)}
						<option value={option.value}>{option.label}</option>
					{/each}
				</select>
			</Field>
		{/if}
	</div>

	<fieldset class="grid gap-1.5">
		<legend class="text-sm font-semibold text-ink">Notify via</legend>
		{#if channelsError}
			<p class="text-[0.8125rem] text-ink-2">
				Could not load the channels ({channelsError}). Leaving the list as it is:
				{rule.channels.length === 0 ? 'all enabled channels.' : `${rule.channels.length} selected channel(s).`}
			</p>
		{:else if channels.length === 0}
			<p class="text-[0.8125rem] text-ink-2">
				No channel yet — nothing is sent. <a href="/settings" class="text-ink hover:underline">Add one in Settings</a>.
			</p>
		{:else}
			<div class="flex flex-wrap gap-x-4 gap-y-1.5">
				{#each channels as channel (channel.id)}
					<label class="inline-flex items-center gap-2 text-sm text-ink">
						<input
							type="checkbox"
							class="size-4 accent-[var(--c-signal)]"
							checked={selected.includes(channel.id)}
							onchange={(e) => toggleChannel(channel.id, (e.currentTarget as HTMLInputElement).checked)}
						/>
						{channel.name}
						{#if !channel.enabled}
							<span class="label-tape">disabled</span>
						{/if}
					</label>
				{/each}
			</div>
			<p class="text-[0.8125rem] text-ink-2" aria-live="polite">
				{#if selected.length === 0}
					None ticked: {allChannelsLabel}.
				{:else}
					Only the ticked channels are notified.
				{/if}
			</p>
		{/if}
	</fieldset>

	{#if !anomaly}
		<OverridesEditor {rule} />
	{/if}

	{#if error}
		<p class="text-[0.8125rem] font-medium text-warning-ink" role="alert">
			{error}
			{#if hint}<span class="font-normal text-ink-2"> {hint}</span>{/if}
		</p>
	{/if}

	<div class="flex items-center gap-2">
		<Button type="submit" variant="primary" size="sm" loading={saving}>Save changes</Button>
		<Button type="button" variant="ghost" size="sm" disabled={saving} onclick={oncancel}>Cancel</Button>
	</div>
</form>
