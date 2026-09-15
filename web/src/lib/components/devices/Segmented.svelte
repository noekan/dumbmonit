<script lang="ts" generics="T extends string">
	/**
	 * A segmented control: a few exclusive choices in one machined strip.
	 * Buttons carry `aria-pressed`; the optional count sits after the word
	 * in tabular figures so the strip does not jitter when counts change.
	 */
	interface Option {
		id: T;
		label: string;
		count?: number;
	}

	interface Props {
		options: readonly Option[];
		value: T;
		onchange: (value: T) => void;
		label: string;
		size?: 'sm' | 'md';
		class?: string;
	}

	let { options, value, onchange, label, size = 'md', class: className = '' }: Props = $props();
</script>

<div
	role="group"
	aria-label={label}
	class={`inline-flex max-w-full items-center gap-0.5 overflow-x-auto rounded-lg border border-line-strong bg-surface-2 p-0.5 shadow-[inset_0_1px_2px_rgb(0_0_0/0.04)] ${className}`}
>
	{#each options as option (option.id)}
		{@const active = option.id === value}
		<button
			type="button"
			aria-pressed={active}
			onclick={() => onchange(option.id)}
			class={`inline-flex shrink-0 items-center gap-1.5 whitespace-nowrap rounded-[6px] font-semibold transition-[background-color,color,box-shadow] duration-200 ease-out-expo ${size === 'sm' ? 'h-7 px-2.5 text-[0.8125rem]' : 'h-8 px-3 text-sm'} ${active ? 'bg-surface text-ink shadow-lift' : 'text-ink-2 hover:text-ink'}`}
		>
			{option.label}
			{#if option.count !== undefined}
				<span class={`tnum text-[0.75rem] ${active ? 'text-ink-2' : 'text-ink-3'}`}>{option.count}</span>
			{/if}
		</button>
	{/each}
</div>
