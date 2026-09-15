<script lang="ts">
	/**
	 * Verification board for the bulletin's weather window: all six sky
	 * conditions side by side, in the current theme. Not linked from the app.
	 */
	import SkyScene, { type SkyCondition } from '$lib/components/overview/SkyScene.svelte';

	const CONDITIONS: { condition: SkyCondition; note: string }[] = [
		{ condition: 'clear', note: 'Everything reporting, nothing firing.' },
		{ condition: 'cloudy', note: 'A few advisories.' },
		{ condition: 'overcast', note: 'Many advisories.' },
		{ condition: 'storm', note: 'Warnings or unreachable devices.' },
		{ condition: 'waiting', note: 'Devices exist, none has reported yet.' },
		{ condition: 'empty', note: 'No devices at all.' }
	];
</script>

<svelte:head>
	<title>Sky scenes</title>
</svelte:head>

<div class="mx-auto max-w-5xl px-4 py-8">
	<h1 class="display text-2xl text-ink">Sky scenes</h1>
	<p class="mt-1 text-ink-2">The bulletin's weather window in each condition.</p>

	<ul class="mt-6 grid grid-cols-1 gap-6 sm:grid-cols-2 lg:grid-cols-3">
		{#each CONDITIONS as { condition, note } (condition)}
			<li>
				<SkyScene {condition} class="w-full" />
				<p class="label-tape mt-2 text-ink-3">{condition}</p>
				<p class="text-sm text-ink-2">{note}</p>
			</li>
		{/each}
	</ul>

	<h2 class="display mt-10 text-lg text-ink">At bulletin size</h2>
	<div class="mt-3 flex flex-wrap items-start gap-4">
		<SkyScene condition="clear" class="h-[130px] w-[220px]" />
		<SkyScene condition="storm" class="h-[130px] w-[220px]" />
	</div>
</div>
