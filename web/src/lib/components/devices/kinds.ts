/**
 * Kind-specific panels of the device page.
 *
 * A collector kind that deserves more than the generic charts (Proxmox VE with
 * its guests, PBS with its backup calendar, Synology with its disks…) registers
 * a panel here; the page mounts it between the alerts and the instruments,
 * with `{ target }` as its only prop. One line per kind, so several people can
 * add theirs without touching the page.
 */
import type { Component } from 'svelte';
import type { Target } from '$lib/api/types';
import PbsPanel from './pbs/PbsPanel.svelte';
import ProxmoxPanel from './proxmox/ProxmoxPanel.svelte';
import RelayPanel from './relay/RelayPanel.svelte';
import SynologyPanel from './synology/SynologyPanel.svelte';

export type KindPanel = Component<{ target: Target }>;

export const kindPanels: Record<string, KindPanel> = { pbs: PbsPanel, agent: RelayPanel, proxmox: ProxmoxPanel, synology: SynologyPanel };

/** The panel for a kind, or `null` when the generic charts are all there is. */
export function kindPanel(kind: string): KindPanel | null {
	return kindPanels[kind] ?? null;
}
