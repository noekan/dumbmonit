/**
 * Active alert counter, shared by the whole application.
 *
 * It feeds the badge in the navigation bar. Polling is deliberately slow
 * (30 s): the interface is not a real-time console, and a homelab gains
 * nothing from hammering its own server.
 */
import { browser } from '$app/environment';
import { listAlerts, listTargets, type Alert, type Target } from '$lib/api';
import { displayState, type ProbeStatus } from '$lib/format';
import { loadProbeStatuses } from '$lib/metrics';

const INTERVAL_MS = 30_000;

class AlertsStore {
	alerts = $state<Alert[]>([]);
	/** Devices, refreshed with the alerts so the badge can count unreachable ones. */
	targets = $state<Target[]>([]);
	probes = $state<Map<number, ProbeStatus>>(new Map());
	/** True until the first response arrives. */
	loading = $state(true);
	/** False if the `/api/alerts` route is not served by the backend yet. */
	available = $state(true);
	#subscribers = 0;

	/**
	 * Alerts that actually need attention: firing right now, and not suppressed
	 * by an offline parent. Pending ("building up") and resolved ones are not
	 * counted — they are not yet, or no longer, a problem.
	 */
	get activeCount(): number {
		const firing = this.alerts.filter((alert) => alert.effective_phase === 'firing');
		const covered = new Set(firing.map((alert) => alert.target_id));
		// A device that is unreachable but has no firing alert yet (no data, rule
		// still evaluating) still needs attention: count it once, like the overview.
		const unreachable = this.targets.filter(
			(target) => {
				const state = displayState(target, this.probes.get(target.id));
				return (state === 'offline' || state === 'down') && !covered.has(target.id);
			}
		).length;
		return firing.length + unreachable;
	}

	async refresh(signal?: AbortSignal): Promise<void> {
		try {
			const [alerts, targets, probes] = await Promise.all([
				listAlerts(signal),
				listTargets(signal).catch(() => [] as Target[]),
				loadProbeStatuses(signal).catch(() => new Map<number, ProbeStatus>())
			]);
			this.alerts = alerts;
			this.targets = targets;
			this.probes = probes;
			this.available = true;
		} catch (cause) {
			if (cause instanceof DOMException && cause.name === 'AbortError') return;
			// A polling failure must not pollute the interface: the Alerts page
			// shows the error in detail, the badge simply shows nothing.
			this.alerts = [];
			this.available = false;
		} finally {
			this.loading = false;
		}
	}

	/**
	 * Starts polling. Counts its callers so that a single timer runs, and
	 * returns the stop function to hand to an `$effect`.
	 */
	startPolling(): () => void {
		if (!browser) return () => {};
		this.#subscribers += 1;
		const controller = new AbortController();
		void this.refresh(controller.signal);
		const timer = setInterval(() => void this.refresh(controller.signal), INTERVAL_MS);
		return () => {
			this.#subscribers -= 1;
			controller.abort();
			clearInterval(timer);
		};
	}
}

export const alertsStore = new AlertsStore();
