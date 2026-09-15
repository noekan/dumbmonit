<script lang="ts">
	/**
	 * Settings → Connect an assistant: API tokens for MCP clients (Claude,
	 * ChatGPT, Cursor…) and the ready-to-paste connection snippets.
	 *
	 * A token is shown in clear once, right after creation. The snippets are
	 * always visible so people can see what they will paste before creating
	 * anything; they carry the real token only while it is on screen.
	 */
	import { Bot, KeyRound } from 'lucide-svelte';
	import {
		createApiToken,
		listApiTokens,
		revokeApiToken,
		type ApiToken,
		type ApiTokenScope,
		type CreatedApiToken
	} from '$lib/api';
	import { formatDateTime, formatRelative } from '$lib/format';
	import { Button, Confirm, CopyBlock, EmptyState, ErrorNotice, Field, Panel, Plate, Skeleton } from '$lib/ui';

	const EXAMPLE_PROMPTS = ['Is everything fine?', 'Silence the NAS for two hours', 'What happened last night?', 'How full is the backup server?'];

	let tokens = $state<ApiToken[]>([]);
	let loading = $state(true);
	let error = $state<unknown>(null);

	async function load(signal?: AbortSignal) {
		loading = true;
		error = null;
		try {
			const list = await listApiTokens(signal);
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
	let scope = $state<ApiTokenScope>('read');
	let nameError = $state<string | null>(null);
	let creating = $state(false);
	let createError = $state<unknown>(null);
	let created = $state<CreatedApiToken | null>(null);

	async function create(event: SubmitEvent) {
		event.preventDefault();
		createError = null;
		if (!name.trim()) {
			nameError = 'Name the token, for example after the assistant or the machine it runs on.';
			return;
		}
		nameError = null;
		creating = true;
		try {
			created = await createApiToken(name.trim(), scope);
			tokens = [created, ...tokens];
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

	async function revoke(token: ApiToken) {
		revoking = token.id;
		revokeError = null;
		try {
			await revokeApiToken(token.id);
			tokens = await listApiTokens();
			if (created?.id === token.id) created = null;
		} catch (cause) {
			revokeError = { id: token.id, cause };
		} finally {
			revoking = null;
		}
	}

	// --- Connection snippets ----------------------------------------------------

	type Client = 'claude' | 'chatgpt' | 'cursor';
	const CLIENTS: { id: Client; label: string }[] = [
		{ id: 'claude', label: 'Claude' },
		{ id: 'chatgpt', label: 'ChatGPT' },
		{ id: 'cursor', label: 'Cursor / other' }
	];
	let client = $state<Client>('claude');

	// The URL assistants will reach this server at: what the browser sees.
	const url = $derived(typeof location === 'undefined' ? '/api/mcp' : `${location.origin}/api/mcp`);
	const isHttps = $derived(typeof location === 'undefined' ? false : location.protocol === 'https:');
	const token = $derived(created?.secret ?? 'dmt_…paste-your-token…');
	const bearer = $derived(`Bearer ${token}`);

	const claudeCommand = $derived(`claude mcp add --transport http dumbmonit ${url} --header "Authorization: ${bearer}"`);
	const claudeDesktop = $derived(
		JSON.stringify({ mcpServers: { dumbmonit: { type: 'http', url, headers: { Authorization: bearer } } } }, null, 2)
	);
	const cursorJson = $derived(JSON.stringify({ mcpServers: { dumbmonit: { url, headers: { Authorization: bearer } } } }, null, 2));

	// Arrow keys move between tabs, as a tablist is expected to behave.
	function onTabKey(event: KeyboardEvent) {
		const index = CLIENTS.findIndex((c) => c.id === client);
		if (event.key === 'ArrowRight') client = CLIENTS[(index + 1) % CLIENTS.length].id;
		else if (event.key === 'ArrowLeft') client = CLIENTS[(index - 1 + CLIENTS.length) % CLIENTS.length].id;
		else return;
		event.preventDefault();
		document.getElementById(`assistant-tab-${client}`)?.focus();
	}
</script>

<Panel
	id="assistant"
	title="Connect an assistant"
	description="Let Claude, ChatGPT, Cursor or any MCP client ask DumbMonit how things are. A read token can only look; a write token can also silence a device, run a probe, or switch a device or rule on and off."
	padded={false}
>
	<div class="px-5 py-4">
		<p class="text-sm text-ink-2">Once connected, try asking:</p>
		<ul class="mt-2 flex flex-wrap gap-2" aria-label="Example prompts">
			{#each EXAMPLE_PROMPTS as prompt (prompt)}
				<li class="rounded-lg border border-line bg-canvas-deep px-2.5 py-1 text-sm text-ink">“{prompt}”</li>
			{/each}
		</ul>

		<form class="mt-5 grid gap-3 sm:grid-cols-[minmax(0,1fr)_auto_auto] sm:items-start" onsubmit={create} novalidate>
			<Field label="New token" for="api-token-name" error={nameError} help="Only used to recognise the token in this list.">
				<input
					id="api-token-name"
					type="text"
					class="input"
					bind:value={name}
					placeholder="Claude on my laptop"
					autocomplete="off"
					disabled={creating}
					aria-invalid={nameError ? 'true' : undefined}
					oninput={() => (nameError = null)}
				/>
			</Field>
			<fieldset class="grid gap-1.5" disabled={creating}>
				<legend class="block text-sm font-semibold text-ink">Scope</legend>
				<div class="flex gap-1 rounded-lg border border-line bg-canvas-deep p-1" role="radiogroup" aria-label="Token scope">
					{#each [{ value: 'read', label: 'Read' }, { value: 'write', label: 'Read and write' }] as option (option.value)}
						<label
							class={`cursor-pointer rounded-md px-3 py-1.5 text-sm transition-colors ${scope === option.value ? 'bg-surface font-semibold text-ink shadow-lift' : 'text-ink-2 hover:text-ink'}`}
						>
							<input type="radio" class="sr-only" name="api-token-scope" value={option.value} bind:group={scope} />
							{option.label}
						</label>
					{/each}
				</div>
				<p class="text-[0.8125rem] text-ink-2">{scope === 'read' ? 'Can never change anything.' : 'Can silence, probe, enable and disable.'}</p>
			</fieldset>
			<!-- Offset by the label height so the button sits level with the inputs. -->
			<Button type="submit" variant="secondary" class="sm:mt-[1.625rem]" loading={creating}>
				<KeyRound class="size-4" aria-hidden="true" />
				Create token
			</Button>
		</form>
		{#if createError}
			<ErrorNotice error={createError} title="Could not create the token" class="mt-3" />
		{/if}

		<div aria-live="polite">
			{#if created}
				<div class="rise-in mt-4 rounded-[var(--radius-card)] border border-advisory/40 bg-surface p-4">
					<div class="flex flex-wrap items-center justify-between gap-2">
						<div class="flex flex-wrap items-center gap-2">
							<p class="font-semibold text-ink">Token “{created.name}” created</p>
							<Plate tone="advisory" label="Shown once — copy it now" />
							<Plate tone={created.scope === 'write' ? 'info' : 'ghost'} label={created.scope === 'write' ? 'Read and write' : 'Read only'} bare />
						</div>
						<Button variant="ghost" size="sm" onclick={() => (created = null)}>I've copied it</Button>
					</div>
					<div class="mt-3">
						<CopyBlock value={created.secret} label="Copy token" secret />
					</div>
					<p class="mt-2 text-sm text-ink-2">The snippets below now carry this token. Treat it like a password.</p>
				</div>
			{/if}
		</div>

		<!-- Connection snippets -->
		<div class="mt-6">
			<div class="flex flex-wrap items-center justify-between gap-2">
				<p class="text-sm font-semibold text-ink">Connect</p>
				<div class="flex gap-1 rounded-lg border border-line bg-canvas-deep p-1" role="tablist" aria-label="Assistant">
					{#each CLIENTS as option (option.id)}
						<button
							type="button"
							role="tab"
							id={`assistant-tab-${option.id}`}
							aria-selected={client === option.id}
							aria-controls="assistant-snippets"
							tabindex={client === option.id ? 0 : -1}
							class={`rounded-md px-3 py-1.5 text-sm transition-colors ${client === option.id ? 'bg-surface font-semibold text-ink shadow-lift' : 'text-ink-2 hover:text-ink'}`}
							onclick={() => (client = option.id)}
							onkeydown={onTabKey}
						>
							{option.label}
						</button>
					{/each}
				</div>
			</div>

			<div id="assistant-snippets" role="tabpanel" aria-labelledby={`assistant-tab-${client}`} class="mt-3 grid min-w-0 gap-4">
				{#if client === 'claude'}
					<div>
						<p class="mb-1.5 text-sm text-ink-2">Claude Code — one command:</p>
						<CopyBlock value={claudeCommand} label="Copy command" />
					</div>
					<div>
						<p class="mb-1.5 text-sm text-ink-2">Claude Desktop — add to <code class="font-mono text-[0.8125rem]">claude_desktop_config.json</code> (or paste it under Settings → Connectors → Add custom connector, URL only, if your Claude plan offers it):</p>
						<CopyBlock value={claudeDesktop} label="Copy JSON" />
					</div>
				{:else if client === 'chatgpt'}
					<div class="grid min-w-0 gap-3">
						<p class="text-sm text-ink-2">
							In ChatGPT, open Settings → Connectors → Create (developer mode), then fill in the MCP server URL and the authorization header. ChatGPT connects from OpenAI's servers, so the URL must be reachable from the internet over HTTPS.
						</p>
						{#if !isHttps}
							<Plate tone="advisory" label="This page is not served over HTTPS: put DumbMonit behind a reverse proxy with TLS before exposing it." />
						{/if}
						<div>
							<p class="mb-1.5 text-sm text-ink-2">MCP server URL</p>
							<CopyBlock value={url} label="Copy URL" />
						</div>
						<div>
							<p class="mb-1.5 text-sm text-ink-2">Authorization header</p>
							<CopyBlock value={bearer} label="Copy header" secret={created !== null} />
						</div>
					</div>
				{:else}
					<div>
						<p class="mb-1.5 text-sm text-ink-2">Cursor (<code class="font-mono text-[0.8125rem]">.cursor/mcp.json</code>) and most other MCP clients accept this shape — a Streamable HTTP server with a bearer header:</p>
						<CopyBlock value={cursorJson} label="Copy JSON" />
					</div>
				{/if}
				<p class="text-sm text-ink-2">
					The server speaks MCP over Streamable HTTP (JSON responses, no session). Full instructions and security notes: <a class="underline decoration-line underline-offset-2 hover:text-ink" href="https://dumbmonit.readthedocs.io/en/latest/using/assistant/" target="_blank" rel="noreferrer">Connect an assistant</a>.
				</p>
			</div>
		</div>

		<!-- Token list -->
		<div class="mt-6">
			<p class="mb-2 text-sm font-semibold text-ink">Tokens</p>
			{#if error}
				<ErrorNotice {error} title="Could not load the tokens" onretry={() => void load()} />
			{:else if loading}
				<Skeleton class="h-14 w-full" />
			{:else if tokens.length === 0}
				<EmptyState icon={Bot} title="No token yet." description="Create one above, then paste the snippet into your assistant." />
			{:else}
				<ul class="divide-y divide-line rounded-[var(--radius-card)] border border-line" role="list">
					{#each tokens as item (item.id)}
						{@const revoked = item.revoked_at !== null}
						<li class={`px-4 py-3 ${revoked ? 'ghost-cell' : ''}`}>
							<div class="flex flex-wrap items-center gap-x-3 gap-y-2">
								<div class="min-w-0 flex-1">
									<div class="flex flex-wrap items-center gap-2">
										<span class={`font-semibold ${revoked ? 'text-ink-2' : 'text-ink'}`}>{item.name}</span>
										<code class="rounded-md border border-line bg-canvas-deep px-1.5 py-0.5 font-mono text-[0.75rem] text-ink-2">{item.prefix}…</code>
										<Plate tone={item.scope === 'write' ? 'info' : 'ghost'} label={item.scope === 'write' ? 'Read and write' : 'Read only'} bare />
										{#if revoked}<Plate tone="ghost" label="Revoked" />{/if}
									</div>
									<p class="mt-1 text-sm text-ink-2">
										Created <time class="tnum" title={formatDateTime(item.created_at)}>{formatRelative(item.created_at)}</time>
										· Last used <time class="tnum" title={formatDateTime(item.last_used_at)}>{formatRelative(item.last_used_at)}</time>
										{#if revoked}
											· Revoked <time class="tnum" title={formatDateTime(item.revoked_at)}>{formatRelative(item.revoked_at)}</time>
										{/if}
									</p>
								</div>
								{#if !revoked}
									<Confirm confirmLabel="Revoke for good?" loading={revoking === item.id} onconfirm={() => revoke(item)}>Revoke</Confirm>
								{/if}
							</div>
							{#if revokeError?.id === item.id}
								<ErrorNotice error={revokeError.cause} title="Could not revoke the token" class="mt-3" />
							{/if}
						</li>
					{/each}
				</ul>
			{/if}
		</div>
	</div>
</Panel>
