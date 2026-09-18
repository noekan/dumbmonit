# Settings

![Settings: account, users, single sign-on, agents, assistant, appearance, about](../assets/screenshots/settings-light.png){ loading=lazy }

Settings holds what is administrative: your account, who can sign in and
how, the tokens that let agents and assistants in, the theme, and the
server's health. One page, one section per topic, with a rail of anchors on a
wide screen and a strip of chips on a phone. Each section is linkable
(`/settings#agents`).

Two things used to live here and moved out, because they are not
administration:

| Looking for | Now under | Old link |
|---|---|---|
| Notification channels, quiet hours, notification policy | **Alerts → Notifications** ([Alerts page](alerts.md#notifications)) | `/settings#notifications`, `/settings#notifications-policy` |
| Status pages and announcements | **Status** ([Status pages](status-pages.md)) | `/settings#status` |

The old links still work: they forward to the new place.

## Account & security

The instance is protected by one password, set on first start. Here you can
change it: current password, new password (at least 12 characters; a whole
phrase is safer than a complicated word), confirmation. Changing it signs out
every other session. Accounts that sign in through the identity provider have
no password here. **Sign out** ends this session.

Login is rate-limited per client address and per account: after five failed
attempts, each further attempt waits longer (30 s, doubling, up to 5 minutes).
Behind a reverse proxy, set `DUMBMONIT_TRUSTED_PROXIES` so the limiter sees
the real client address rather than the proxy's (see
[Configuration](../reference/configuration.md)). Sessions last 30 days. A
forgotten password is reset with `DUMBMONIT_RESET_PASSWORD=1`: see the
[FAQ](../faq.md#i-lost-the-password).

### Two-factor authentication

Accounts that sign in with a password can add a second factor: a six-digit
time-based code (TOTP) from an authenticator app — Aegis, FreeOTP, Google
Authenticator, 1Password, Bitwarden… **Set up** asks for your password, shows
a QR code to scan (or the key to type), and enables the second factor once you
enter a first code from the app. Eight **recovery codes** are then shown once:
save them, each one signs you in a single time if the phone is lost. The
sign-in screen asks for the code (or a recovery code) after the password;
five wrong codes cancel the attempt and you start over from the password.

**Disable** asks for the password again. Recovery codes cannot be regenerated
on their own: disable and set up again to get a fresh set. An admin can reset
another user's second factor from **Users** (*Reset 2FA*) when both the phone
and the codes are gone — that signs the user out everywhere and leaves the
password alone. The secret is stored encrypted with the instance secret;
codes are hashed. Single sign-on accounts get their second factor from the
identity provider, not here.

Admins also find the **security log** in this section: sign-ins and failures,
password and two-factor changes, token and account changes, with the client
address. It keeps the last 5 000 entries.

## Users

Admins only. The accounts that can sign in, and their role: **admin**
(everything) or **viewer** (read only — every page opens, every control that
would change something is hidden). Create an account with a username, an
optional display name, a role and a password; disable or delete one; reset
its password. The server keeps at least one active admin, and the controls
reflect that rule rather than reporting it as an error.

## Single sign-on

Admins only. Sign-in through an OpenID Connect provider (Authelia, Authentik,
Keycloak, Pocket ID, Google Workspace…). Enter the issuer URL, the client id
and secret, and register the callback URL shown in the form with your
provider. **Test discovery** reads the provider's configuration without
signing anyone in. Settings saved here take precedence over the
`DUMBMONIT_OIDC_*` environment variables.

**Roles.** *Admin groups* lists the groups whose members become admins;
everyone else is a viewer. Roles are re-evaluated at each sign-in, except
that the last active admin is never demoted.

**Which account a sign-in lands on.** The identity is remembered by its
provider subject after the first sign-in. On a first sign-in:

- An existing account is linked only when the provider asserts
  `email_verified: true` and that email is the account's username. A
  `preferred_username` or an unverified email never links: on most providers
  the user chooses those, so `admin` from the provider must not become *your*
  `admin`.
- A local **admin that has a password is never linked automatically**, even
  on a verified email: the sign-in creates a distinct account instead. Keep
  managing that admin with its password, or create a separate SSO admin
  through the admin groups.
- Otherwise, with *Create accounts on first sign-in* on, a new account is
  created (username from the provider, suffixed `-1`, `-2`… when taken). With
  it off, only the verified-email link above can sign in.

## Agents

Enrollment tokens for the [Linux and Windows agent](../devices/agent.md).
Create one with a name ("File server", "Home fleet"): the token is shown once,
with the Linux and Windows install commands ready to copy. The list shows each
token's prefix, creation date, last use and whether it was revoked. **Revoke**
stops every agent using that token at its next push.

One token can enrol several machines. Revoking it does not delete the devices.

## Connect an assistant

API tokens for MCP clients, with the connection snippets ready to paste. A
**read** token can only look; a **read and write** token can also silence a
device, run a probe, or switch a device or rule on and off. See
[Connect an assistant](assistant.md).

## Appearance

Three choices: **System** (follows your device and switches with it),
**Day** (the "chart paper" theme) and **Night** (the "radar" theme). The
choice is stored in the browser. The header's toggle and the command
palette's *Toggle theme* switch between day and night.

## About

Version of the server and the health of its two dependencies, the SQLite
database and VictoriaMetrics, as reported by `GET /api/health`.
