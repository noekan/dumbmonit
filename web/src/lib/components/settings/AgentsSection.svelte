<script lang="ts">
	/**
	 * Settings → Agents: enrolment tokens for machines that run the agent.
	 *
	 * A token is shown in clear once, right after creation, together with the
	 * install commands. After that only its prefix is ever displayed.
	 */
	import { Cpu, KeyRound } from 'lucide-svelte';
	import { createAgentToken, listAgentTokens, revokeAgentToken, type AgentToken, type CreatedAgentToken } from '$lib/api';
	import { formatDateTime, formatRelative } from '$lib/format';
	import { auth } from '$lib/stores/auth.svelte';
	import { Button, Confirm, CopyBlock, EmptyState, ErrorNotice, Field, Panel, Plate, Skeleton } from '$lib/ui';

	let tokens = $state<AgentToken[]>([]);
	let loading = $state(true);
	let error = $state<unknown>(null);

	async function load(signal?: AbortSignal) {
		loading = true;
		error = null;
		try {
			const list = await listAgentTokens(signal);
			tokens = Array.isArray(list) ? list : [];
		} catch (cause) {
			if (cause instanceof DOMException && cause.name === 'AbortError') return;
			error = cause;
		} finally {
			loading = false;
		}
	}

	$effect(() => {
		const controller = new AbortController();
		void load(controller.signal);
		return () => controller.abort();
	});

	// --- Create ---------------------------------------------------------------

	let name = $state('');
	let nameError = $state<string | null>(null);
	let creating = $state(false);
	let createError = $state<unknown>(null);
	let created = $state<CreatedAgentToken | null>(null);

	async function create(event: SubmitEvent) {
		event.preventDefault();
		createError = null;
		if (!name.trim()) {
			nameError = 'Name the token, for example after the machine it will enrol.';
			return;
		}
		nameError = null;
		creating = true;
		try {
			// `location.origin` is the address machines will reach this server at.
			created = await createAgentToken(name.trim(), location.origin);
			tokens = [...tokens, created];
			name = '';
		} catch (cause) {
			createError = cause;
		} finally {
			creating = false;
		}
	}

	// --- Revoke ---------------------------------------------------------------

	let revoking = $state<number | null>(null);
	let revokeError = $state<{ id: number; cause: unknown } | null>(null);

	async function revoke(token: AgentToken) {
		revoking = token.id;
		revokeError = null;
		try {
			await revokeAgentToken(token.id);
			tokens = await listAgentTokens();
			if (created?.id === token.id) created = null;
		} catch (cause) {
			revokeError = { id: token.id, cause };
		} finally {
			revoking = null;
		}
	}
</script>

<Panel id="agents" title="Agents" description="Install the agent on Linux or Windows machines that don't speak SNMP. It registers itself as a device." padded={false}>
	{#snippet aside()}
		{#if !auth.isAdmin}
			<Plate tone="ghost" label="Viewer — read only" />
		{/if}
	{/snippet}

	<div class="px-5 py-4">
		{#if auth.isAdmin}
		<form class="flex flex-col gap-3 sm:flex-row sm:items-start" onsubmit={create} novalidate>
			<Field label="New token" for="token-name" error={nameError} class="flex-1" help="Only used to recognise the token in this list.">
				<input
					id="token-name"
					type="text"
					class="input"
					bind:value={name}
					placeholder="File server"
					autocomplete="off"
					disabled={creating}
					aria-invalid={nameError ? 'true' : undefined}
					oninput={() => (nameError = null)}
				/>
			</Field>
			<!-- Offset by the label height so the button sits level with the input. -->
			<Button type="submit" variant="secondary" class="sm:mt-[1.625rem]" loading={creating}>
				<KeyRound class="size-4" aria-hidden="true" />
				Create token
			</Button>
		</form>
		{#if createError}
			<ErrorNotice error={createError} title="Could not create the token" class="mt-3" />
		{/if}
		{/if}

		<div aria-live="polite">
			{#if created}
				<div class="rise-in mt-4 rounded-[var(--radius-card)] border border-advisory/40 bg-surface p-4">
					<div class="flex flex-wrap items-center justify-between gap-2">
						<div class="flex flex-wrap items-center gap-2">
							<p class="font-semibold text-ink">Token “{created.name}” created</p>
							<Plate tone="advisory" label="Shown once — copy it now" />
						</div>
						<Button variant="ghost" size="sm" onclick={() => (created = null)}>I've copied it</Button>
					</div>
					<div class="mt-4 grid gap-4">
						<div>
							<p class="mb-1.5 text-sm font-semibold text-ink">Token</p>
							<CopyBlock value={created.secret} label="Copy token" secret />
						</div>
						{#if created.install_linux}
							<div>
								<p class="mb-1.5 text-sm font-semibold text-ink">Install on Linux</p>
								<CopyBlock value={created.install_linux} label="Copy command" />
							</div>
						{/if}
						{#if created.install_windows}
							<div>
								<p class="mb-1.5 text-sm font-semibold text-ink">Install on Windows (PowerShell)</p>
								<CopyBlock value={created.install_windows} label="Copy command" />
							</div>
						{/if}
					</div>
				</div>
			{/if}
		</div>

		<div class={auth.isAdmin ? 'mt-4' : ''}>
			{#if error}
				<ErrorNotice {error} title="Could not load the tokens" onretry={() => void load()} />
			{:else if loading}
				<Skeleton class="h-14 w-full" />
			{:else if tokens.length === 0}
				<EmptyState icon={Cpu} title="No token yet." description="Create one, then run the install command on the machine." />
			{:else}
				<ul class="divide-y divide-line rounded-[var(--radius-card)] border border-line" role="list">
					{#each tokens as token (token.id)}
						{@const revoked = token.revoked_at !== null}
						<li class={`px-4 py-3 ${revoked ? 'ghost-cell' : ''}`}>
							<div class="flex flex-wrap items-center gap-x-3 gap-y-2">
								<div class="min-w-0 flex-1">
									<div class="flex flex-wrap items-center gap-2">
										<span class={`font-semibold ${revoked ? 'text-ink-2' : 'text-ink'}`}>{token.name}</span>
										<code class="rounded-md border border-line bg-canvas-deep px-1.5 py-0.5 font-mono text-[0.75rem] text-ink-2">{token.prefix}…</code>
										{#if revoked}<Plate tone="ghost" label="Revoked" />{/if}
									</div>
									<p class="mt-1 text-sm text-ink-2">
										Created <time class="tnum" title={formatDateTime(token.created_at)}>{formatRelative(token.created_at)}</time>
										· Last used <time class="tnum" title={formatDateTime(token.last_used_at)}>{formatRelative(token.last_used_at)}</time>
										{#if revoked}
											· Revoked <time class="tnum" title={formatDateTime(token.revoked_at)}>{formatRelative(token.revoked_at)}</time>
										{/if}
									</p>
								</div>
								{#if !revoked && auth.isAdmin}
									<Confirm confirmLabel="Revoke for good?" loading={revoking === token.id} onconfirm={() => revoke(token)}>Revoke</Confirm>
								{/if}
							</div>
							{#if revokeError?.id === token.id}
								<ErrorNotice error={revokeError.cause} title="Could not revoke the token" class="mt-3" />
							{/if}
						</li>
					{/each}
				</ul>
			{/if}
		</div>
	</div>
</Panel>
