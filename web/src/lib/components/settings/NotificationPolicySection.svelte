<script lang="ts">
	/**
	 * Settings → Notification policy: the global knobs that keep notifications
	 * quiet — batching window, hourly cap per channel, flap detection, and the
	 * public URL used for device links in messages. Per-channel choices (what a
	 * channel hears, quiet hours) live on each channel above; this panel only
	 * summarises them.
	 */
	import { SlidersHorizontal } from 'lucide-svelte';
	import {
		getNotificationPolicy,
		listChannels,
		updateNotificationPolicy,
		type Channel,
		type NotificationPolicy
	} from '$lib/api';
	import { auth } from '$lib/stores/auth.svelte';
	import { Button, ClickSpark, ErrorNotice, Field, Panel, Plate, Skeleton, Toggle } from '$lib/ui';

	let policy = $state<NotificationPolicy | null>(null);
	let channels = $state<Channel[]>([]);
	let loading = $state(true);
	let error = $state<unknown>(null);

	async function load(signal?: AbortSignal) {
		loading = true;
		error = null;
		try {
			const [current, list] = await Promise.all([getNotificationPolicy(signal), listChannels(signal)]);
			policy = current;
			channels = Array.isArray(list) ? list : [];
			batchWindow = current.batch_window_secs;
			maxPerHour = current.max_per_hour === 0 ? '' : String(current.max_per_hour);
			flapOn = current.flap_events > 0;
			flapEvents = current.flap_events > 0 ? String(current.flap_events) : '4';
			flapWindow = current.flap_window_secs;
			flapHold = current.flap_hold_secs;
			publicUrl = current.public_url;
		} catch (cause) {
			if (cause instanceof DOMException && cause.name === 'AbortError') return;
			error = cause;
		} finally {
			loading = false;
		}
	}

	$effect(() => {
		const controller = new AbortController();
		void load(controller.signal);
		return () => controller.abort();
	});

	// --- Form -----------------------------------------------------------------

	const WINDOWS = [
		{ value: 0, label: 'Send right away' },
		{ value: 30, label: '30 s' },
		{ value: 60, label: '1 min' },
		{ value: 120, label: '2 min' },
		{ value: 300, label: '5 min' }
	];
	const MINUTES = [
		{ value: 300, label: '5 min' },
		{ value: 900, label: '15 min' },
		{ value: 1800, label: '30 min' },
		{ value: 3600, label: '1 h' }
	];

	let batchWindow = $state(60);
	let maxPerHour = $state('20');
	let flapOn = $state(true);
	let flapEvents = $state('4');
	let flapWindow = $state(1800);
	let flapHold = $state(1800);
	let publicUrl = $state('');
	let showMore = $state(false);

	let saving = $state(false);
	let saveError = $state<unknown>(null);
	let saved = $state(false);

	function withCurrent(options: { value: number; label: string }[], current: number) {
		if (options.some((o) => o.value === current)) return options;
		return [...options, { value: current, label: `${current} s (current)` }].sort((a, b) => a.value - b.value);
	}

	const dirty = $derived.by(() => {
		if (!policy) return false;
		return (
			batchWindow !== policy.batch_window_secs ||
			Number(maxPerHour || 0) !== policy.max_per_hour ||
			(flapOn ? Number(flapEvents) : 0) !== policy.flap_events ||
			flapWindow !== policy.flap_window_secs ||
			flapHold !== policy.flap_hold_secs ||
			publicUrl.trim() !== policy.public_url
		);
	});

	let formError = $state<string | null>(null);

	async function save(event: SubmitEvent) {
		event.preventDefault();
		formError = null;
		saveError = null;
		saved = false;
		const cap = maxPerHour.trim() === '' ? 0 : Number(maxPerHour);
		if (!Number.isInteger(cap) || cap < 0) {
			formError = 'The hourly cap must be a whole number, or empty for no cap.';
			return;
		}
		const events = flapOn ? Number(flapEvents) : 0;
		if (flapOn && (!Number.isInteger(events) || events < 2)) {
			formError = 'Flap detection needs at least 2 state changes.';
			return;
		}
		const url = publicUrl.trim();
		if (url && !/^https?:\/\//.test(url)) {
			formError = 'The public URL must start with http:// or https://.';
			return;
		}
		saving = true;
		try {
			policy = await updateNotificationPolicy({
				batch_window_secs: batchWindow,
				max_per_hour: cap,
				flap_events: events,
				flap_window_secs: flapWindow,
				flap_hold_secs: flapHold,
				public_url: url
			});
			publicUrl = policy.public_url;
			saved = true;
			setTimeout(() => (saved = false), 2500);
		} catch (cause) {
			saveError = cause;
		} finally {
			saving = false;
		}
	}

	function severityWord(severity: Channel['policy']['min_severity']): string {
		if (severity === 'critical') return 'Warning only';
		if (severity === 'warning') return 'Advisory and up';
		return 'everything';
	}

	function clock(minute: number): string {
		const pad = (n: number) => String(n).padStart(2, '0');
		return `${pad(Math.floor(minute / 60))}:${pad(minute % 60)}`;
	}

	const DAY = ['Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat', 'Sun'];

	function channelSummary(channel: Channel): string {
		const p = channel.policy;
		const parts = [severityWord(p.min_severity)];
		if (!p.notify_resolved) parts.push('no recoveries');
		if (p.min_interval_secs > 0) parts.push(`same alert every ${Math.round(p.min_interval_secs / 60)} min at most`);
		if (p.quiet_hours) {
			const days = p.quiet_hours.days.length === 7 ? 'daily' : p.quiet_hours.days.map((d) => DAY[d]).join(' ');
			parts.push(`quiet ${days} ${clock(p.quiet_hours.start_minute)}–${clock(p.quiet_hours.end_minute)}`);
		}
		return parts.join(' · ');
	}
