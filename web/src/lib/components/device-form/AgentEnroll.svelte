<script lang="ts">
	/**
	 * Adding a machine with the agent: no address to type.
	 *
	 * The agent installs on the machine and enrols itself with this server. All
	 * it needs is a token and the command that ships it. The token is shown once:
	 * the server only keeps a fingerprint.
	 */
	import { createAgentToken, type CreatedAgentToken } from '$lib/api';
	import { Button, ClickSpark, CopyBlock, ErrorNotice, Field, Plate } from '$lib/ui';

	interface Props {
		cancelHref?: string;
	}

	let { cancelHref = '/targets' }: Props = $props();

	let name = $state('servers');
	let creating = $state(false);
	let error = $state<unknown>(null);
	let token = $state<CreatedAgentToken | null>(null);

	async function create(event: SubmitEvent) {
		event.preventDefault();
		if (!name.trim() || creating) return;
		creating = true;
		error = null;
		try {
			token = await createAgentToken(name.trim(), window.location.origin);
		} catch (cause) {
			error = cause;
		} finally {
			creating = false;
		}
	}

	function reset() {
		token = null;
		error = null;
	}
</script>

{#if token}
	<div class="grid gap-5" aria-live="polite">
		<div class="flex flex-wrap items-center gap-2">
			<Plate tone="advisory" label="Shown once" />
			<p class="text-sm text-ink">Copy the command now: this token will not be displayed again.</p>
		</div>

		<div class="grid gap-1.5">
			<p class="text-sm font-semibold text-ink">Linux / macOS</p>
			<CopyBlock value={token.install_linux} label="Copy the Linux command" />
		</div>
		<div class="grid gap-1.5">
			<p class="text-sm font-semibold text-ink">Windows (PowerShell, as administrator)</p>
			<CopyBlock value={token.install_windows} label="Copy the Windows command" />
		</div>
		<div class="grid gap-1.5">
			<p class="text-sm font-semibold text-ink">Token <span class="font-normal text-ink-2">— {token.name}</span></p>
			<CopyBlock value={token.secret} label="Copy the token" secret />
		</div>

		<p class="text-sm leading-relaxed text-ink-2">
			The agent registers itself as a device within a minute. You can close this page.
		</p>

		<div class="flex flex-wrap items-center gap-2">
			<Button variant="secondary" href="/targets">See devices</Button>
			<Button variant="ghost" onclick={reset}>Create another token</Button>
		</div>
	</div>
{:else}
	<form onsubmit={create} class="grid gap-5">
		<p class="text-sm leading-relaxed text-ink-2">
			A server with the agent has no address to enter: the agent installed on the machine
			introduces itself to this server. Create a token, run the command it gives you, done.
		</p>

		<Field
			label="Token name"
			for="agent-token-name"
			required
			help="One token can enrol several machines: name it after a group or a machine."
		>
			<input id="agent-token-name" class="input" type="text" autocomplete="off" bind:value={name} />
		</Field>

		{#if error}
			<ErrorNotice {error} title="Could not create the token" />
		{/if}

		<div class="flex flex-wrap items-center gap-2">
			<ClickSpark>
				<Button type="submit" variant="primary" loading={creating} disabled={!name.trim()}>
					Create the install command
				</Button>
			</ClickSpark>
			<Button variant="ghost" href={cancelHref}>Cancel</Button>
		</div>
	</form>
{/if}
