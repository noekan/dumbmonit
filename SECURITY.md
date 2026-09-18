# Security policy

DumbMonit runs inside your network, holds the credentials of every device it
watches, and receives measurements from agents installed on your machines. We
take reports about it seriously and appreciate the time it takes to write one.

## Supported versions

DumbMonit has no stable release line yet. Security fixes land on `main` and ship
in the next tagged release and in the `edge` image.

| Version | Supported |
|---|---|
| `main` / `edge` image | Yes |
| Latest tagged release (`latest` image) | Yes |
| Older tags | No — upgrade |

## Reporting a vulnerability

**Please do not open a public issue for a security problem.**

Report it privately through GitHub's vulnerability reporting:
**https://github.com/noekan/dumbmonit/security/advisories/new**

Include what you can of: the affected component (see the scope below), steps to
reproduce or a proof of concept, the impact as you understand it, and the
version or commit you tested. A draft advisory lets us discuss and fix the
problem together before it is public; you will be credited in the advisory
unless you prefer not to be.

## What to expect

- An acknowledgement within **7 days**.
- A first assessment (confirmed / not a vulnerability / need more information)
  within **14 days**.
- A fix on `main` as soon as it is ready, a tagged release, and a published
  advisory. For confirmed issues we aim for a fix within 90 days and usually
  much sooner: this is a small codebase.

This is a volunteer-maintained project; there is no bug bounty.

## Scope

In scope, roughly in order of how much we care:

- **Authentication and sessions** — the instance password, `/setup`, the login
  rate limit, the session cookie, `DUMBMONIT_RESET_PASSWORD`, the OpenID
  Connect flow (state, nonce, PKCE, which local account an identity is linked
  to), the post-login redirect, and any way to reach `/api/*` without a
  session — including on a fresh instance, where only `/api/auth/status`,
  `/api/auth/setup`, `/api/auth/login` and `/api/health` must answer.
- **Secrets at rest** — the AES-256-GCM encryption of SNMP communities, API
  tokens and passwords, the derivation of the key from `/data/secret.key` or
  `DUMBMONIT_SECRET`, and any path by which a secret is returned by the API
  (`credential` / `secrets` must never round-trip).
- **Agent ingest** — the `dmon_…` tokens (and the `ezym_…` ones still accepted
  from before the rename), `/api/ingest`, `install.sh` /
  `install.ps1`, the binaries served under `/download/…`, and anything a
  malicious agent or a spoofed server could do to the other side.
- **Outbound requests** — SSRF through device addresses, notification webhooks
  or the VictoriaMetrics proxy; TLS verification of integrations.
- **Injection** — SQL, MetricsQL, notification templates, the SNMP and HTTP
  parsers.
- **The Docker image and Compose files** — defaults that expose more than they
  should.

Out of scope: vulnerabilities in the devices you monitor, in VictoriaMetrics
itself (report those upstream), findings that require an attacker who already
controls the host or the `/data` volume, and reports from automated scanners
without a demonstrated impact.

## Hardening notes for operators

- Put DumbMonit behind a reverse proxy with TLS if it is reachable from outside
  the network it monitors; the built-in server speaks plain HTTP.
- Create the first admin right after the first start (or after
  `DUMBMONIT_RESET_PASSWORD=1`): until then the API refuses everything but
  the setup routes, but whoever reaches the port first can create the account.
- With single sign-on, an identity is linked to an existing local account only
  on a provider-verified email matching that account's username, and never to
  a password-holding admin: if you want an SSO admin, put its group in *Admin
  groups* rather than reusing the local admin's name.
- Turn on two-factor authentication (Settings → Account & security) on every
  password account, admins first. The TOTP secret is encrypted with the
  instance secret; recovery codes are hashed and single-use; an admin can
  reset a user's second factor from *Users*, which also signs that user out.
- Login attempts are counted per client address and per account. Behind a
  reverse proxy, set `DUMBMONIT_TRUSTED_PROXIES` to the proxy's address so
  the counters and the security log see the real client; `X-Forwarded-For` is
  ignored from anywhere else.
- Every state-changing request authenticated by the session cookie must carry
  `X-Requested-With: DumbMonit` (the web UI does), or same-origin fetch
  metadata; a cross-site request is refused with `403`. Scripts that reuse a
  browser cookie need that header — or better, an API token on `/api/mcp`.
- Back up `/data/secret.key` with the database, and keep it out of your
  screenshots.
- Prefer SNMP v3 on shared networks: a v2c community travels in clear text.
- Give agent tokens one per machine so a leaked token can be revoked alone.
- The container runs as an unprivileged user with no capability, a read-only
  root file system and `no-new-privileges` — keep those lines when you adapt
  the Compose file. ICMP ping goes through the `ping_group_range` sysctl, not
  `NET_RAW`.
- The install scripts verify the downloaded agent binary against the SHA-256
  the server publishes at `/download/<file>.sha256`; the checksums are also
  shown next to the install command so you can compare them by hand when the
  server is reached over plain HTTP.
- The embedded VictoriaMetrics has no authentication. It listens on the
  container's loopback by default; leave `DUMBMONIT_VM_LISTEN` alone unless
  you publish the port on purpose.
