# Backup and restore

Two different things, for two different accidents.

| | Encrypted bundle | Scheduled local backup |
|---|---|---|
| What it holds | The configuration: devices and their credentials, rules, channels, status pages, accounts, tokens. | The whole database, as one file, plus `secret.key`. |
| Where it goes | Wherever you put it — it is encrypted with a passphrase you choose. | `/data/backups/`, inside the same volume. |
| What it saves you from | A lost server, a move to new hardware, a fresh install. | The change you regret ten minutes later, an upgrade that went wrong. |
| What it does not hold | Metrics history, alert state and history, the audit log, sessions. | Metrics history (`vm/`). |

Neither replaces an archive of the whole volume if you want the graphs back
too; that is at the bottom of this page.

!!! danger "`/data/secret.key` is what decrypts your device credentials"
    SNMP communities, Proxmox tokens, SMTP passwords and heartbeat URLs are
    stored encrypted with a key derived from the instance secret. On first start
    DumbMonit writes it to `/data/secret.key` (or reads it from
    `DUMBMONIT_SECRET` if you set one).

    **A copy of the database without that file restores an instance that cannot
    talk to anything.** The server notices and refuses to start with an explicit
    message rather than failing on every probe — but the credentials are gone,
    and every device has to be entered again.

    The encrypted bundle is the one exception: its secrets are re-encrypted with
    your passphrase, so it restores onto a fresh instance that has its own,
    brand-new secret.

## The encrypted bundle

**Settings → Backup → Download the bundle.** Choose a passphrase of at least 16
characters — three words is enough — and confirm it. The file that comes down is
plain JSON with an encrypted body:

```json
{
  "format": "dumbmonit-backup",
  "version": 1,
  "created_at": "2026-09-22 19:04:11",
  "source_version": "0.1.0-alpha.3",
  "summary": { "targets": 12, "channels": 2, "rules": 41 },
  "kdf": { "algorithm": "argon2id", "salt": "…", "m_cost": 32768, "t_cost": 3, "p_cost": 1 },
  "cipher": "aes-256-gcm",
  "payload": "…"
}
```

The header stays readable so you can tell what a file is before typing anything.
Everything else — addresses included — is inside `payload`, encrypted with
AES-256-GCM under a key derived from your passphrase by Argon2id. There is **no
recovery**: a bundle whose passphrase is lost is lost.

!!! warning "The bundle holds every credential of this instance"
    Anyone with the file and the passphrase can read the password of every
    device you monitor. Keep it where you keep passwords.

### What is in it

| Section | Contents |
|---|---|
| `targets` | Devices: name, kind, address, interval, options, parent, relay agent, and the credential. |
| `rules` | Alert rules, including built-in ones you have retuned or disabled. |
| `rule_overrides` | Per-device thresholds and exclusions. |
| `channels` | Notification channels, their settings, their per-channel policy and their secrets. |
| `notify_policy` | The global notification policy (grouping, hourly cap, flapping, public URL). |
| `silences` | Maintenance windows. |
| `status_pages` | Public status pages and the services each one lists. |
| `incidents` | Incidents and maintenance announcements, with their updates. |
| `users` | Accounts: username, display name, role, SSO link, enabled or not. |
| `agent_tokens` | Agent enrolment tokens, as hashes. Installed agents keep pushing after a restore. |
| `api_tokens` | API tokens, as hashes. Existing tokens keep working. |
| `push_monitors` | Heartbeat tokens, so the URLs your cron jobs already call stay the same. |

Devices are matched on **kind + address**, which is also the unique key of the
table: importing the same bundle twice never creates a second copy of anything.

### Account passwords

By default accounts are exported **without** their password hash, TOTP secret or
recovery codes. Restored accounts then exist with the right name and role, but
nobody can sign in with them until an administrator sets a password — or until
they sign in through SSO, which needs nothing from the bundle.

Turn on **Include account passwords and 2FA secrets** when you are moving an
instance rather than copying its configuration. An Argon2 hash in a file that
travels can be attacked offline for as long as the attacker likes; the default
is the safe one.

### What is not in it

Metrics history (that lives in VictoriaMetrics, not in the database), alert
state and history, anomaly baselines, the audit log, open sessions, the
registration and last-seen of agents, and the probe caches of the Proxmox and
Synology panels. They are observations, not settings: they come back on their
own.

And **not the instance secret**. That is the point: the bundle carries its
secrets re-encrypted under your passphrase precisely so that it can be restored
somewhere that has never seen `secret.key`.

### Restoring onto a fresh instance

1. Start the new container and create the first administrator account at
   `/setup`. That account is yours; the restore will not touch it.
2. **Settings → Backup → Restore a bundle.** Pick the file, type the passphrase,
   and press **Check this backup**.
3. Read the report. It says, section by section, what would be **created**, what
   would be **updated** because it differs from what is already here, and what is
   already identical — plus a note for anything it could not place, such as a
   rule that pointed at a device the bundle does not contain.
4. Press **Restore for real**.

Three rules the restore never breaks:

- **Nothing is ever deleted.** A restore makes what the bundle describes exist;
  anything the instance has in addition stays.
- **Nothing is ever duplicated.** Every item is matched on its natural key —
  kind + address for a device, `uid` for a rule, name for a channel, slug for a
  status page. Restoring the same bundle twice reports everything as already
  identical and writes nothing.
- **Existing accounts are left alone.** An account whose username is already
  here is skipped, never overwritten: a file must not be able to change the role
  or the password of the administrator running the restore.

A dry run is not a simulation of the restore, it *is* the restore, run inside a
transaction that is rolled back at the end. The report cannot be wrong about
what applying would do.

