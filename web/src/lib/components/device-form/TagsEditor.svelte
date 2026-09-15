<script lang="ts">
	/**
	 * Free tags as `key=value` chips. Type `room=rack-1`, press Enter; each chip
	 * has its own remove button. Keys that belong to the kind's settings are
	 * handled by OptionsFields and never appear here.
	 */
	import { X } from 'lucide-svelte';
	import { Field } from '$lib/ui';

	interface Props {
		tags: Record<string, string>;
		/** Keys reserved by the kind's options: typing one is refused with a hint. */
		reserved?: string[];
		onchange: (tags: Record<string, string>) => void;
	}

	let { tags, reserved = [], onchange }: Props = $props();

	let draft = $state('');
	let problem = $state<string | null>(null);

	const entries = $derived(Object.entries(tags));

	function add() {
		const raw = draft.trim();
		if (!raw) return;
		const eq = raw.indexOf('=');
		const key = (eq === -1 ? raw : raw.slice(0, eq)).trim();
		const value = eq === -1 ? '' : raw.slice(eq + 1).trim();
		if (!key) {
			problem = 'A tag needs a key, as in room=rack-1.';
			return;
		}
		if (reserved.includes(key)) {
			problem = `"${key}" is a setting of this type: use its field above.`;
			return;
		}
		onchange({ ...tags, [key]: value });
		draft = '';
		problem = null;
	}

	function remove(key: string) {
		const next = { ...tags };
		delete next[key];
		onchange(next);
	}

	function onkeydown(event: KeyboardEvent) {
		if (event.key === 'Enter') {
			event.preventDefault();
			add();
		} else if (event.key === 'Escape') {
			draft = '';
			problem = null;
		} else if (event.key === 'Backspace' && draft === '' && entries.length > 0) {
			remove(entries[entries.length - 1][0]);
		}
	}
</script>

<Field label="Tags" for="tag-draft" help="Group and filter devices, e.g. room=rack-1. Press Enter to add." error={problem}>
	<div class="input flex min-h-10 flex-wrap items-center gap-1.5 py-1.5" onclick={(e) => (e.currentTarget.querySelector('input')?.focus())} role="presentation">
		{#each entries as [key, value] (key)}
			<span class="inline-flex h-7 items-center gap-1 rounded-[var(--radius-plate)] border border-line bg-surface-2 pl-2 text-[0.8125rem] text-ink">
				<span class="font-medium">{key}</span>{#if value}<span class="text-ink-2">={value}</span>{/if}
				<button
					type="button"
					class="flex size-6 items-center justify-center rounded-full text-ink-3 hover:text-warning-ink"
					aria-label={`Remove tag ${key}`}
					onclick={() => remove(key)}
				>
					<X class="size-3.5" aria-hidden="true" />
				</button>
			</span>
		{/each}
		<input
			id="tag-draft"
			type="text"
			class="min-w-[8rem] flex-1 bg-transparent text-sm text-ink outline-none placeholder:text-ink-3"
			placeholder={entries.length ? 'key=value' : 'room=rack-1'}
			autocomplete="off"
			bind:value={draft}
			onkeydown={onkeydown}
			onblur={add}
			oninput={() => (problem = null)}
		/>
	</div>
</Field>
