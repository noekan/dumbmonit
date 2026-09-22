<script lang="ts">
	/**
	 * Everything the device page shows for a Proxmox VE hypervisor, in the
	 * order an administrator reads it: the nodes first (if the cluster itself
	 * is unwell, nothing below matters), then Ceph when there is one, then the
	 * guests.
	 *
	 * Each section loads and refreshes on its own and hides or degrades on its
	 * own: an older server, a missing privilege or a cluster without Ceph
	 * simply leaves that section out, never an error.
	 */
	import type { Target } from '$lib/api';
	import CephPanel from './CephPanel.svelte';
	import GuestTable from './GuestTable.svelte';
	import NodesPanel from './NodesPanel.svelte';

	interface Props {
		target: Target;
	}

	let { target }: Props = $props();
</script>

<div class="flex flex-col gap-4">
	<NodesPanel {target} />
	<CephPanel {target} />
	<GuestTable {target} />
</div>
