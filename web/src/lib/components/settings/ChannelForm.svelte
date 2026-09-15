<script lang="ts">
	/**
	 * Add or edit a notification channel, in two steps: pick a kind, then fill
	 * only the fields that kind asks for. Nothing is hard-coded: kinds and
	 * fields come from the server catalogue.
	 *
	 * Secrets are never read back. On edit they show empty and are only sent
	 * when typed again: the payload then omits `secrets` and the server keeps
	 * what it has. Sending `secrets` replaces the whole set, and `{}` clears it.
	 */
	import { untrack } from 'svelte';
	import { ExternalLink, Search, ChevronLeft } from 'lucide-svelte';
	import {
		createChannel,
		updateChannel,
		type AlertSeverity,
		type Channel,
		type ChannelPayload,
		type QuietHours
	} from '$lib/api';
	import { Button, ClickSpark, ErrorNotice, Field, Toggle } from '$lib/ui';
	import { kindDocUrl, kindIcon, type KindField, type KindInfo } from './kinds';
	import ChannelFieldInput from './ChannelFieldInput.svelte';
	import QuietHoursEditor from './QuietHoursEditor.svelte';

	interface Props {
		kinds: KindInfo[];
		/** Present when editing, absent when adding. */
		channel?: Channel;
		onsaved: (channel: Channel) => void;
		oncancel: () => void;
	}

	let { kinds, channel, onsaved, oncancel }: Props = $props();
	const editing = $derived(channel !== undefined);

	type Value = string | boolean;

	/** Turns a stored value into what its input edits: text, lines, JSON or a boolean. */
	function toField(field: KindField, raw: unknown): Value {
		if (field.input === 'boolean') return raw === true;
		if (raw === null || raw === undefined) return '';
		if (field.shape === 'list') return Array.isArray(raw) ? raw.map(String).join('\n') : String(raw);
		if (field.shape === 'object') return typeof raw === 'string' ? raw : JSON.stringify(raw, null, 2);
		return typeof raw === 'string' ? raw : String(raw);
	}

	function initialSettings(): Record<string, Value> {
		if (!channel) return {};
		const fields = kinds.find((k) => k.kind === channel.kind)?.settings ?? [];
		const initial: Record<string, Value> = {};
		for (const field of fields) initial[field.key] = toField(field, channel.settings?.[field.key]);
		return initial;
	}

	// The initial state is frozen when the form opens: it must not follow list
	// refreshes while the user types. `untrack` says this one-time read is meant.
	let kind = $state(untrack(() => channel?.kind ?? ''));
	let name = $state(untrack(() => channel?.name ?? ''));
	let enabled = $state(untrack(() => channel?.enabled ?? true));
	let settings = $state<Record<string, Value>>(untrack(() => initialSettings()));
	let secrets = $state<Record<string, string>>({});
	let clearSecrets = $state(false);

	// Delivery policy: what this channel accepts and when. The bulletin's ladder
	// words map onto the API severities (Advisory = warning, Warning = critical).
	const SEVERITY_FLOORS: { id: AlertSeverity; label: string }[] = [
		{ id: 'info', label: 'Everything (Info and up)' },
		{ id: 'warning', label: 'Advisory and up' },
		{ id: 'critical', label: 'Warning only' }
	];
	const INTERVALS: { value: number; label: string }[] = [
		{ value: 0, label: 'No minimum' },
		{ value: 300, label: '5 min' },
		{ value: 900, label: '15 min' },
		{ value: 3600, label: '1 h' },
		{ value: 21600, label: '6 h' }
	];
	let minSeverity = $state<AlertSeverity>(untrack(() => channel?.policy?.min_severity ?? 'info'));
	let notifyResolved = $state(untrack(() => channel?.policy?.notify_resolved ?? true));
	let minInterval = $state(untrack(() => channel?.policy?.min_interval_secs ?? 0));
	let quietHours = $state<QuietHours | null>(untrack(() => channel?.policy?.quiet_hours ?? null));
	let showDelivery = $state(
		untrack(
			() =>
				channel !== undefined &&
				(channel.policy?.min_severity !== 'info' ||
					channel.policy?.notify_resolved === false ||
					(channel.policy?.min_interval_secs ?? 0) > 0 ||
					channel.policy?.quiet_hours != null)
		)
	);
	const intervalOptions = $derived(
		INTERVALS.some((o) => o.value === minInterval)
			? INTERVALS
			: [...INTERVALS, { value: minInterval, label: `${minInterval} s (current)` }].sort((a, b) => a.value - b.value)
	);

	let step = $state<'kind' | 'fields'>(untrack(() => (channel ? 'fields' : 'kind')));
	let search = $state('');
	let searchBox = $state<HTMLInputElement | null>(null);
	// The picker opens with the cursor in the search box, every time it shows.
	$effect(() => {
		if (step === 'kind') searchBox?.focus();
	});

	const info = $derived(kinds.find((k) => k.kind === kind));
	const settingFields = $derived(info?.settings ?? []);
	const secretFields = $derived(info?.secrets ?? []);
	const keepsSecrets = $derived(editing && channel?.has_secret === true && !clearSecrets);

	const filteredKinds = $derived.by(() => {
		const q = search.trim().toLowerCase();
		if (!q) return kinds;
		return kinds.filter((k) => `${k.label} ${k.kind} ${k.summary}`.toLowerCase().includes(q));
	});

	function pickKind(next: string) {
		if (next !== kind) {
			kind = next;
			// Fields belong to a kind: switching kinds starts from a blank sheet.
			settings = {};
			secrets = {};
		}
		step = 'fields';
		fieldErrors = {};
	}

	// --- Validation and payload -----------------------------------------------

	let fieldErrors = $state<Record<string, string>>({});
	let nameError = $state<string | null>(null);
	let apiError = $state<unknown>(null);
	let saving = $state(false);

	function isBlank(value: Value | undefined): boolean {
		return value === undefined || (typeof value === 'string' && value.trim() === '');
	}

	/** Converts a field value to what the server expects. Throws a message when it cannot. */
	function toServer(field: KindField, value: Value): unknown {
		if (field.input === 'boolean') return value === true;
		if (typeof value !== 'string') return value;
		const text = value.trim();
		if (field.shape === 'list') {
			return text
				.split(/\r?\n/)
				.map((line) => line.trim())
				.filter(Boolean);
		}
		if (field.shape === 'object') {
			let parsed: unknown;
			try {
				parsed = JSON.parse(text);
			} catch {
				throw new Error('Must be a valid JSON object, for example {"key": "value"}.');
			}
			if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) {
				throw new Error('Must be a JSON object, not a list or a plain value.');
			}
			return parsed;
		}
		if (field.input === 'number') {
			const n = Number(text);
			if (Number.isNaN(n)) throw new Error('Must be a number.');
			return n;
		}
		return text;
	}

	function validate(): boolean {
		const errors: Record<string, string> = {};
		nameError = name.trim() ? null : 'Give the channel a name; you will pick it in alert rules.';

		for (const field of settingFields) {
			if (field.input === 'boolean') continue;
			const value = settings[field.key];
			if (field.required && isBlank(value)) {
				errors[`setting-${field.key}`] = 'This field is required.';
				continue;
			}
			if (isBlank(value)) continue;
			try {
				toServer(field, value ?? '');
			} catch (cause) {
				errors[`setting-${field.key}`] = cause instanceof Error ? cause.message : 'Invalid value.';
			}
		}
		// A required secret may stay blank on edit: the server keeps the one it has,
		// unless the user asked to clear the saved secrets.
		for (const field of secretFields) {
			if (field.required && isBlank(secrets[field.key]) && !keepsSecrets) {
				errors[`secret-${field.key}`] = 'This secret is required.';
			}
		}
		fieldErrors = errors;
		return !nameError && Object.keys(errors).length === 0;
	}

	function buildPayload(): ChannelPayload {
		// Editing without changing the kind keeps settings the catalogue does not
		// describe (yet); anything else starts clean.
		const outSettings: Record<string, unknown> = channel && channel.kind === kind ? { ...channel.settings } : {};
		for (const field of settingFields) {
			const value = settings[field.key];
			if (field.input !== 'boolean' && isBlank(value)) {
				delete outSettings[field.key];
				continue;
			}
			outSettings[field.key] = toServer(field, value ?? '');
		}

		const typed: Record<string, unknown> = {};
		for (const field of secretFields) {
			const value = secrets[field.key];
			if (!isBlank(value)) typed[field.key] = value.trim();
		}

		const payload: ChannelPayload = {
			name: name.trim(),
			kind,
			enabled,
			settings: outSettings,
			policy: {
				min_severity: minSeverity,
				notify_resolved: notifyResolved,
				min_interval_secs: minInterval,
				quiet_hours: quietHours
			}
		};
		// Rules: on create, always send `secrets` (possibly `{}`). On edit, send
		// them only if something was typed or a clear was requested — an omitted
		// `secrets` keeps the stored ones; `{}` clears them.
		if (!editing || clearSecrets || Object.keys(typed).length > 0) payload.secrets = typed;
		return payload;
	}

	async function save(event: SubmitEvent) {
		event.preventDefault();
		apiError = null;
		if (!validate()) return;

		saving = true;
		try {
			const payload = buildPayload();
			const saved = channel ? await updateChannel(channel.id, payload) : await createChannel(payload);
			onsaved(saved);
		} catch (cause) {
			apiError = cause;
		} finally {
			saving = false;
		}
	}

	const KindIcon = $derived(kindIcon(kind));
