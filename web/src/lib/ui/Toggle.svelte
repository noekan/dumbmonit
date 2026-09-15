<script lang="ts">
	/** A switch. Bind `checked`. */
	interface Props {
		checked: boolean;
		id?: string;
		disabled?: boolean;
		label?: string;
		onchange?: (value: boolean) => void;
	}
	let { checked = $bindable(false), id, disabled = false, label, onchange }: Props = $props();
</script>

<button
	{id}
	type="button"
	role="switch"
	aria-checked={checked}
	aria-label={label}
	{disabled}
	class={`relative inline-flex h-6 w-11 shrink-0 items-center rounded-full border transition-colors duration-200 disabled:opacity-50 ${checked ? 'border-signal bg-signal' : 'border-line-strong bg-surface-2'}`}
	onclick={() => {
		checked = !checked;
		onchange?.(checked);
	}}
>
	<span
		class={`absolute left-0.5 size-[1.125rem] rounded-full bg-white shadow-sm transition-transform duration-200 ease-spring ${checked ? 'translate-x-5' : 'translate-x-0'}`}
	></span>
</button>
