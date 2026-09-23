<script lang="ts">
	/**
	 * Routing filter of a channel: which alerts it wants.
	 *
	 * Off by default, and off is what every existing channel keeps: the channel
	 * receives everything the policy lets through. Switched on, it becomes a
	 * short list of conditions — a tag, a device kind, a rule — each either
	 * required or excluded. Conditions on the same field read as OR, different
	 * fields as AND, and exclusions always win.
	 *
	 * The preview underneath is not a guess: the server runs the very filter the
	 * alerting engine runs and says which devices come out of it.
	 */
	import { untrack } from 'svelte';
	import { Plus, X } from 'lucide-svelte';
	import {
		previewMatcher,
		type AlertSeverity,
		type ChannelMatcher,
		type MatchCondition,
		type MatchPreview
	} from '$lib/api';
	import { Field, Plate, Toggle } from '$lib/ui';
	import { matcherIsEmpty, matcherSentence } from '$lib/components/alerts/helpers';

	interface Props {
		value: ChannelMatcher | null;
		/** Shown in the sentence, so the whole delivery rule reads as one line. */
		minSeverity: AlertSeverity;
		onchange: (next: ChannelMatcher | null) => void;
		disabled?: boolean;
	}

	let { value, minSeverity, onchange, disabled = false }: Props = $props();

	/** A condition plus which list it belongs to, which is what a row edits. */
	type Row = { mode: 'include' | 'exclude'; field: 'tag' | 'kind' | 'rule'; key: string; value: string };

	function toRows(matcher: ChannelMatcher | null): Row[] {
		const rows: Row[] = [];
		for (const [mode, list] of [
			['include', matcher?.include ?? []],
			['exclude', matcher?.exclude ?? []]
		] as const) {
			for (const condition of list) {
				rows.push({
					mode,
					field: condition.field,
					key: condition.field === 'tag' ? condition.key : '',
					value: condition.value
				});
			}
		}
		return rows;
	}

	// Seeded once when the form opens: the draft must not follow list refreshes.
	const initial = untrack(() => value);
	let enabled = $state(!matcherIsEmpty(initial));
	let rows = $state<Row[]>(toRows(initial));

	/** The rows as the API sees them, blank ones dropped. */
	function build(): ChannelMatcher {
		const include: MatchCondition[] = [];
		const exclude: MatchCondition[] = [];
		for (const row of rows) {
			const text = row.value.trim();
			if (!text) continue;
			if (row.field === 'tag' && !row.key.trim()) continue;
			const condition: MatchCondition =
				row.field === 'tag'
					? { field: 'tag', key: row.key.trim(), value: text }
					: { field: row.field, value: text };
			(row.mode === 'include' ? include : exclude).push(condition);
		}
		return { include, exclude };
	}

	const draft = $derived(enabled ? build() : { include: [], exclude: [] });

	function emit() {
		onchange(enabled ? build() : null);
	}

	function addRow() {
		rows = [...rows, { mode: 'include', field: 'tag', key: '', value: '' }];
	}

	function removeRow(index: number) {
		rows = rows.filter((_, i) => i !== index);
		emit();
	}

	// --- Preview ---------------------------------------------------------------

	let preview = $state<MatchPreview | null>(null);
	let previewError = $state<string | null>(null);
	const signature = $derived(JSON.stringify(draft));

	$effect(() => {
		// Reading the signature is what subscribes this effect to the draft.
		const current = signature;
		if (!enabled) {
			preview = null;
			previewError = null;
			return;
		}
		const controller = new AbortController();
		// Debounced: the filter changes on every keystroke, the preview should
		// not follow at that pace.
		const timer = setTimeout(() => {
			previewMatcher(JSON.parse(current) as ChannelMatcher, controller.signal)
				.then((result) => {
					preview = result;
					previewError = null;
				})
				.catch((cause: unknown) => {
					if (cause instanceof DOMException && cause.name === 'AbortError') return;
					preview = null;
					previewError =
						cause instanceof Error ? cause.message : 'Could not preview this filter.';
				});
		}, 400);
		return () => {
			clearTimeout(timer);
			controller.abort();
		};
	});

	const sentence = $derived(matcherSentence(enabled ? draft : null, minSeverity));
	const matchedNames = $derived(
		(preview?.devices ?? []).filter((device) => device.matched).map((device) => device.name)
	);
