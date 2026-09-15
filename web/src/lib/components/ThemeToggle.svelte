<script lang="ts">
	/** Cycles auto → light → dark. The icon shows what is applied right now. */
	import { Sun, Moon, SunMoon } from 'lucide-svelte';
	import { theme, type ThemePreference } from '$lib/stores/theme.svelte';

	const ORDER: ThemePreference[] = ['auto', 'light', 'dark'];
	const LABEL: Record<ThemePreference, string> = { auto: 'Theme: system', light: 'Theme: day', dark: 'Theme: night' };

	function next() {
		const i = ORDER.indexOf(theme.preference);
		theme.preference = ORDER[(i + 1) % ORDER.length];
	}
	const Icon = $derived(theme.preference === 'auto' ? SunMoon : theme.resolved === 'dark' ? Moon : Sun);
</script>

<button
	type="button"
	class="inline-flex size-9 items-center justify-center rounded-lg text-ink-2 transition-colors hover:bg-surface-2 hover:text-ink"
	onclick={next}
	aria-label={LABEL[theme.preference]}
	title={LABEL[theme.preference]}
>
	<Icon class="size-[18px]" aria-hidden="true" />
</button>
