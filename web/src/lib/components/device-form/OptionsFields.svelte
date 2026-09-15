<script lang="ts">
	/**
	 * Settings specific to a kind, as described by the server.
	 *
	 * Each option becomes a typed control; values stay strings because they end
	 * up in `Target.tags`. A blank value is never sent: the server then applies
	 * its own default.
	 */
	import type { CollectorOption } from '$lib/api';
	import { Field, Toggle } from '$lib/ui';

	interface Props {
		options: CollectorOption[];
		/** Current values by option key. A missing key means "server default". */
		values: Record<string, string>;
		errors?: Record<string, string>;
		onchange: (values: Record<string, string>) => void;
	}

	let { options, values, errors = {}, onchange }: Props = $props();

	function set(key: string, value: string) {
		const next = { ...values };
		if (value.trim()) next[key] = value;
		else delete next[key];
		onchange(next);
	}

	function checked(option: CollectorOption): boolean {
		return (values[option.key] ?? option.default).trim().toLowerCase() === 'true';
	}

	/** Text inputs get the full row: they often hold lists or long values. */
	function span(option: CollectorOption): string {
		return option.input === 'text' || option.input === 'boolean' ? 'sm:col-span-2' : '';
	}
</script>

<div class="grid gap-4 sm:grid-cols-2">
	{#each options as option (option.key)}
		{@const id = `option-${option.key}`}
		<div class={span(option)}>
			{#if option.input === 'boolean'}
				<Field label={option.label} for={id} help={option.help} inline>
					<Toggle {id} checked={checked(option)} onchange={(value) => set(option.key, value ? 'true' : 'false')} />
				</Field>
			{:else if option.input === 'select'}
				<Field label={option.label} for={id} help={option.help} required={option.required} error={errors[option.key]}>
					<select
						{id}
						class="input"
						value={values[option.key] ?? (option.required ? option.default : '')}
						onchange={(e) => set(option.key, e.currentTarget.value)}
						aria-invalid={errors[option.key] ? 'true' : undefined}
					>
						{#if !option.required}
							<option value="">{option.default ? `Default (${option.default})` : 'Default'}</option>
						{/if}
						{#each option.choices as choice (choice)}
							<option value={choice}>{choice}</option>
						{/each}
					</select>
				</Field>
			{:else}
				<Field label={option.label} for={id} help={option.help} required={option.required} error={errors[option.key]}>
					<input
						{id}
						class={`input ${option.input === 'number' ? 'tnum' : ''}`}
						type={option.input === 'number' ? 'number' : 'text'}
						inputmode={option.input === 'number' ? 'decimal' : undefined}
						value={values[option.key] ?? ''}
						placeholder={option.placeholder || option.default}
						autocomplete="off"
						oninput={(e) => set(option.key, e.currentTarget.value)}
						aria-invalid={errors[option.key] ? 'true' : undefined}
					/>
				</Field>
			{/if}
		</div>
	{/each}
</div>