</script>

<div class="grid gap-3">
	<Field
		label="Only some alerts"
		for="channel-matcher-on"
		inline
		help="Off: the channel receives everything above its severity floor. On: only what matches, by tag, device kind or rule."
	>
		<Toggle
			id="channel-matcher-on"
			checked={enabled}
			{disabled}
			label="Only some alerts"
			onchange={(next) => {
				enabled = next;
				if (next && rows.length === 0) addRow();
				emit();
			}}
		/>
	</Field>

	{#if enabled}
		<div class="grid gap-3 pl-1">
			{#each rows as row, index (index)}
				<div class="flex flex-wrap items-end gap-2">
					<label class="grid gap-1 text-[0.8125rem] font-semibold text-ink">
						<span class="sr-only">Include or exclude</span>
						<select
							class="input w-[7.5rem]"
							bind:value={row.mode}
							{disabled}
							onchange={emit}
							aria-label="Include or exclude"
						>
							<option value="include">Only</option>
							<option value="exclude">Except</option>
						</select>
					</label>
					<label class="grid gap-1 text-[0.8125rem] font-semibold text-ink">
						<span class="sr-only">Field</span>
						<select
							class="input w-[8.5rem]"
							bind:value={row.field}
							{disabled}
							onchange={emit}
							aria-label="Field"
						>
							<option value="tag">tag</option>
							<option value="kind">device kind</option>
							<option value="rule">rule</option>
						</select>
					</label>
					{#if row.field === 'tag'}
						<input
							class="input w-[9rem]"
							bind:value={row.key}
							{disabled}
							placeholder="site"
							autocomplete="off"
							aria-label="Tag name"
							oninput={emit}
						/>
						<span class="pb-2 text-ink-3" aria-hidden="true">=</span>
					{/if}
					<input
						class="input min-w-[9rem] flex-1"
						bind:value={row.value}
						{disabled}
						placeholder={row.field === 'tag' ? 'cellar' : row.field === 'kind' ? 'synology' : 'disk_full'}
						autocomplete="off"
						aria-label="Value"
						oninput={emit}
					/>
					<button
						type="button"
						class="rounded-lg border border-line-strong p-2 text-ink-2 transition hover:text-ink"
						{disabled}
						aria-label="Remove this condition"
						onclick={() => removeRow(index)}
					>
						<X class="size-4" aria-hidden="true" />
					</button>
				</div>
			{/each}

			<button
				type="button"
				class="inline-flex w-fit items-center gap-1.5 text-sm font-semibold text-ink"
				{disabled}
				onclick={addRow}
			>
				<Plus class="size-4" aria-hidden="true" />
				Add a condition
			</button>

			<div class="rounded-lg border border-line bg-surface-2 p-3">
				<p class="text-[0.8125rem] text-ink">{sentence}</p>
				{#if previewError}
					<p class="mt-1.5 text-[0.8125rem] font-medium text-warning-ink" role="alert">
						{previewError}
					</p>
				{:else if preview}
					<div class="mt-2 flex flex-wrap items-center gap-2">
						<Plate
							tone={preview.matched === 0 ? 'warning' : 'signal'}
							label={`${preview.matched} of ${preview.total} devices`}
						/>
						{#if preview.matched === 0}
							<span class="text-[0.8125rem] text-ink-2">
								No device matches today — this channel would stay silent.
							</span>
						{:else}
							<span class="text-[0.8125rem] text-ink-2">
								{matchedNames.slice(0, 6).join(', ')}{matchedNames.length > 6
									? ` and ${matchedNames.length - 6} more`
									: ''}
							</span>
						{/if}
					</div>
					{#if preview.rules.length > 0 || preview.excluded_rules.length > 0}
						<p class="mt-1.5 text-[0.8125rem] text-ink-2">
							Rule conditions apply to alerts, not devices: the count above ignores them.
						</p>
					{/if}
				{:else}
					<p class="mt-1.5 text-[0.8125rem] text-ink-3">Checking which devices match…</p>
				{/if}
			</div>
		</div>
	{/if}
</div>
