<script lang="ts">
	/**
	 * What the backup server says about itself beyond its load: the units that
	 * have to run, the Proxmox package versions (installed, available, and the
	 * one actually running), the certificate the web interface serves, and the
	 * traffic-control rules with what they are carrying right now.
	 *
	 * Each block disappears when the probe has nothing for it — an option left
	 * off, a privilege the token does not have, or a server with no such thing.
	 * An absent block means "not read", never "all clear".
	 */
	import type { PbsCertificate, PbsPackage, PbsService, PbsTrafficRule } from '$lib/api';
	import { Plate, type Tone } from '$lib/ui';
	import { formatAgo, formatBytes, formatUnix } from './format';

	interface Props {
		services: PbsService[];
		packages: PbsPackage[];
		certificates: PbsCertificate[];
		traffic: PbsTrafficRule[];
	}

	let { services, packages, certificates, traffic }: Props = $props();

	/** The units a backup server cannot do without; the rest are informational. */
	const REQUIRED = ['proxmox-backup', 'proxmox-backup-proxy', 'proxmox-backup-banner'];

	/** Stopped required units first, then stopped ones, then the rest by name. */
	const sortedServices = $derived(
		[...services].sort((a, b) => {
			const rank = (s: PbsService) =>
				!s.running && REQUIRED.includes(s.service) ? 0 : s.running ? 2 : 1;
			return rank(a) - rank(b) || a.service.localeCompare(b.service);
		})
	);

	const stoppedRequired = $derived(
		services.filter((s) => !s.running && REQUIRED.includes(s.service)).length
	);

	function servicePlate(service: PbsService): { tone: Tone; label: string } {
		if (service.running) return { tone: 'signal', label: 'Running' };
		if (REQUIRED.includes(service.service)) return { tone: 'warning', label: 'Stopped' };
		if (service.enabled === false) return { tone: 'ghost', label: 'Stopped, disabled' };
		return { tone: 'advisory', label: 'Stopped' };
	}

	function packagePlate(entry: PbsPackage): { tone: Tone; label: string } {
		if (entry.restart_pending) return { tone: 'advisory', label: 'Restart pending' };
		if (entry.upgradable) return { tone: 'info', label: 'Update available' };
		return { tone: 'signal', label: 'Up to date' };
	}

	function certificatePlate(certificate: PbsCertificate): { tone: Tone; label: string } {
		if (certificate.not_after === null) return { tone: 'ghost', label: 'No expiry date' };
		const left = certificate.not_after - Date.now() / 1000;
		if (left <= 0) return { tone: 'warning', label: 'Expired' };
		if (left < 21 * 86_400) return { tone: 'warning', label: `Expires ${formatAgo(certificate.not_after)}` };
		if (left < 60 * 86_400) return { tone: 'advisory', label: `Expires ${formatAgo(certificate.not_after)}` };
		return { tone: 'signal', label: `Valid until ${formatUnix(certificate.not_after)}` };
	}

	/** The `CN=` of a multi-line X.509 name, or the whole thing if there is none. */
	function commonName(dn: string | null): string {
		if (!dn) return '—';
		const cn = dn
			.split(/[\n,]/)
			.map((part) => part.trim())
			.find((part) => part.startsWith('CN='));
		return cn ? cn.slice(3) : dn.trim();
	}

	/** "100 MB/s", or "no limit" when the rule does not cap that direction. */
	function rate(bytes: number | null): string {
		return bytes === null ? 'no limit' : `${formatBytes(bytes)}/s`;
	}

	const hasAnything = $derived(
		services.length > 0 || packages.length > 0 || certificates.length > 0 || traffic.length > 0
	);
</script>