</script>

<div class="rounded-[var(--radius-card)] border border-signal/40 bg-surface shadow-lift" role="region" aria-label={editing ? `Edit ${channel?.name}` : 'Add channel'}>
	{#if step === 'kind'}
		<div class="border-b border-line px-5 py-4">
			<div class="flex flex-wrap items-start justify-between gap-3">
				<div>
					<h3 class="text-base font-semibold tracking-tight text-ink">{editing ? 'Change the channel type' : 'Where should alerts go?'}</h3>
					<p class="mt-0.5 text-sm text-ink-2">Pick a service. Only the fields it needs come next.</p>
				</div>
				<Button variant="ghost" size="sm" onclick={editing ? () => (step = 'fields') : oncancel}>
					{editing ? 'Keep current type' : 'Cancel'}
				</Button>
			</div>
			<div class="relative mt-3 max-w-sm">
				<Search class="pointer-events-none absolute top-1/2 left-3 size-4 -translate-y-1/2 text-ink-3" aria-hidden="true" />
				<input
					bind:this={searchBox}
					type="search"
					class="input pl-9"
					placeholder="Search Discord, email, webhook…"
					bind:value={search}
					autocomplete="off"
					aria-label="Search channel types"
				/>
			</div>
		</div>
		<div class="px-5 py-4">
			{#if filteredKinds.length === 0}
				<p class="ghost-cell rounded-lg border border-dashed border-line px-4 py-8 text-center text-sm text-ink-2">
					No channel type matches “{search}”.
				</p>
			{:else}
				<ul class="grid gap-2 sm:grid-cols-2 xl:grid-cols-3" role="list">
					{#each filteredKinds as option, i (option.kind)}
						{@const Icon = kindIcon(option.kind)}
						<li class="rise-in" style="--rise-delay: {Math.min(i, 12) * 25}ms">
							<button
								type="button"
								class={`faceplate flex w-full items-start gap-3 px-3.5 py-3 text-left ${option.kind === kind ? '!border-signal ring-2 ring-signal/30' : ''}`}
								data-interactive
								onclick={() => pickKind(option.kind)}
								aria-pressed={option.kind === kind}
							>
								<span class="mt-0.5 flex size-8 shrink-0 items-center justify-center rounded-lg bg-signal-soft text-signal-ink">
									<Icon class="size-4" aria-hidden="true" />
								</span>
								<span class="min-w-0">
									<span class="block text-sm font-semibold text-ink">{option.label}</span>
									<span class="mt-0.5 line-clamp-2 block text-[0.8125rem] leading-snug text-ink-2">{option.summary}</span>
								</span>
							</button>
						</li>
					{/each}
				</ul>
			{/if}
		</div>
	{:else}
		<form onsubmit={save} novalidate>
			<div class="flex flex-wrap items-center justify-between gap-3 border-b border-line px-5 py-4">
				<div class="flex min-w-0 items-center gap-3">
					<span class="flex size-9 shrink-0 items-center justify-center rounded-lg bg-signal-soft text-signal-ink">
						<KindIcon class="size-4" aria-hidden="true" />
					</span>
					<div class="min-w-0">
						<h3 class="truncate text-base font-semibold tracking-tight text-ink">
							{editing ? `Edit “${channel?.name}”` : `New ${info?.label ?? kind} channel`}
						</h3>
						{#if info?.summary}<p class="mt-0.5 text-sm text-ink-2">{info.summary}</p>{/if}
					</div>
				</div>
				<div class="flex items-center gap-1">
					<Button variant="ghost" size="sm" onclick={() => (step = 'kind')} disabled={saving}>
						<ChevronLeft class="size-4" aria-hidden="true" />
						Change type
					</Button>
					<Button variant="ghost" size="sm" href={kindDocUrl(info)} target="_blank" rel="noopener">
						Documentation
						<ExternalLink class="size-3.5" aria-hidden="true" />
					</Button>
				</div>
			</div>

			<div class="grid gap-5 px-5 py-5">
				<Field label="Name" for="channel-name" required error={nameError} help="Shown in the channel list and picked in alert rules.">
					<input
						id="channel-name"
						type="text"
						class="input"
						bind:value={name}
						placeholder={info ? `${info.label} — family` : 'Family chat'}
						autocomplete="off"
						disabled={saving}
						aria-invalid={nameError ? 'true' : undefined}
						oninput={() => (nameError = null)}
					/>
				</Field>

				{#if settingFields.length > 0}
					<fieldset class="grid gap-4 border-t border-line pt-5">
						<legend class="sr-only">Settings</legend>
						{#each settingFields as field (field.key)}
							<ChannelFieldInput
								{field}
								idPrefix="setting"
								value={settings[field.key] ?? (field.input === 'boolean' ? false : '')}
								error={fieldErrors[`setting-${field.key}`] ?? null}
								disabled={saving}
								onchange={(value) => {
									settings[field.key] = value;
									delete fieldErrors[`setting-${field.key}`];
								}}
							/>
						{/each}
					</fieldset>
				{/if}

				{#if secretFields.length > 0}
					<fieldset class="grid gap-4 border-t border-line pt-5">
						<legend class="mb-1 text-sm font-semibold text-ink">Secrets</legend>
						<p class="-mt-3 text-[0.8125rem] text-ink-2">
							Encrypted at rest and never shown again.
							{#if editing && channel?.has_secret}A secret is already saved for this channel.{/if}
						</p>
						{#each secretFields as field (field.key)}
							<ChannelFieldInput
								{field}
								idPrefix="secret"
								value={secrets[field.key] ?? ''}
								note={keepsSecrets ? 'Leave blank to keep the saved secret.' : undefined}
								error={fieldErrors[`secret-${field.key}`] ?? null}
								disabled={saving}
								onchange={(value) => {
									secrets[field.key] = typeof value === 'string' ? value : '';
									delete fieldErrors[`secret-${field.key}`];
								}}
							/>
						{/each}
						{#if editing && channel?.has_secret}
							<Field label="Clear the saved secrets" for="clear-secrets" inline help="Removes what is stored; the channel will fail until new secrets are entered.">
								<Toggle id="clear-secrets" bind:checked={clearSecrets} disabled={saving} label="Clear the saved secrets" />
							</Field>
						{/if}
					</fieldset>
				{/if}

				<div class="border-t border-line pt-5">
					<Field label="Enabled" for="channel-enabled" inline help="A disabled channel keeps its setup but receives nothing.">
						<Toggle id="channel-enabled" bind:checked={enabled} disabled={saving} label="Enabled" />
					</Field>
				</div>

				<fieldset class="grid gap-4 border-t border-line pt-5">
					<legend class="sr-only">Delivery</legend>
					<button
						type="button"
						class="inline-flex w-fit items-center gap-1.5 text-sm font-semibold text-ink"
						aria-expanded={showDelivery}
						aria-controls="channel-delivery"
						onclick={() => (showDelivery = !showDelivery)}
					>
						{showDelivery ? 'Hide delivery options' : 'Delivery options'}
						<span class="font-normal text-ink-2">— what this channel hears, and when</span>
					</button>
					{#if showDelivery}
						<div id="channel-delivery" class="grid gap-4">
							<div class="grid gap-4 sm:grid-cols-2">
								<Field label="Send" for="channel-min-severity" help="Alerts below this level never reach the channel.">
									<select id="channel-min-severity" class="input" bind:value={minSeverity} disabled={saving}>
										{#each SEVERITY_FLOORS as option (option.id)}
											<option value={option.id}>{option.label}</option>
										{/each}
									</select>
								</Field>
								<Field label="Same alert again no sooner than" for="channel-min-interval" help="Reminders and re-fires of one alert are spaced out by at least this.">
									<select id="channel-min-interval" class="input" bind:value={minInterval} disabled={saving}>
										{#each intervalOptions as option (option.value)}
											<option value={option.value}>{option.label}</option>
										{/each}
									</select>
								</Field>
							</div>
							<Field label="Tell me when it clears" for="channel-resolved" inline help="Off: the channel only hears about problems, never recoveries.">
								<Toggle id="channel-resolved" bind:checked={notifyResolved} disabled={saving} label="Tell me when it clears" />
							</Field>
							<QuietHoursEditor value={quietHours} idPrefix="channel-quiet" disabled={saving} onchange={(next) => (quietHours = next)} />
						</div>
					{/if}
				</fieldset>

				{#if apiError}
					<ErrorNotice error={apiError} title="Could not save the channel" />
				{/if}
			</div>

			<div class="flex flex-wrap items-center justify-end gap-2 border-t border-line px-5 py-4">
				<Button variant="ghost" onclick={oncancel} disabled={saving}>Cancel</Button>
				<ClickSpark>
					<Button type="submit" variant="primary" loading={saving}>
						{editing ? 'Save changes' : 'Add channel'}
					</Button>
				</ClickSpark>
			</div>
		</form>
	{/if}
</div>
