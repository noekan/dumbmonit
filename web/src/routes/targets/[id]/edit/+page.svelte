<script lang="ts">
	/**
	 * Edit a device. Same form as "Add", seeded from the saved target; the kind
	 * is fixed and shown as a stamp. Credentials start blank and are only sent
	 * when retyped — the server keeps the saved ones otherwise.
	 */
	import { page } from '$app/state';
	import { goto } from '$app/navigation';
	import {
		getTarget,
		listCollectors,
		listTargets,
		normalizeCollector,
		type CollectorInfo,
		type Target
	} from '$lib/api';
	import { ErrorNotice, PageHeader, Panel, Plate, Skeleton } from '$lib/ui';
	import TargetForm from '$lib/components/device-form/TargetForm.svelte';
	import SetupNotice from '$lib/components/device-form/SetupNotice.svelte';
	import { kindIcon } from '$lib/components/device-form/kinds';

	const id = $derived(Number(page.params.id));

	let target = $state<Target | null>(null);
	let collectors = $state<CollectorInfo[]>([]);
	let targets = $state<Target[]>([]);
	let loading = $state(true);
	let error = $state<unknown>(null);

	async function load(signal?: AbortSignal) {
		loading = true;
		error = null;
		try {
			// The collectors and the parent list are helpers: their failure must not
			// hide the device itself, which is what the user came to edit.
			const [loaded, kinds, all] = await Promise.all([
				getTarget(id, signal),
				listCollectors(signal).catch(() => [] as CollectorInfo[]),
				listTargets(signal).catch(() => [] as Target[])
			]);
			target = loaded;
			collectors = kinds;
			targets = all;
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

	/** The device's kind as the server describes it, or a bare shell for an unknown one. */
	const collector = $derived.by(() => {
		if (!target) return null;
		const kind = target.kind;
		return collectors.find((c) => c.kind === kind) ?? normalizeCollector({ kind });
	});
	const KindIcon = $derived(target ? kindIcon(target.kind) : null);
</script>

<svelte:head><title>Edit {target?.name ?? 'device'} · DumbMonit</title></svelte:head>

<PageHeader
	title={target ? `Edit ${target.name}` : 'Edit device'}
	back={{ href: `/targets/${id}`, label: target?.name ?? 'Device' }}
/>

{#if error}
	<ErrorNotice {error} title="Could not load this device" onretry={() => void load()} />
{:else if loading || !target || !collector}
	<div class="grid items-start gap-6 lg:grid-cols-12">
		<div class="rounded-[var(--radius-card)] border border-line bg-surface p-5 lg:col-span-7">
			<div class="grid gap-4 sm:grid-cols-2">
				<Skeleton class="h-10 w-full" rows={2} />
			</div>
			<Skeleton class="mt-4 h-10 w-full" />
			<Skeleton class="mt-4 h-10 w-1/2" />
		</div>
		<div class="rounded-[var(--radius-card)] border border-line bg-surface p-5 lg:col-span-5">
			<Skeleton class="h-5 w-2/3" />
			<Skeleton class="mt-3 h-3.5 w-full" rows={3} />
		</div>
	</div>
{:else}
	<div class="grid items-start gap-6 lg:grid-cols-12">
		<div class="min-w-0 lg:col-span-7">
			<Panel title={collector.label} description="Type cannot be changed: remove the device and add it again as another type.">
				{#snippet aside()}
					{#if KindIcon}
						<Plate tone="ghost" bare>
							<KindIcon class="size-3.5" aria-hidden="true" />
							{target?.kind}
						</Plate>
					{/if}
				{/snippet}
				<!-- Re-mounted per device so every field is seeded fresh. -->
				{#key target.id}
					<TargetForm
						{collector}
						{target}
						{targets}
						cancelHref={`/targets/${target.id}`}
						onsaved={(saved) => goto(`/targets/${saved.id}`)}
					/>
				{/key}
			</Panel>
		</div>
		<aside class="min-w-0 lg:sticky lg:top-20 lg:col-span-5">
			<SetupNotice {collector} />
		</aside>
	</div>
{/if}