{#if !hasAnything}
	<p class="px-5 py-4 text-sm text-ink-2">
		Nothing read about the server itself yet. The units, package versions and traffic rules need
		Sys.Audit on <span class="font-mono">/system</span>; the certificate needs Sys.Modify, so that
		one is off unless you turn it on in the device options.
	</p>
{:else}
	<div class="divide-y divide-line">
		{#if services.length > 0}
			<section>
				<div class="flex flex-wrap items-center gap-2 px-5 pt-3">
					<p class="text-[0.75rem] font-semibold tracking-wide text-ink-3 uppercase">Services</p>
					{#if stoppedRequired > 0}
						<Plate tone="warning" label={`${stoppedRequired} stopped`} />
					{:else}
						<Plate tone="signal" label="All required services running" bare />
					{/if}
				</div>
				<ul class="mt-1 divide-y divide-line">
					{#each sortedServices as service (service.service)}
						{@const plate = servicePlate(service)}
						<li class="flex flex-wrap items-center gap-x-3 gap-y-1 px-5 py-2">
							<span class="font-mono text-[0.8125rem] font-semibold text-ink">{service.service}</span>
							<Plate tone={plate.tone} label={plate.label} />
							{#if service.description}
								<span class="min-w-0 truncate text-[0.8125rem] text-ink-2">{service.description}</span>
							{/if}
							{#if service.enabled === false}
								<span class="text-[0.75rem] text-ink-3">not started at boot</span>
							{/if}
						</li>
					{/each}
				</ul>
			</section>
		{/if}

		{#if packages.length > 0}
			<section>
				<p class="px-5 pt-3 text-[0.75rem] font-semibold tracking-wide text-ink-3 uppercase">
					Versions
				</p>
				<ul class="mt-1 divide-y divide-line">
					{#each packages as entry (entry.package)}
						{@const plate = packagePlate(entry)}
						<li class="flex flex-wrap items-center gap-x-3 gap-y-1 px-5 py-2">
							<span class="font-mono text-[0.8125rem] font-semibold text-ink">{entry.package}</span>
							<Plate tone={plate.tone} label={plate.label} />
							<span class="tnum flex flex-wrap gap-x-3 text-[0.8125rem] text-ink-2">
								{#if entry.installed}<span>installed {entry.installed}</span>{/if}
								{#if entry.upgradable && entry.available}<span>available {entry.available}</span>{/if}
								{#if entry.running}<span>running {entry.running}</span>{/if}
							</span>
						</li>
					{/each}
				</ul>
			</section>
		{/if}

		{#if certificates.length > 0}
			<section>
				<p class="px-5 pt-3 text-[0.75rem] font-semibold tracking-wide text-ink-3 uppercase">
					Certificates
				</p>
				<ul class="mt-1 divide-y divide-line">
					{#each certificates as certificate (certificate.filename)}
						{@const plate = certificatePlate(certificate)}
						<li class="px-5 py-2">
							<div class="flex flex-wrap items-center gap-x-3 gap-y-1">
								<span class="font-mono text-[0.8125rem] font-semibold text-ink">
									{certificate.filename}
								</span>
								<Plate tone={plate.tone} label={plate.label} />
								<span class="min-w-0 truncate text-[0.8125rem] text-ink-2">
									{commonName(certificate.subject)} · issued by {commonName(certificate.issuer)}
								</span>
							</div>
							{#if certificate.san.length > 0}
								<p class="mt-0.5 text-[0.75rem] break-all text-ink-3">{certificate.san.join(', ')}</p>
							{/if}
						</li>
					{/each}
				</ul>
			</section>
		{/if}

		{#if traffic.length > 0}
			<section>
				<p class="px-5 pt-3 text-[0.75rem] font-semibold tracking-wide text-ink-3 uppercase">
					Traffic limits
				</p>
				<ul class="mt-1 divide-y divide-line">
					{#each traffic as rule (rule.name)}
						<li class="px-5 py-2">
							<div class="flex flex-wrap items-center gap-x-3 gap-y-1">
								<span class="font-semibold text-ink">{rule.name}</span>
								<span class="tnum text-[0.8125rem] text-ink-2">
									in {rate(rule.limit_in_bytes)} · out {rate(rule.limit_out_bytes)}
								</span>
								{#if (rule.rate_in_bytes ?? 0) > 0 || (rule.rate_out_bytes ?? 0) > 0}
									<Plate
										tone="info"
										label={`now ${formatBytes(rule.rate_in_bytes ?? 0)}/s in, ${formatBytes(rule.rate_out_bytes ?? 0)}/s out`}
										bare
									/>
								{:else}
									<span class="text-[0.75rem] text-ink-3">idle</span>
								{/if}
							</div>
							<p class="mt-0.5 text-[0.75rem] text-ink-3">
								{rule.comment ? `${rule.comment} · ` : ''}{rule.networks.join(', ') || 'every network'}{rule.timeframe.length >
								0
									? ` · ${rule.timeframe.join(', ')}`
									: ''}
							</p>
						</li>
					{/each}
				</ul>
			</section>
		{/if}
	</div>
{/if}
