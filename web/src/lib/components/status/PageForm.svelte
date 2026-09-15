<script lang="ts">
	/**
	 * Create or edit a status page, inline: title, slug (suggested from the
	 * title until typed by hand), description, theme, history depth, published
	 * toggle, and the service picker — tick devices, name them for the public,
	 * group them. Saves the page then its services in one go.
	 */
	import { untrack } from 'svelte';
	import { Check, X } from 'lucide-svelte';
	import {
		createStatusPage,
		setStatusPageItems,
		updateStatusPage,
		type StatusPage,
		type StatusPageItemPayload,
		type StatusPageTheme,
		type Target
	} from '$lib/api';
	import { Button, ErrorNotice, Field, Toggle } from '$lib/ui';
	import { slugify } from './words';

	interface Props {
		/** `null` creates a page. */
		page: StatusPage | null;
		targets: Target[];
		onsaved: (page: StatusPage) => void;
		oncancel: () => void;
	}

	let { page, targets, onsaved, oncancel }: Props = $props();

	// The form seeds itself once from the page it was opened for; the parent
	// re-mounts it for another page.
	const initial = untrack(() => page);

	const SLUG_RULE = /^[a-z0-9-]{2,40}$/;
	const HISTORY_CHOICES = [30, 60, 90];

	let title = $state(initial?.title ?? '');
	let slug = $state(initial?.slug ?? '');
	let slugTouched = $state(initial !== null);
	let description = $state(initial?.description ?? '');
	let theme = $state<StatusPageTheme>(initial?.theme ?? 'auto');
	let published = $state(initial?.published ?? false);
	let showDays = $state(initial?.show_uptime_days ?? 90);

	// Picker state: per target, whether it is shown and how.
	interface Pick {
		label: string;
		group: string;
	}
	let picks = $state<Map<number, Pick>>(
		new Map((initial?.items ?? []).map((item) => [item.target_id, { label: item.label, group: item.group_name }]))
	);
	// Order of appearance = order of the original list, then order of ticking.
	let order = $state<number[]>((initial?.items ?? []).map((item) => item.target_id));

	function toggle(target: Target, on: boolean) {
		const next = new Map(picks);
		if (on) {
			next.set(target.id, { label: target.name, group: '' });
			order = [...order.filter((id) => id !== target.id), target.id];
		} else {
			next.delete(target.id);
			order = order.filter((id) => id !== target.id);
		}
		picks = next;
	}

	function setPick(id: number, patch: Partial<Pick>) {
		const current = picks.get(id);
		if (!current) return;
		const next = new Map(picks);
		next.set(id, { ...current, ...patch });
		picks = next;
	}

	function move(id: number, delta: -1 | 1) {
		const index = order.indexOf(id);
		const target = index + delta;
		if (index < 0 || target < 0 || target >= order.length) return;
		const next = [...order];
		[next[index], next[target]] = [next[target], next[index]];
		order = next;
	}

	const groupsSeen = $derived([...new Set([...picks.values()].map((p) => p.group).filter(Boolean))]);
	const targetById = $derived(new Map(targets.map((t) => [t.id, t])));
	const sortedTargets = $derived([...targets].sort((a, b) => a.name.localeCompare(b.name)));

	let titleError = $state<string | null>(null);
	let slugError = $state<string | null>(null);
	let saving = $state(false);
	let error = $state<unknown>(null);

	function onTitleInput() {
		titleError = null;
		if (!slugTouched) slug = slugify(title);
	}

	async function submit(event: SubmitEvent) {
		event.preventDefault();
		error = null;
		titleError = title.trim() ? null : 'Give the page a title.';
		slugError = SLUG_RULE.test(slug) ? null : '2 to 40 characters: lowercase letters, digits and hyphens.';
		if (titleError || slugError) return;

		saving = true;
		try {
			const payload = {
				title: title.trim(),
				slug,
				description: description.trim(),
				theme,
				published,
				show_uptime_days: showDays
			};
			const saved = page ? await updateStatusPage(page.id, payload) : await createStatusPage(payload);
			const items: StatusPageItemPayload[] = order
				.filter((id) => picks.has(id))
				.map((id) => {
					const pick = picks.get(id)!;
					return { target_id: id, label: pick.label.trim(), group_name: pick.group.trim() };
				});
			const savedItems = await setStatusPageItems(saved.id, items);
			onsaved({ ...saved, items: savedItems });
		} catch (cause) {
			error = cause;
		} finally {
			saving = false;
		}
	}

	const idPrefix = $derived(page ? `sp-${page.id}` : 'sp-new');
</script>

