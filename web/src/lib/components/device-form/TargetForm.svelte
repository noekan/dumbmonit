<script lang="ts">
	/**
	 * The device form, shared by "Add a device" and "Edit".
	 *
	 * The kind is decided before this form exists (picker on add, fixed on
	 * edit), so it only shows what that kind asks for: name, address, the
	 * credential families it accepts, its first settings. Everything else waits
	 * behind "More options". The form is uncontrolled: fields are seeded once
	 * from `target` and the pages re-mount it (`{#key}`) when the subject changes.
	 */
	import { untrack } from 'svelte';
	import { ChevronDown } from 'lucide-svelte';
	import {
		createTarget,
		updateTarget,
		type CollectorInfo,
		type CredentialView,
		type Target,
		type TargetPayload
	} from '$lib/api';
	import { listRelays } from '$lib/api/relay';
	import type { RelayAgent } from '$lib/api/types';
	import { Button, ClickSpark, ErrorNotice, Field, Toggle } from '$lib/ui';
	import CredentialFields from './CredentialFields.svelte';
	import OptionsFields from './OptionsFields.svelte';
	import TagsEditor from './TagsEditor.svelte';
	import {
		draftTouched,
		emptyDraft,
		initialView,
		toCredential,
		validateCredential,
		type CredentialDraft,
		type CredentialErrors
	} from './credentials';
	import { DEFAULT_INTERVAL, INTERVALS, suggestName } from './kinds';

	interface Props {
		collector: CollectorInfo;
		/** Present on edit, absent on add. */
		target?: Target;
		/** Candidates for "Parent device". The target itself is filtered out. */
		targets?: Target[];
		onsaved: (target: Target) => void;
		cancelHref: string;
	}

	let { collector, target, targets = [], onsaved, cancelHref }: Props = $props();

	const editing = $derived(target !== undefined);
	/** The credential families this kind accepts, field by field, from the server. */
	const views = $derived(collector.credentials);

	// --- Seed once from the target -------------------------------------------
	const initialCredential: CredentialView = untrack(() => initialView(collector.credentials, target?.credential_kind));

	let name = $state(untrack(() => target?.name ?? ''));
	let address = $state(untrack(() => target?.address ?? ''));
	let intervalSecs = $state(untrack(() => target?.interval_secs ?? DEFAULT_INTERVAL));
	let parentId = $state<number | null>(untrack(() => target?.parent_id ?? null));
	let viaAgent = $state<number | null>(untrack(() => target?.via_agent ?? null));
	let enabled = $state(untrack(() => target?.enabled ?? true));
	let tags = $state<Record<string, string>>(untrack(() => ({ ...(target?.tags ?? {}) })));
	let credential = $state<CredentialView>(initialCredential);
	// "public" is the factory community of nearly every device: pre-filled on add.
	let draft = $state<CredentialDraft>(
		untrack(() => ({
			...emptyDraft(initialCredential),
			...(!target && initialCredential.kind === 'snmp_community' ? { community: 'public' } : {})
		}))
	);

	function changeCredential(kind: string) {
		const next = views.find((view) => view.kind === kind);
		if (!next || next.kind === credential.kind) return;
		credential = next;
		draft = emptyDraft(next);
	}

	let touched = $state<Set<string>>(new Set());
	let attempted = $state(false);
	let saving = $state(false);
	let serverError = $state<unknown>(null);

	// "More options" opens by itself when an edited device already uses one.
	let more = $state(
		untrack(
			() =>
				!!target &&
				(target.parent_id !== null ||
					target.via_agent !== null ||
					target.interval_secs !== DEFAULT_INTERVAL ||
					!target.enabled ||
					Object.keys(target.tags).some((key) => !collector.options.some((o) => o.key === key)))
		)
	);

	function touch(field: string) {
		if (!touched.has(field)) touched = new Set(touched).add(field);
	}

	// --- Options vs free tags -------------------------------------------------
	const options = $derived(collector.options);
	const optionKeys = $derived(options.map((o) => o.key));
	/** The first few settings sit in the main form; the rest behind "More options". */
	const primaryOptions = $derived(options.filter((o, i) => o.required || i < 3));
	const moreOptions = $derived(options.filter((o) => !primaryOptions.includes(o)));
	const optionValues = $derived(Object.fromEntries(Object.entries(tags).filter(([k]) => optionKeys.includes(k))));
	const freeTags = $derived(Object.fromEntries(Object.entries(tags).filter(([k]) => !optionKeys.includes(k))));

	function setOptions(values: Record<string, string>) {
		tags = { ...freeTags, ...values };
	}
	function setFreeTags(values: Record<string, string>) {
		tags = { ...values, ...optionValues };
	}

	// --- Validation -----------------------------------------------------------
	const credentialChanged = $derived(credential.kind !== initialCredential.kind || draftTouched(credential, draft));
	/** On edit an untouched credential is simply not sent: the server keeps it. */
	const sendCredential = $derived(!editing || credentialChanged);

	const nameError = $derived(name.trim() ? null : 'Give this device a name.');
	const addressError = $derived(address.trim() ? null : 'Enter the address to reach it.');
	const credentialErrors = $derived<CredentialErrors>(sendCredential ? validateCredential(credential, draft) : {});
	const optionErrors = $derived.by(() => {
		const errors: Record<string, string> = {};
		for (const option of options) {
			if (!option.required) continue;
			if ((tags[option.key] ?? '').trim() || option.default.trim()) continue;
			errors[option.key] = `${option.label} is required for this type.`;
		}
		return errors;
	});

	const valid = $derived(
		!nameError && !addressError && Object.keys(credentialErrors).length === 0 && Object.keys(optionErrors).length === 0
	);

	function shown(field: string, error: string | null | undefined): string | undefined {
		return error && (attempted || touched.has(field)) ? error : undefined;
	}
	const shownCredentialErrors = $derived.by(() => {
		const out: CredentialErrors = {};
		for (const [field, message] of Object.entries(credentialErrors)) {
			if (attempted || touched.has(`cred.${field}`)) out[field] = message;
		}
		return out;
	});
	// Required settings without a default are rare (none today): show them at once.
	const shownOptionErrors = $derived(optionErrors);

	// --- Submit ---------------------------------------------------------------
	const addressHelp = $derived.by(() => {
		const parts: string[] = [];
		if (collector.address_hint) parts.push(`Example: ${collector.address_hint}.`);
		if (collector.default_port > 0) parts.push(`Add :port to use another port than ${collector.default_port}.`);
		return parts.join(' ') || undefined;
	});

	const parentCandidates = $derived(
		targets.filter((t) => t.id !== target?.id).sort((a, b) => a.name.localeCompare(b.name))
	);

	// --- Reached through (relay agents) --------------------------------------
	// Agents are not relays for themselves: their metrics are pushed, not probed.
	const canRelay = $derived(collector.kind !== 'agent');
	let relays = $state<RelayAgent[]>([]);
	$effect(() => {
		if (!canRelay) return;
		const controller = new AbortController();
		listRelays(controller.signal)
			.then((list) => (relays = list.filter((r) => r.id !== target?.id)))
			.catch(() => (relays = []));
		return () => controller.abort();
	});
	const relayCandidates = $derived(relays);
	/** The chosen relay, when it has not declared `relay: true`: probes would wait forever. */
	const relayNotReady = $derived(
		viaAgent !== null && relays.some((r) => r.id === viaAgent && !r.relay)
	);
	function relayLabel(relay: RelayAgent): string {
		return relay.site ? `${relay.name} (site ${relay.site})` : relay.name;
	}

	function buildPayload(): TargetPayload {
		const payload: TargetPayload = {
			name: name.trim(),
			address: address.trim(),
			kind: collector.kind,
			// The profile is detected by the server; on edit the detected one is kept.
			profile_id: target?.profile_id ?? null,
			parent_id: parentId,
			via_agent: canRelay ? viaAgent : null,
			interval_secs: intervalSecs,
			enabled,
			// No empty tag: a blank setting means "server default".
			tags: Object.fromEntries(Object.entries(tags).filter(([, v]) => v.trim()))
		};
		if (sendCredential) payload.credential = toCredential(credential, draft);
		return payload;
	}

	async function submit(event: SubmitEvent) {
		event.preventDefault();
		attempted = true;
		serverError = null;
		if (!valid || saving) return;
		saving = true;
		try {
			const payload = buildPayload();
			const saved = target ? await updateTarget(target.id, payload) : await createTarget(payload);
			onsaved(saved);
		} catch (cause) {
			serverError = cause;
		} finally {
			saving = false;
		}
	}
