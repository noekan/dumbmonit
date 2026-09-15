<script lang="ts">
	/**
	 * Settings → About: version and the health of the two stores behind the
	 * server. Polled every 15 s while the page is open.
	 */
	import { BookOpen } from 'lucide-svelte';
	import { getHealth, type ComponentHealth, type Health } from '$lib/api';
	import { ErrorNotice, Panel, Plate, Skeleton } from '$lib/ui';

	let health = $state<Health | null>(null);
	let loading = $state(true);
	let error = $state<unknown>(null);

	async function load(signal?: AbortSignal) {
		error = null;
		try {
			health = await getHealth(signal);
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
		const timer = setInterval(() => void load(controller.signal), 15_000);
		return () => {
			controller.abort();
			clearInterval(timer);
		};
	});

	const COMPONENTS: { key: 'database' | 'victoria'; name: string; role: string }[] = [
		{ key: 'database', name: 'Database', role: 'Device setup and alert history.' },
		{ key: 'victoria', name: 'VictoriaMetrics', role: 'Every measurement shown in the charts.' }
	];
</script>

<Panel id="about" title="About">
	{#if error}
		<ErrorNotice {error} title="Could not reach the server" onretry={() => void load()} />
	{:else if loading && !health}
		<Skeleton class="h-5 w-40" />
		<div class="mt-4 grid gap-2"><Skeleton class="h-12 w-full" rows={2} /></div>
	{:else if health}
		<dl class="grid gap-y-3 text-sm sm:grid-cols-[10rem_minmax(0,1fr)] sm:gap-x-6">
			<dt class="font-semibold text-ink">Version</dt>
			<dd class="tnum text-ink-2">{health.version}</dd>

			{#each COMPONENTS as component (component.key)}
				{@const state: ComponentHealth = health[component.key]}
				<dt class="font-semibold text-ink">{component.name}</dt>
				<dd class="min-w-0">
					<div class="flex flex-wrap items-center gap-2">
						{#if state.ok}
							<Plate tone="signal" label="Reporting" />
						{:else}
							<Plate tone="warning" label="Warning" />
						{/if}
						<span class="text-ink-2">{component.role}</span>
					</div>
					{#if !state.ok}
						<p class="mt-1 break-words text-warning-ink">{state.error ?? 'Component unreachable.'}</p>
						<p class="mt-0.5 text-ink-2">Check that its container is running, then read its logs with <code class="rounded-md border border-line bg-canvas-deep px-1 py-0.5 font-mono text-[0.8125rem]">docker compose logs</code>.</p>
					{/if}
				</dd>
			{/each}
		</dl>
	{/if}

	<div class="mt-5 flex flex-wrap items-center gap-x-4 gap-y-2 border-t border-line pt-4 text-sm">
		<a href="/docs/notifications" class="inline-flex items-center gap-1.5 font-medium text-signal-ink hover:underline">
			<BookOpen class="size-4" aria-hidden="true" />
			Documentation: Notification channels →
		</a>
		<span class="text-ink-2">Open source, Apache 2.0.</span>
	</div>
</Panel>
