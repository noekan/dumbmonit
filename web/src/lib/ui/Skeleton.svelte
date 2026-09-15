<script lang="ts">
	/** A loading placeholder that shimmers. Sized by the caller via class. */
	interface Props {
		class?: string;
		rows?: number;
	}
	let { class: className = 'h-4 w-full', rows = 1 }: Props = $props();
</script>

{#each Array(rows) as _, i (i)}
	<div
		class={`animate-[shimmer_1.6s_linear_infinite] rounded-md bg-[linear-gradient(90deg,var(--c-surface-2)_25%,var(--c-ghost)_50%,var(--c-surface-2)_75%)] bg-[length:200%_100%] ${className}`}
		aria-hidden="true"
	></div>
{/each}