</script>

<form onsubmit={submit} class="grid gap-5" novalidate>
	<div class="grid gap-4 sm:grid-cols-2">
		<Field label="Name" for="target-name" required error={shown('name', nameError)} help="How it appears in lists and alerts.">
			<input
				id="target-name"
				class="input"
				type="text"
				autocomplete="off"
				bind:value={name}
				placeholder={suggestName(address) || 'Core switch'}
				aria-invalid={shown('name', nameError) ? 'true' : undefined}
				onblur={() => touch('name')}
			/>
		</Field>

		<Field label="Address" for="target-address" required error={shown('address', addressError)} help={addressHelp}>
			<div class="relative">
				<input
					id="target-address"
					class={`input font-mono text-[0.8125rem] ${collector.default_port > 0 ? 'pr-16' : ''}`}
					type="text"
					autocomplete="off"
					spellcheck="false"
					bind:value={address}
					placeholder={collector.address_hint || 'hostname or IP'}
					aria-invalid={shown('address', addressError) ? 'true' : undefined}
					onblur={() => {
						touch('address');
						// Naming a switch is not a prerequisite to watching it.
						if (!name.trim()) name = suggestName(address);
					}}
				/>
				{#if collector.default_port > 0 && !/:\d+$/.test(address.trim())}
					<span
						class="tnum pointer-events-none absolute top-1/2 right-3 -translate-y-1/2 font-mono text-[0.8125rem] text-ink-3"
						aria-hidden="true"
					>
						:{collector.default_port}
					</span>
				{/if}
			</div>
		</Field>
	</div>

	{#if !(views.length <= 1 && credential.fields.length === 0)}
		<CredentialFields
			{views}
			selected={credential}
			bind:draft
			{editing}
			errors={shownCredentialErrors}
			onkindchange={changeCredential}
			onblur={(field) => touch(`cred.${field}`)}
		/>
	{/if}

	{#if primaryOptions.length > 0}
		<OptionsFields options={primaryOptions} values={optionValues} errors={shownOptionErrors} onchange={setOptions} />
	{/if}

	<div class="graticule pb-3">
		<button
			type="button"
			class="inline-flex items-center gap-1.5 text-sm font-semibold text-ink-2 hover:text-ink"
			aria-expanded={more}
			aria-controls="target-more"
			onclick={() => (more = !more)}
		>
			<ChevronDown class={`size-4 transition-transform duration-200 ${more ? 'rotate-180' : ''}`} aria-hidden="true" />
			More options
		</button>
	</div>

	{#if more}
		<div id="target-more" class="grid gap-5 rise-in">
			{#if moreOptions.length > 0}
				<div class="grid gap-1">
					<p class="text-sm font-semibold text-ink">More {collector.label} settings</p>
					<p class="mb-2 text-[0.8125rem] text-ink-2">Blank fields use the server's defaults.</p>
					<OptionsFields options={moreOptions} values={optionValues} errors={shownOptionErrors} onchange={setOptions} />
				</div>
			{/if}

			<div class="grid gap-4 sm:grid-cols-2">
				<Field label="Check interval" for="target-interval" help="How often DumbMonit reads this device.">
					<select id="target-interval" class="input tnum" bind:value={intervalSecs}>
						{#each INTERVALS as option (option.value)}
							<option value={option.value}>{option.label}</option>
						{/each}
						{#if !INTERVALS.some((o) => o.value === intervalSecs)}
							<option value={intervalSecs}>{intervalSecs} s</option>
						{/if}
					</select>
				</Field>

				<Field
					label="Parent device"
					for="target-parent"
					help="If the parent goes down, alerts from this device are suppressed instead of sent."
				>
					<select id="target-parent" class="input" bind:value={parentId}>
						<option value={null}>None</option>
						{#each parentCandidates as candidate (candidate.id)}
							<option value={candidate.id}>{candidate.name}</option>
						{/each}
					</select>
				</Field>

				{#if canRelay && (relayCandidates.length > 0 || viaAgent !== null)}
					<Field
						label="Reached through"
						for="target-relay"
						help={relayNotReady
							? 'This agent has not enabled relay mode (relay: true): probes sent to it will time out until it does.'
							: 'Direct: the server probes the device. An agent: the agent probes it from its own network and reports back — for devices on another site.'}
					>
						<select id="target-relay" class="input" bind:value={viaAgent}>
							<option value={null}>Direct</option>
							{#each relayCandidates as relay (relay.id)}
								<option value={relay.id}>{relayLabel(relay)}{relay.relay ? '' : ' — relay off'}</option>
							{/each}
							{#if viaAgent !== null && !relayCandidates.some((r) => r.id === viaAgent)}
								<option value={viaAgent}>Agent #{viaAgent}</option>
							{/if}
						</select>
					</Field>
				{/if}
			</div>

			<TagsEditor tags={freeTags} reserved={optionKeys} onchange={setFreeTags} />

			<Field label="Enabled" for="target-enabled" inline help="A paused device is not checked and raises no alerts.">
				<Toggle id="target-enabled" bind:checked={enabled} label="Enabled" />
			</Field>
		</div>
	{/if}

	{#if serverError}
		<ErrorNotice error={serverError} title={editing ? 'Could not save the changes' : 'Could not add the device'} />
	{/if}

	<div class="flex flex-wrap items-center gap-2 pt-1">
		<ClickSpark>
			<Button type="submit" variant="primary" loading={saving} disabled={!valid}>
				{editing ? 'Save changes' : 'Add device'}
			</Button>
		</ClickSpark>
		<Button variant="ghost" href={cancelHref}>Cancel</Button>
	</div>
</form>
