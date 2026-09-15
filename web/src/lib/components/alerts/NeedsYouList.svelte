<script lang="ts">
	/**
	 * The "Needs you" list, shared by the Overview and the Alerts "Now" tab.
	 *
	 * Rows come pre-ordered from the sky helper: unreachable devices and
	 * warnings first, then advisories, building up, and the suppressed last.
	 * When `grouped` is set, rows are gathered under their device (host
	 * grouping), which is how the Alerts page reads them; the Overview leaves
	 * them as one flat stream. The empty state only appears when the sky says
	 * so — a device that has stopped reporting is never "nothing".
	 */
	import type { Alert, Target } from '$lib/api';
	import type { Sky, SkyRow } from '$lib/components/overview/sky';
	import { EmptyState } from '$lib/ui';
	import { CloudSun } from 'lucide-svelte';
	import AlertRow from './AlertRow.svelte';
	import DeviceRow from './DeviceRow.svelte';

	interface Props {
		sky: Sky;
		/** Relative time of the last check, for the empty state's second line. */
		checkedLabel?: string;
		grouped?: boolean;
		showOpen?: boolean;
		/** Fingerprint of the alert whose silence request is in flight. */
		silencingKey?: string | null;
		/** The pigeon instead of the icon in the quiet state (the Overview smiles). */
		mascot?: 'watch' | 'dizzy' | 'happy';
		onsilence: (alert: Alert, target: Target) => void;
	}

	let {
		sky,
		checkedLabel,
		grouped = false,
		showOpen = false,
		silencingKey = null,
		mascot,
		onsilence
	}: Props = $props();

	/**
	 * One rule firing on several series of the same device (four filesystems
	 * nearly full) is one problem, not four: consecutive alert rows sharing the
	 * device, rule and phase fold into the first, which lists the others.
	 */
	interface Folded {
		row: SkyRow;
		extra: Alert[];
	}
	function fold(input: SkyRow[]): Folded[] {
		const out: Folded[] = [];
		for (const row of input) {
			const last = out[out.length - 1];
			if (
				row.kind === 'alert' &&
				last &&
				last.row.kind === 'alert' &&
				last.row.alert.target_id === row.alert.target_id &&
				last.row.alert.rule_uid === row.alert.rule_uid &&
				last.row.alert.effective_phase === row.alert.effective_phase
			) {
				last.extra.push(row.alert);
				continue;
			}
			out.push({ row, extra: [] });
		}
		return out;
	}

	const rows = $derived(fold(sky.needsYou));

	/** What "quiet" means right now: everything reporting, or still waiting. */
	const quietDescription = $derived.by(() => {
		const { waiting, devices } = sky.counts;
		const base =
			devices === 0
				? 'Add a device and its first report shows up here.'
				: waiting > 0
					? `No rule is firing. ${waiting === 1 ? 'One device is' : `${waiting} devices are`} still waiting for a first report.`
					: 'Every device is reporting and no rule is firing.';
		return checkedLabel ? `${base} Checked ${checkedLabel}.` : base;
	});

	/** Rows gathered per device, keeping the ordered stream within each group. */
	interface Group {
		key: string;
		name: string;
		target?: Target;
		items: Folded[];
	}
	const groups = $derived.by<Group[]>(() => {
		const map = new Map<string, Group>();
		for (const folded of rows) {
			const row = folded.row;
			const target = row.target;
			const targetId = row.kind === 'device' ? row.target.id : row.alert.target_id;
			const key = targetId === null ? 'none' : String(targetId);
			let group = map.get(key);
			if (!group) {
				group = {
					key,
					name: target?.name ?? (targetId === null ? 'No device' : `Device ${targetId}`),
					target,
					items: []
				};
				map.set(key, group);
			}
			group.items.push(folded);
		}
		return [...map.values()];
	});
</script>

{#snippet rowView(folded: Folded)}
	{#if folded.row.kind === 'device'}
		<DeviceRow row={folded.row} />
	{:else}
		<AlertRow
			row={folded.row}
			extra={folded.extra}
			{showOpen}
			silencing={silencingKey === folded.row.alert.fingerprint}
			{onsilence}
		/>
	{/if}
{/snippet}

{#if sky.quiet}
	<EmptyState
		tone="signal"
		icon={mascot ? undefined : CloudSun}
		{mascot}
		title={sky.counts.devices === 0 ? 'Nothing to watch yet.' : 'Nothing needs you.'}
		description={quietDescription}
	/>
{:else if grouped}
	<div class="space-y-6">
		{#each groups as group (group.key)}
			<div>
				<h3 class="label-tape mb-2">
					{#if group.target}
						<a href={`/targets/${group.target.id}`} class="hover:text-ink hover:underline"
							>{group.name}</a
						>
					{:else}
						{group.name}
					{/if}
				</h3>
				<div class="space-y-2.5">
					{#each group.items as folded, i (folded.row.key)}
						<div class="rise-in" style={`--rise-delay: ${i * 30}ms`}>
							{@render rowView(folded)}
						</div>
					{/each}
				</div>
			</div>
		{/each}
	</div>
{:else}
	<div class="space-y-2.5">
		{#each rows as folded, i (folded.row.key)}
			<div class="rise-in" style={`--rise-delay: ${i * 30}ms`}>
				{@render rowView(folded)}
			</div>
		{/each}
	</div>
{/if}