A bundle written by a newer DumbMonit is refused with a sentence that says so —
upgrade first, then restore. A bundle that has been modified, or whose
passphrase is wrong, is refused too: AES-GCM cannot tell the two apart, and
neither can the message.

### From the command line

Both routes need an administrator **session**, not an API token: a token lives
in a script, and a script has no business exporting every credential of the
instance.

```bash
# Sign in
curl -s -c jar -X POST http://localhost:8080/api/auth/login \
  -H 'content-type: application/json' -H 'X-Requested-With: DumbMonit' \
  -d '{"username":"admin","password":"…"}'

# Export
curl -s -b jar -X POST http://localhost:8080/api/backup \
  -H 'content-type: application/json' -H 'X-Requested-With: DumbMonit' \
  -d '{"passphrase":"three words are enough"}' -o backup.json

# Dry run, then apply
jq -n --slurpfile b backup.json \
  '{bundle: $b[0], passphrase: "three words are enough", apply: false}' > restore.json
curl -s -b jar -X POST http://localhost:8080/api/backup/restore \
  -H 'content-type: application/json' -H 'X-Requested-With: DumbMonit' \
  -d @restore.json | jq
```

## Scheduled local backups

DumbMonit writes a copy of its own database on a schedule, by default **once a
day, keeping the last seven**, into `/data/backups/`:

```
/data/backups/dumbmonit-20260922-030000.db
/data/backups/dumbmonit-20260922-030000.key
```

The `.db` file is produced with SQLite's `VACUUM INTO`: an online, consistent
copy written while the server keeps running, not a file copy that could catch
the database mid-write. `sqlite3` opens it as it is, with no journal to replay.
The `.key` beside it is the copy of `secret.key` — together, the pair is enough
to rebuild the instance.

They live in the same volume as the database, so they undo a mistake, not a lost
disk. Copy them off the machine if you want more than that.

### Settings

| Variable | Default | Role |
|---|---|---|
| `DUMBMONIT_BACKUP_ENABLED` | `1` | `0`, `false`, `no` or `off` to stop writing scheduled backups. |
| `DUMBMONIT_BACKUP_DIR` | `<data dir>/backups` | Where they are written. Point it at a second volume to survive the first one. |
| `DUMBMONIT_BACKUP_INTERVAL_HOURS` | `24` | Hours between two backups. The first one happens one interval after startup, not at startup. |
| `DUMBMONIT_BACKUP_KEEP` | `7` | How many are kept. The oldest are removed, with their `.key`. |

Nothing else in the directory is ever touched: only files named
`dumbmonit-<timestamp>.db` and their `.key` are rotated.

When the instance secret comes from `DUMBMONIT_SECRET` there is no file to copy,
and none is written — keep that value in your password manager instead. The
Backup section says which of the two cases you are in.

### Knowing that it ran

Each run publishes three series:

| Metric | Meaning |
|---|---|
| `dumbmonit_instance_backup_last_success_seconds` | Unix timestamp of the last **successful** backup. A failed attempt never refreshes it. |
| `dumbmonit_instance_backup_last_status` | `1` if the last attempt succeeded, `0` if it failed. |
| `dumbmonit_instance_backup_size_bytes` | Size of the last file written. |

The built-in rule **DumbMonit backup did not run**
(`instance_backup_missing`) fires when the last success is more than two days
old — one missed night can be a restart, two is a problem. The series only
exists once a first backup has been written, so an instance with the schedule
turned off is never nagged about it.

**Settings → Backup** shows the same thing without MetricsQL: the last run and
whether it succeeded, the files kept and their size, and **Back up now** to
write one immediately.

### Restoring one

Stop the stack, put the file back under the database's name, and restore the key
next to it:

```bash
docker compose stop
docker run --rm -v dumbmonit-data:/data alpine sh -c '
  cp /data/backups/dumbmonit-20260922-030000.db /data/dumbmonit.db &&
  cp /data/backups/dumbmonit-20260922-030000.key /data/secret.key &&
  rm -f /data/dumbmonit.db-wal /data/dumbmonit.db-shm'
docker compose start
```

Removing the `-wal` and `-shm` files matters: they belong to the database you
just replaced, and SQLite would try to apply them to the new one.

To read a backup without restoring it, copy it out and open it with `sqlite3`:

```bash
docker run --rm -v dumbmonit-data:/data -v "$PWD:/out" alpine \
  cp /data/backups/dumbmonit-20260922-030000.db /out/
sqlite3 dumbmonit-20260922-030000.db 'SELECT name, kind, address FROM targets;'
```

Credentials in that file stay unreadable without the matching `secret.key`.

## Archiving the whole volume

The only way to keep the graphs as well. One named volume, `dumbmonit-data`,
holds everything: the database, the secret, the local backups and the
VictoriaMetrics data.

```bash
docker compose stop
docker run --rm -v dumbmonit-data:/data -v "$PWD:/backup" alpine \
  tar czf /backup/dumbmonit-data.tgz -C /data .
docker compose start
```

To restore, create the volume, extract the archive into it the same way, then
`docker compose up -d`.

If space matters, `--exclude=./vm` keeps the archive small: the database and the
secret are the setup, `vm/` is only the history.

## What to do before an upgrade

1. **Settings → Backup → Back up now** — thirty seconds, and it includes the
   secret.
2. Download a bundle as well if the upgrade is a big one, or if you are moving
   to another machine.
3. `docker compose pull && docker compose up -d`. Migrations run at startup.

If it goes wrong, [restore the local backup](#restoring-one) and start the
previous image tag.
