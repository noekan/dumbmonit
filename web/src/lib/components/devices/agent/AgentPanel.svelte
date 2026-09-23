<script lang="ts">
	/**
	 * Agent panel of a device: which machine the agent describes, and — the part
	 * that matters for security — whether this registration is bound to that one
	 * agent installation.
	 *
	 * Unbound is not a failure: agents installed before binding existed keep
	 * reporting. But it is worth saying, because until they are bound any other
	 * machine holding the same fleet token can push in their name and take their
	 * container commands.
	 */
	import { ShieldCheck, ShieldAlert, ShieldQuestion } from 'lucide-svelte';
	import { allowAgentRebind, getAgentHost, type AgentHost, type Target } from '$lib/api';
	import { formatDateTime, formatRelative } from '$lib/format';
	import { auth } from '$lib/stores/auth.svelte';
	import { Button, Confirm, ErrorNotice, Panel, Plate } from '$lib/ui';
	import RelayPanel from '$lib/components/devices/relay/RelayPanel.svelte';

	interface Props {
		target: Target;
	}

	let { target }: Props = $props();

	let host = $state<AgentHost | null>(null);
	let rebinding = $state(false);
	let rebindError = $state<unknown>(null);
	let reloads = $state(0);

	$effect(() => {
		const id = target.id;
		// Read so the effect re-runs after a re-enrolment window is opened.
		reloads;
		const controller = new AbortController();
		getAgentHost(id, controller.signal)
			.then((agent) => (host = agent))
			.catch(() => (host = null));
		return () => controller.abort();
	});

	const BINDING = {
		bound: {
			tone: 'signal' as const,
			icon: ShieldCheck,
			label: 'Bound',
			detail:
				'This agent holds a secret of its own. No other machine can push measurements in its name, or pick up its container commands.'
		},
		pending: {
			tone: 'advisory' as const,
			icon: ShieldQuestion,
			label: 'Not bound yet',
			detail:
				'The agent knows how to be bound and will be at its next batch. Until then, any machine holding the same enrolment token could report in its name.'
		},
		unsupported: {
			tone: 'warning' as const,
			icon: ShieldAlert,
			label: 'Not bound — agent too old',
			detail:
				'This agent predates binding and cannot be bound. It keeps reporting, but any machine holding the same enrolment token could report in its name. Re-run the install command on it to fix that.'
		}
	};

	const binding = $derived(host ? BINDING[host.binding] : null);
	/** A re-enrolment window that has not run out yet. */
	const window_ = $derived.by(() => {
		if (!host?.rebind_until) return null;
		return new Date(host.rebind_until + 'Z') > new Date() ? host.rebind_until : null;
	});

	async function rebind() {
		rebinding = true;
		rebindError = null;
		try {
			await allowAgentRebind(target.id);
			reloads += 1;
		} catch (cause) {
			rebindError = cause;
		} finally {
			rebinding = false;
		}
	}
</script>

{#if host && binding}
	{@const Icon = binding.icon}
	<Panel title="Agent" description="The machine as its agent describes it, and how the server knows it is really this one.">
		{#snippet aside()}
			<Plate tone={binding.tone} label={binding.label} />
		{/snippet}

		<dl class="grid gap-x-6 gap-y-2 text-sm sm:grid-cols-[auto_1fr]">
			<dt class="text-ink-2">Machine</dt>
			<dd class="text-ink">{host.hostname}</dd>
			<dt class="text-ink-2">System</dt>
			<dd class="text-ink">{host.os_version ?? host.os}{host.arch ? ` · ${host.arch}` : ''}</dd>
			<dt class="text-ink-2">Agent</dt>
			<dd class="text-ink">{host.agent_version}</dd>
			<dt class="text-ink-2">Last batch</dt>
			<dd class="text-ink">
				<time class="tnum" title={formatDateTime(host.last_seen_at)}>{formatRelative(host.last_seen_at)}</time>
			</dd>
			<dt class="text-ink-2">Binding</dt>
			<dd class="text-ink">
				{#if host.bound_at}
					Bound <time class="tnum" title={formatDateTime(host.bound_at)}>{formatRelative(host.bound_at)}</time>
				{:else}
					Not bound
				{/if}
			</dd>
		</dl>

		<p class="mt-3 flex items-start gap-2 text-sm text-ink-2">
			<Icon class="mt-0.5 size-4 shrink-0" aria-hidden="true" />
			<span>{binding.detail}</span>
		</p>

		{#if window_}
			<p class="mt-3 rounded-[var(--radius-card)] border border-advisory/40 bg-surface px-3 py-2 text-sm text-ink">
				Re-enrolment is open until
				<time class="tnum" title={formatDateTime(window_)}>{formatDateTime(window_)}</time>. The next batch
				from an agent with a valid token will bind this machine again.
			</p>
		{:else if host.bound && auth.isAdmin}
			<div class="mt-3 flex flex-wrap items-center gap-3">
				<Confirm confirmLabel="Open the window?" loading={rebinding} onconfirm={rebind}>Allow re-enrolment</Confirm>
				<p class="text-sm text-ink-2">
					Use this after reinstalling the machine, when the agent lost the secret it had. It opens a short
					window during which the agent binds itself again.
				</p>
			</div>
		{/if}

		{#if rebindError}
			<ErrorNotice error={rebindError} title="Could not open the re-enrolment window" class="mt-3" />
		{/if}
	</Panel>
{/if}

<RelayPanel {target} />
