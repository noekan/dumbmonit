/**
 * Agent distribution: the SHA-256 checksums the server publishes next to each
 * agent binary (`/download/<name>.sha256`). Outside `/api` on purpose: these are
 * the URLs the install scripts read, and they need no session.
 */

/** File names served under `/download/`, mirroring `AGENT_FILES` on the server. */
export const AGENT_FILES = {
	linux_x86_64: 'dumbmonit-agent-linux-x86_64',
	linux_aarch64: 'dumbmonit-agent-linux-aarch64',
	windows_x86_64: 'dumbmonit-agent-windows-x86_64.exe'
} as const;

export interface AgentChecksum {
	file: string;
	/** Lower-case hexadecimal SHA-256, or `null` when this image ships no binary. */
	sha256: string | null;
}

/**
 * Fetches the checksum of one agent binary. Never throws: an image without the
 * binaries (404) or an unreachable server yields `sha256: null`, and the
 * enrolment screen simply shows nothing for that file.
 */
export async function fetchAgentChecksum(file: string): Promise<AgentChecksum> {
	try {
		const response = await fetch(`/download/${file}.sha256`, { cache: 'no-cache' });
		if (!response.ok) return { file, sha256: null };
		const text = await response.text();
		const hex = text.trim().split(/\s+/)[0]?.toLowerCase() ?? '';
		return { file, sha256: /^[0-9a-f]{64}$/.test(hex) ? hex : null };
	} catch {
		return { file, sha256: null };
	}
}

/** Checksums of every agent binary the server knows, in a stable order. */
export function fetchAgentChecksums(): Promise<AgentChecksum[]> {
	return Promise.all(Object.values(AGENT_FILES).map(fetchAgentChecksum));
}