</script>

<Panel id="notifications-policy" title="Notification policy" description="How DumbMonit keeps notifications few: one message per burst, a cap per hour, and silence for alerts that flap.">
	{#snippet aside()}
		{#if !auth.isAdmin}
			<Plate tone="ghost" label="Viewer — read only" />
		{/if}
	{/snippet}

	{#if error}
		<ErrorNotice {error} title="Could not load the notification policy" onretry={() => void load()} />
	{:else if loading || !policy}
		<Skeleton class="h-10 w-full" rows={3} />
	{:else}
		<form class="grid gap-5" onsubmit={save} aria-label="Notification policy">
			<div class="grid gap-4 sm:grid-cols-2">
				<Field label="Group alerts for" for="policy-window" help="Alerts that fire within this window leave as one message per channel: “3 alerts on 2 devices”.">
					<select id="policy-window" class="input" bind:value={batchWindow} disabled={saving || !auth.isAdmin}>
						{#each withCurrent(WINDOWS, batchWindow) as option (option.value)}
							<option value={option.value}>{option.label}</option>
						{/each}
					</select>
				</Field>
				<Field label="Messages per channel per hour" for="policy-cap" help="Past the cap, alerts wait and arrive as one digest. Empty: no cap.">
					<input id="policy-cap" type="number" min="0" step="1" class="input tnum" bind:value={maxPerHour} placeholder="No cap" disabled={saving || !auth.isAdmin} />
				</Field>
			</div>

			<Field label="Public URL" for="policy-url" help="Used for the “Open in DumbMonit” link in every message. Leave empty to use EZYMONIT_PUBLIC_URL.">
				<input id="policy-url" type="url" class="input" bind:value={publicUrl} placeholder="https://monit.example.lan" autocomplete="off" disabled={saving || !auth.isAdmin} />
			</Field>

			<div class="grid gap-3">
				<button
					type="button"
					class="inline-flex w-fit items-center gap-1.5 text-sm font-semibold text-ink"
					aria-expanded={showMore}
					aria-controls="policy-more"
					onclick={() => (showMore = !showMore)}
				>
					<SlidersHorizontal class="size-4" aria-hidden="true" />
					{showMore ? 'Fewer options' : 'More options'}
					<span class="font-normal text-ink-2">— flap detection</span>
				</button>
				{#if showMore}
					<div id="policy-more" class="grid gap-4 rounded-lg border border-line bg-surface-2 p-4">
						<Field label="Hold alerts that flap" for="policy-flap" inline help="An alert that fires and clears repeatedly sends one “flapping” notice, then stays silent for a while.">
							<Toggle id="policy-flap" bind:checked={flapOn} disabled={saving || !auth.isAdmin} label="Hold alerts that flap" />
						</Field>
						{#if flapOn}
							<div class="grid gap-4 sm:grid-cols-3">
								<Field label="State changes" for="policy-flap-events" help="Fires and clears counted together.">
									<input id="policy-flap-events" type="number" min="2" step="1" class="input tnum" bind:value={flapEvents} disabled={saving || !auth.isAdmin} />
								</Field>
								<Field label="Within" for="policy-flap-window">
									<select id="policy-flap-window" class="input" bind:value={flapWindow} disabled={saving || !auth.isAdmin}>
										{#each withCurrent(MINUTES, flapWindow) as option (option.value)}
											<option value={option.value}>{option.label}</option>
										{/each}
									</select>
								</Field>
								<Field label="Then hold for" for="policy-flap-hold">
									<select id="policy-flap-hold" class="input" bind:value={flapHold} disabled={saving || !auth.isAdmin}>
										{#each withCurrent(MINUTES, flapHold) as option (option.value)}
											<option value={option.value}>{option.label}</option>
										{/each}
									</select>
								</Field>
							</div>
						{/if}
					</div>
				{/if}
			</div>

			{#if formError}
				<p class="text-[0.8125rem] font-medium text-warning-ink" role="alert">{formError}</p>
			{/if}
			{#if saveError}
				<ErrorNotice error={saveError} title="Could not save the policy" />
			{/if}

			{#if auth.isAdmin}
				<div class="flex flex-wrap items-center gap-3" aria-live="polite">
					<ClickSpark>
						<Button type="submit" variant="primary" loading={saving} disabled={!dirty}>Save changes</Button>
					</ClickSpark>
					{#if saved}
						<Plate tone="signal" label="Saved" />
					{/if}
				</div>
			{/if}
		</form>

		<div class="mt-6 border-t border-line pt-4">
			<h3 class="text-sm font-semibold text-ink">Per channel</h3>
			<p class="mt-0.5 text-[0.8125rem] text-ink-2">
				What each channel hears, and its quiet hours, are set on the channel itself (Edit, then “Delivery options”).
			</p>
			{#if channels.length === 0}
				<p class="mt-2 text-sm text-ink-2">No channel yet.</p>
			{:else}
				<ul class="mt-2 grid gap-1.5" role="list">
					{#each channels as channel (channel.id)}
						<li class="flex flex-wrap items-baseline gap-x-2 text-sm">
							<span class="font-medium text-ink">{channel.name}</span>
							<span class="text-ink-2">{channelSummary(channel)}</span>
							{#if !channel.enabled}
								<Plate tone="ghost" label="Disabled" bare />
							{/if}
						</li>
					{/each}
				</ul>
			{/if}
		</div>
	{/if}
</Panel>