<form class="grid gap-4" onsubmit={submit} novalidate>
	<div class="grid gap-4 sm:grid-cols-2">
		<Field label="Title" for="{idPrefix}-title" error={titleError} required>
			<input
				id="{idPrefix}-title"
				type="text"
				class="input"
				bind:value={title}
				oninput={onTitleInput}
				placeholder="Home lab"
				maxlength="120"
				disabled={saving}
				aria-invalid={titleError ? 'true' : undefined}
			/>
		</Field>
		<Field label="Address" for="{idPrefix}-slug" error={slugError} help={slug ? `Public URL: /s/${slug}` : 'Lowercase letters, digits and hyphens.'} required>
			<input
				id="{idPrefix}-slug"
				type="text"
				class="input font-mono"
				bind:value={slug}
				oninput={() => {
					slugTouched = true;
					slugError = null;
				}}
				placeholder="home-lab"
				maxlength="40"
				autocomplete="off"
				spellcheck="false"
				disabled={saving}
				aria-invalid={slugError ? 'true' : undefined}
			/>
		</Field>
	</div>
	<Field label="Description" for="{idPrefix}-description" help="One sentence under the title. Optional.">
		<input id="{idPrefix}-description" type="text" class="input" bind:value={description} maxlength="1000" placeholder="What is up at home, at a glance." disabled={saving} />
	</Field>
	<div class="grid gap-4 sm:grid-cols-3">
		<Field label="Theme" for="{idPrefix}-theme">
			<select id="{idPrefix}-theme" class="input" bind:value={theme} disabled={saving}>
				<option value="auto">Follow the visitor's system</option>
				<option value="light">Day</option>
				<option value="dark">Night</option>
			</select>
		</Field>
		<Field label="History" for="{idPrefix}-days">
			<select id="{idPrefix}-days" class="input" bind:value={showDays} disabled={saving}>
				{#each HISTORY_CHOICES as choice (choice)}
					<option value={choice}>{choice} days</option>
				{/each}
			</select>
		</Field>
		<Field label="Published" for="{idPrefix}-published" inline help={published ? 'Anyone with the link can see it.' : 'Draft: answers 404 to visitors.'}>
			<Toggle id="{idPrefix}-published" bind:checked={published} disabled={saving} label="Published" />
		</Field>
	</div>

	<!-- Service picker -->
	<fieldset class="grid gap-2" disabled={saving}>
		<legend class="text-sm font-semibold text-ink">Services</legend>
		<p class="text-[0.8125rem] text-ink-2">Tick the devices to show. The label is what visitors read; a group heads a block of services.</p>
		{#if sortedTargets.length === 0}
			<p class="ghost-cell rounded-lg border border-dashed border-line px-4 py-6 text-center text-sm text-ink-2">No device yet. Add one on the Devices page first.</p>
		{:else}
			<ul class="divide-y divide-line rounded-[var(--radius-card)] border border-line" role="list">
				{#each sortedTargets as target (target.id)}
					{@const pick = picks.get(target.id)}
					<li class="px-3 py-2.5">
						<div class="flex items-center gap-3">
							<input
								id="{idPrefix}-pick-{target.id}"
								type="checkbox"
								class="size-4 shrink-0 accent-signal"
								checked={pick !== undefined}
								onchange={(event) => toggle(target, event.currentTarget.checked)}
							/>
							<label for="{idPrefix}-pick-{target.id}" class="min-w-0 flex-1 truncate text-sm text-ink">
								{target.name}
								<span class="text-ink-3">· {target.kind}</span>
							</label>
						</div>
						{#if pick}
							<div class="mt-2 grid gap-2 pl-7 sm:grid-cols-[minmax(0,1fr)_minmax(0,1fr)_auto] sm:items-center">
								<input
									type="text"
									class="input"
									value={pick.label}
									oninput={(event) => setPick(target.id, { label: event.currentTarget.value })}
									placeholder={target.name}
									maxlength="80"
									aria-label={`Public label of ${target.name}`}
								/>
								<input
									type="text"
									class="input"
									value={pick.group}
									oninput={(event) => setPick(target.id, { group: event.currentTarget.value })}
									placeholder="Group (optional)"
									maxlength="60"
									list="{idPrefix}-groups"
									aria-label={`Group of ${target.name}`}
								/>
								<div class="flex gap-1">
									<Button variant="ghost" size="sm" onclick={() => move(target.id, -1)} disabled={order.indexOf(target.id) <= 0}>Up</Button>
									<Button variant="ghost" size="sm" onclick={() => move(target.id, 1)} disabled={order.indexOf(target.id) >= order.length - 1}>Down</Button>
								</div>
							</div>
						{/if}
					</li>
				{/each}
			</ul>
			<datalist id="{idPrefix}-groups">
				{#each groupsSeen as group (group)}<option value={group}></option>{/each}
			</datalist>
			{#if order.length > 0}
				<p class="text-[0.8125rem] text-ink-2">
					Shown in this order: {order.map((id) => picks.get(id)?.label || targetById.get(id)?.name || id).join(', ')}.
				</p>
			{/if}
		{/if}
	</fieldset>

	{#if error}
		<ErrorNotice {error} title={page ? 'Could not save the page' : 'Could not create the page'} />
	{/if}

	<div class="flex flex-wrap items-center gap-2">
		<Button type="submit" variant="primary" loading={saving}>
			<Check class="size-4" aria-hidden="true" />
			{page ? 'Save changes' : 'Create page'}
		</Button>
		<Button variant="ghost" onclick={oncancel} disabled={saving}>
			<X class="size-4" aria-hidden="true" />
			Cancel
		</Button>
	</div>
</form>
