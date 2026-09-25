# Services: HTTP, TCP, DNS, ping, TLS, SMTP, SQL, MQTT, WebSocket

Ten monitors watch **services** rather than equipment, Uptime Kuma style: a web
page, a port, a DNS name, a host, a certificate, a mail relay, a database, an
MQTT broker, a WebSocket endpoint. Each service gets a history bar, a response
time and an availability percentage on its device page.

The last five go further than opening a connection: they begin a real session in
the service's own protocol. That is the difference between "port 5432 is open"
and "the database still accepts my account and answers a query".

## How service state works

A service can answer and still be down: a `500`, a missing keyword, an expired
certificate. So these monitors write a point on every check, `probe_success`
being 1 or 0, instead of going silent like a hardware device. That is what
makes an availability percentage possible: `avg_over_time(dumbmonit_probe_success[30d])`.

| Situation | What happens |
|---|---|
| Invalid option or address, ICMP sockets not allowed (see Ping) | Configuration error: nothing is written, the device page shows the error, no "down" alert. |
| Connection refused, timeout, TLS refused, `500`, keyword absent, total packet loss, password refused, query failed | `probe_success = 0`: the outage is recorded, dated and counted. |
| All good | `probe_success = 1`. |

Every monitor has its own timeout (`timeout_seconds`, 5 s by default, 60 s at
most), shorter than the server's `DUMBMONIT_PROBE_TIMEOUT_SECS`: interrupted by
the scheduler, it could not write its zero.

Built-in rules that apply to all ten: Service down (3 minutes), Service
flapping (more than six state changes in thirty minutes), Slow service (more
than 3 s for ten minutes). For every check that reads a certificate — `http`,
`tls`, `smtp`, `mqtt`, `websocket`: Certificate expiring soon (14 days) and
Certificate expired.

A refused password is never an outage of the service: it is recorded as
`reason="auth"`, so the alert says what to fix.

### Metrics

All gauges, all prefixed `dumbmonit_`, all labelled
`probe="http|tcp|dns|ping|tls|smtp|postgres|mysql|mqtt|websocket"` in addition
to `target`, `host` and `tag_*`.

| Metric | Monitors | Meaning |
|---|---|---|
| `probe_success` | all | 1 if the service answers correctly, 0 otherwise. |
| `probe_duration_seconds` | all | Total duration of the check. |
| `probe_failure_info` | all | Presence (1) with a `reason` label: `dns`, `connect`, `timeout`, `tls`, `cert_expired`, `status`, `keyword`, `json`, `body`, `packet_loss`, `record`, `auth`, `protocol`, `query`, `payload`. |
| `probe_http_status_code` | http, websocket | Status code obtained. |
| `probe_http_first_byte_seconds` | http | Time to the response headers. |
| `probe_http_content_bytes` | http | Body size. |
| `probe_connect_seconds` | all but dns and ping | Connection establishment (login included for databases). |
| `probe_tls_handshake_seconds` | tls, http, smtp, mqtt, websocket | TLS negotiation alone. |
| `probe_tls_version_info` | tls, http, smtp, mqtt, websocket | Presence; `version` label. |
| `probe_ssl_cert_expiry_days` | tls, http, smtp, mqtt, websocket | Days before expiry, negative if expired. |
| `probe_ssl_cert_valid` | tls, http, smtp, mqtt, websocket | 1 if the chain leads to a known authority. |
| `probe_ssl_cert_issuer_info` | tls, http, smtp, mqtt, websocket | Presence; `issuer` label. |
| `probe_dns_lookup_seconds` | dns | Resolution time. |
| `probe_dns_answer_records` | dns | Records of the requested type. |
| `probe_icmp_rtt_seconds`, `_min_seconds`, `_max_seconds` | ping | Average, shortest and longest round trip. |
| `probe_icmp_packet_loss_ratio` | ping | Loss between 0 and 1. |
| `probe_icmp_packets_sent`, `probe_icmp_packets_received` | ping | Echoes sent and received. |
| `probe_smtp_greeting_seconds` | smtp | Time to the `220` banner. |
| `probe_smtp_ehlo_seconds` | smtp | Round trip of the `EHLO` command. |
| `probe_smtp_capabilities` | smtp | Extensions advertised. |
| `probe_smtp_authenticated` | smtp | 1 when `AUTH` succeeded. |
| `probe_sql_query_seconds` | postgres, mysql | Query execution alone. |
| `probe_sql_rows` | postgres, mysql | Rows returned. |
| `probe_sql_value` | postgres, mysql | First column of the first row, when it is a number. |
| `probe_mqtt_connack_seconds` | mqtt | Time to the `CONNACK`. |
| `probe_mqtt_suback_seconds` | mqtt | Time to the `SUBACK`. |
| `probe_mqtt_message_bytes` | mqtt | Size of the retained message. |
| `probe_mqtt_message_value` | mqtt | Its content, when it is a number. |
| `probe_ws_handshake_seconds` | websocket | Opening handshake. |
| `probe_ws_message_bytes` | websocket | Size of the frame received. |

## HTTP

**Website or web API (HTTP)** — checks that a page or an API answers, with the
right status code, the right content, and a valid certificate. Examples: a
website, an application health page, a REST API, a self-hosted service
interface.

### Setup

1. In the address, paste the full URL of the page to monitor. Without
   "http://" or "https://", HTTPS is assumed.
2. Prefer a light page that needs no login, for example the application's
   health page ("/health", "/status"), rather than the home page.
3. If the page requires authentication, fill in the credential: a username and
   password give basic authentication, a token is sent as "Bearer".
4. By default, any status code between 200 and 299 is fine. To go further,
   require a keyword in the page or a specific value in a JSON response.
5. Over HTTPS, the certificate is read automatically: you will be warned before
   it expires.

!!! warning
    Never write a password or a token in the options: they are copied in clear
    text on every measurement. Use the credential field, which is encrypted.

Credentials: none, username / password (basic authentication) or API token
(`Authorization: Bearer`).

### Options

| Key | Label | Default | Help |
|---|---|---|---|
| `method` | HTTP method | `GET` | GET fits almost every case. HEAD avoids downloading the page when only the status code matters. Choices: GET, HEAD, POST, PUT, PATCH, DELETE, OPTIONS. |
| `accepted_status` | Accepted status codes | `200-299` | Codes or ranges considered normal, separated by commas. Any other code counts as down. Example: `200-299,301,404`. |
| `keyword` | Expected keyword | *(empty)* | Text that must appear in the page. If it is missing, the service is reported down even though the page answers. |
| `keyword_absent` | Keyword must be absent | `false` | Inverts the check: the presence of the keyword signals a failure. Useful for an error page that answers 200. |
| `keyword_case_sensitive` | Match keyword case | `false` | By default, upper and lower case are treated the same. |
| `json_path` | JSON path to check | *(empty)* | For a JSON response: path of the value to check. Fill it in together with the expected value, never one without the other. Example: `$.status`. |
| `json_expect` | Expected JSON value | *(empty)* | Value the JSON path above must have, for example "ok" or "true". |
| `headers` | Extra headers | *(empty)* | One or more "Name: value" headers, separated by "\|". Do not put secrets here: options are visible in the charts. |
| `body` | Request body | *(empty)* | Content sent with the request, for methods that expect one (POST, PUT…). |
| `follow_redirects` | Follow redirects | `true` | Disable to monitor the redirect itself, for example a 301 to HTTPS. |
| `max_redirects` | Maximum redirects followed | `10` | Between 1 and 20. No effect if redirects are not followed. |
| `insecure_tls` | Accept an unverifiable certificate | `false` | The check no longer fails on a self-signed certificate or one issued by a private authority. |
| `allow_private_targets` | Allow loopback and link-local targets | `false` | By default the check refuses addresses only the DumbMonit host itself can reach (127.0.0.1, ::1, 169.254.x.x). Enable this to monitor a service running on the DumbMonit host. Private LAN addresses (10.x, 192.168.x) are always allowed. |
| `check_certificate` | Read the certificate | `true` | Over HTTPS, also records the certificate expiry date so you can be warned before it expires. |
| `max_body_bytes` | Maximum bytes read | `524288` | Beyond this, the rest of the page is not downloaded. The keyword and JSON path are only searched in this part. |
| `user_agent` | Announced identity (User-Agent) | `DumbMonit/<version>` | Name the check gives to the server. Change it if the server filters robots. |
| `timeout_seconds` | Timeout (seconds) | `5` | Time after which the service is reported down if it has not answered. Between 1 and 60. |

## TCP

**Network port (TCP)** — checks that a port accepts connections: SSH, database,
file share, game server, network printer.

### Setup

1. In the address, write the host followed by the port, separated by a colon:
   "nas.home.lan:22" or "192.168.1.5:445".
2. For an IPv6 address, put it in brackets: "[fd00::1]:445".
3. You can also leave the address without a port and enter it in the "Port"
   option: one of the two is required.
4. Only the connection opening is tested: no data is sent to the service, so
   it has no effect on it.

!!! warning
    An open port does not prove the application behind it works. For a web
    service, prefer the "Website or web API" type, which reads the response.

### Options

| Key | Label | Default | Help |
|---|---|---|---|
| `port` | Port | *(empty)* | Port to open, if the address does not already give it as "host:port". One of the two is required. |
| `allow_private_targets` | Allow loopback and link-local targets | `false` | By default the check refuses addresses only the DumbMonit host itself can reach (127.0.0.1, ::1, 169.254.x.x). Enable this to monitor a service running on the DumbMonit host. Private LAN addresses (10.x, 192.168.x) are always allowed. |
| `timeout_seconds` | Timeout (seconds) | `5` | Time after which the service is reported down if it has not answered. Between 1 and 60. |

## DNS

**Domain name (DNS)** — checks that a name resolves, and that it points to the
right address. Examples: your domain name, an internal name served by your
Pi-hole or AdGuard, an MX record.

### Setup

1. In the address, write the name to resolve, without "http://":
   "www.example.com".
2. By default, the check asks the system resolver for an IPv4 address (A
   record). Pick another record type in the options if needed.
3. To monitor your own DNS server, enter its IP address in "DNS server to
   query": the check will fail if it stops answering.
4. To detect hijacking or a misconfiguration, list in "Expected values" the
   addresses the answer must contain.

!!! warning
    The DNS server is given by its IP address, not by a name: it would take a
    resolver to resolve the resolver.

### Options

| Key | Label | Default | Help |
|---|---|---|---|
| `record_type` | Record type | `A` | A for an IPv4 address, AAAA for IPv6, MX for mail, CNAME for an alias… Choices: A, AAAA, CNAME, MX, TXT, NS, SOA, SRV, PTR, CAA. |
| `resolver` | DNS server to query | *(empty)* | IP address of a resolver, port optional. Empty: the system resolver. Handy to check your own DNS server. Example: `1.1.1.1` or `10.0.0.1:5353`. |
| `expect` | Expected values | *(empty)* | Values that must all appear in the answer, separated by commas. Empty: only the resolution is checked. |
| `expect_mode` | Comparison | `contains` | `contains`: each expected value must appear somewhere in the answer. `exact`: the answer must hold those values and nothing else. |
| `forbid` | Forbidden values | *(empty)* | Values that must never appear in the answer, separated by commas. Checked before the expected ones. |
| `timeout_seconds` | Timeout (seconds) | `5` | Time after which the service is reported down if it has not answered. Between 1 and 60. |

### Asserting what a zone answers

`expect` alone catches the record that disappeared. It does not catch the record
that was *added*: a hijacked zone often keeps the legitimate `A` and puts a
second one beside it, and a client then reaches either one at random.

- `expect_mode = exact` refuses any value the expectation does not cover. Order
  does not matter — DNS does not keep one.
- `forbid` names values that must never come back: the address of a former host,
  an expired validation `TXT`, a mail server you decommissioned. It is checked
  first, so it wins over `expect`.

Both compare case-insensitively, ignore the trailing dot of a name and the
quotes around a `TXT`, and treat an expected value as found when it appears
inside a record — so an `MX` matches on `mail.example.com` without you having to
copy its priority.

## Ping

**Reachable host (ping)** — sends a few ICMP echoes and measures response time
and packet loss. Examples: home router or gateway, Wi-Fi access point, printer,
machine without agent or SNMP, remote host.

### Setup

1. In the address, write the host name or IP address: "192.168.1.1" or
   "router.home.lan".
2. On each poll, the check sends four echoes and measures the response time,
   along with the share of lost packets.
3. By default, only a total loss counts as down; partial loss stays visible in
   the charts. Lower "Tolerated loss" to be warned earlier.
4. If DumbMonit runs in Docker, keep the `sysctls:` lines of the shipped
   docker-compose.yml under the dumbmonit service: they allow the container's
   unprivileged user to open ICMP echo sockets. No capability is needed.

!!! warning
    Without that sysctl, the check cannot open an ICMP socket: it reports this
    as a configuration error, not as a host failure. Some devices also ignore
    pings on purpose: check that before drawing conclusions.

```yaml
services:
  dumbmonit:
    sysctls:
      net.ipv4.ping_group_range: "0 2147483647"
```

Outside Docker, `sysctl -w net.ipv4.ping_group_range="0 2147483647"` on the
host does the same, and a raw socket (`NET_RAW`, root) works too.

### Options

| Key | Label | Default | Help |
|---|---|---|---|
| `count` | Number of echoes | `4` | Packets sent on each poll, from 1 to 20. |
| `packet_timeout_ms` | Wait per packet (ms) | `1000` | Time allowed for each reply, from 50 to 10,000 ms. |
| `interval_ms` | Gap between two echoes (ms) | `100` | Pause between two echoes, from 0 to 5,000 ms. |
| `payload_bytes` | Payload size (bytes) | `56` | Data carried by each echo, from 0 to 1,400 bytes. |
| `ip_version` | IP version | `auto` | "auto" takes the first resolved address; force 4 or 6 if the host has both and one does not answer. Choices: auto, 4, 6. |
| `loss_threshold_percent` | Tolerated loss (%) | `100` | Above this percentage of lost packets, the host is reported down. 100: only a total loss counts, partial loss stays visible in the charts. |
| `timeout_seconds` | Timeout (seconds) | `5` | Total budget for the poll, from 1 to 60. Refused if it does not cover "number of echoes × wait per packet". |

## TLS

**TLS certificate** — checks that a certificate is valid and warns before it
expires, on any encrypted port. Examples: mail server (IMAPS, SMTPS), reverse
proxy, LDAPS directory, MQTT broker, administration interface.

### Setup

1. In the address, write the host and, if it is not 443, the port of the
   encrypted service: "mail.example.com:993", "ldap.home.lan:636".
2. The check opens an encrypted connection, reads the presented certificate and
   closes right away: no data is exchanged with the application.
3. It records the number of days before expiry, the issuer and the TLS version.
   The bundled alert rules warn at fourteen days, then at expiry.
4. If the service is reached by its IP address but the certificate carries a
   name, enter that name in "Server name (SNI)".
5. For a certificate issued by your own authority, tick "Accept an unverifiable
   certificate": the expiry date is still monitored.

!!! warning
    For a website, the "Website or web API" type already reads the certificate:
    this type is meant for services that do not speak HTTP, or whose
    application you do not want to hit.

The handshake completes even when the certificate is refused: that is what
allows reading the expiry date of a certificate that has already expired. The
verdict of the verification is in `probe_ssl_cert_valid`.

### Options

| Key | Label | Default | Help |
|---|---|---|---|
| `server_name` | Server name (SNI) | *(empty)* | Domain name announced to the server and checked in the certificate. Set it when the address is an IP behind a reverse proxy. |
| `insecure_tls` | Accept an unverifiable certificate | `false` | An unverifiable chain no longer counts as a failure: the expiry date is still recorded. |
| `allow_private_targets` | Allow loopback and link-local targets | `false` | By default the check refuses addresses only the DumbMonit host itself can reach (127.0.0.1, ::1, 169.254.x.x). Enable this to monitor a service running on the DumbMonit host. Private LAN addresses (10.x, 192.168.x) are always allowed. |
| `timeout_seconds` | Timeout (seconds) | `5` | Time after which the service is reported down if it has not answered. Between 1 and 60. |

## SMTP

**Mail relay (SMTP)** — opens a real session on a mail server: greeting, EHLO,
STARTTLS, and the login if you give one. Examples: your provider's SMTP relay,
a local Postfix or msmtp, Proxmox Mail Gateway, the submission port of a mail
server.

### Setup

1. In the address, write the mail server, and the port if it is not the usual
   one: "smtp.example.com" or "smtp.example.com:2525".
2. Pick the encryption your relay uses: STARTTLS for the submission port 587,
   TLS for port 465, None for a relay on your own network listening on port 25.
3. To check that the relay still accepts your account, fill in the credential:
   the check then runs AUTH and reports a refused password as such, not as an
   outage.
4. Nothing is ever sent: the check stops after the greeting, the extensions and
   the optional login, then hangs up. No message enters the queue.
5. To be warned when an extension disappears, name it in "Expected extension":
   a relay that stops advertising STARTTLS is a relay that would send your mail
   in clear text.

!!! warning
    An accepted session does not prove mail leaves. A relay that accepts
    everything and queues it forever answers perfectly here: watch the queues
    themselves for that.

Credentials: none, or a username and password sent with `AUTH PLAIN` (or
`AUTH LOGIN` when the server offers only that). They are sent **after** the
connection is encrypted and after the certificate has been judged: a relay whose
chain cannot be verified never receives them.

Over TLS or STARTTLS, the certificate is read like the TLS check reads it —
expiry, issuer, protocol version — so a mail relay is also a certificate you are
warned about.

### What it cannot detect

That mail actually leaves. A relay that accepts every session and then defers
everything answers perfectly here; watch its queues instead (see the Proxmox
Mail Gateway page). It also never sends a message, so it says nothing about
recipient filtering, SPF or DKIM.

### Options

| Key | Label | Default | Help |
|---|---|---|---|
| `security` | Encryption | `starttls` | STARTTLS starts in clear text and upgrades (port 587), TLS encrypts from the first byte (port 465), None never encrypts (port 25) and only suits a relay on your own network. |
| `port` | Port | *(empty)* | Used if the address does not give one. Empty: 587 with STARTTLS, 465 with TLS, 25 without encryption. |
| `helo_name` | Name announced (EHLO) | `dumbmonit` | Name the check gives when it introduces itself. A strict relay refuses a name it cannot resolve. |
| `expect_capability` | Expected extension | *(empty)* | Extension the server must advertise in its EHLO answer, for example STARTTLS or AUTH. Empty: no expectation. |
| `server_name` | Server name (SNI) | *(the host)* | Domain name announced to the server and checked in the certificate. Set it when the address is an IP. |
| `insecure_tls` | Accept an unverifiable certificate | `false` | An unverifiable chain no longer counts as a failure: the expiry date is still recorded. |
| `allow_private_targets` | Allow loopback and link-local targets | `false` | See "Addresses the checks refuse" below. |
| `timeout_seconds` | Timeout (seconds) | `5` | Time after which the service is reported down if it has not answered. Between 1 and 60. |

### Failure reasons

| `reason` | What happened |
|---|---|
| `connect` | The port is closed or the relay is unreachable. |
| `protocol` | Something answered, but not SMTP — or it refused the connection, `EHLO`, or STARTTLS it had advertised. |
| `auth` | The account was refused (`535`, `530`, `534`). The relay is fine; the credential is not. |
| `payload` | The extension named in "Expected extension" is not advertised. |
| `tls`, `cert_expired` | Same meaning as for the TLS check. |

## PostgreSQL

**PostgreSQL database** — connects, authenticates and runs one query: the signal
for a database that is up but no longer answering. Examples: the database behind
Nextcloud or Immich, a Home Assistant recorder, an application database, a
TimescaleDB instance.

### Setup

1. On the server, create an account for monitoring and give it nothing more
   than the right to connect: CREATE ROLE dumbmonit LOGIN PASSWORD
   'a-long-password';
2. Make sure that account may reach the server from the DumbMonit machine,
   which usually means one more line in pg_hba.conf followed by a configuration
   reload.
3. In the address, write the server, and the port if it is not 5432:
   "db.home.lan" or "db.home.lan:5433".
4. The default query is SELECT 1, which reads no table: nothing else has to be
   granted. Connection time and query time are recorded separately.
5. To watch something of your own, replace the query with one that returns a
   single number, and it becomes a chart: a queue length, a row count, a
   replication lag.

!!! warning
    The check opens a real connection on every poll. On an instance already
    close to its connection limit, give it a longer interval.

Credentials: a username and password, required. A database is never monitored
anonymously, so the form offers nothing else.

### What it cannot detect

Replication lag, a lock that drags on, a table that keeps growing. All of those
live in the engine's own administration views — write the query yourself, and
the number it returns becomes `probe_sql_value`, a chart you can alert on.

### Options

| Key | Label | Default | Help |
|---|---|---|---|
| `port` | Port | `5432` | Used if the address does not give one. |
| `database` | Database | `postgres` | Database opened on connection. The monitoring account must be allowed to connect to it. |
| `query` | Query | `SELECT 1` | Run on every check. The default reads no table, so the account needs no privilege beyond connecting. A query returning one number turns it into a chart. |
| `expect` | Expected value | *(empty)* | Value the first column of the first row must hold. Empty: only the query succeeding is checked. |
| `sslmode` | Encryption | `prefer` | `prefer` encrypts when the server offers it, `require` refuses to connect without it, `verify-full` also checks the certificate and the name, `disable` never encrypts. |
| `allow_private_targets` | Allow loopback and link-local targets | `false` | See "Addresses the checks refuse" below. |
| `timeout_seconds` | Timeout (seconds) | `5` | Time after which the service is reported down if it has not answered. Between 1 and 60. |

## MySQL and MariaDB

**MySQL or MariaDB database** — connects, authenticates and runs one query: the
signal for a database that is up but no longer answering. Examples: the database
behind a WordPress, a Nextcloud or a Kimai, an application database, a MariaDB
in Docker.

### Setup

1. On the server, create an account for monitoring with no privilege at all:
   CREATE USER 'dumbmonit'@'%' IDENTIFIED BY 'a-long-password';
2. Check that the account may connect from the DumbMonit machine: a host
   pattern of localhost would only accept connections made on the server
   itself.
3. In the address, write the server, and the port if it is not 3306:
   "db.home.lan" or "db.home.lan:3307".
4. The default query is SELECT 1, which reads no table: nothing else has to be
   granted. Connection time and query time are recorded separately.
5. To watch something of your own, replace the query with one that returns a
   single number, and it becomes a chart: a queue length, a row count, a
   replication lag.

!!! warning
    The check opens a real connection on every poll. On an instance already
    close to its connection limit, give it a longer interval.

### Options

| Key | Label | Default | Help |
|---|---|---|---|
| `port` | Port | `3306` | Used if the address does not give one. |
| `database` | Database | *(empty)* | Database opened on connection. Empty: none, which is enough for the default query. |
| `query` | Query | `SELECT 1` | Run on every check. The default reads no table, so the account needs no privilege beyond connecting. A query returning one number turns it into a chart. |
| `expect` | Expected value | *(empty)* | Value the first column of the first row must hold. Empty: only the query succeeding is checked. |
| `sslmode` | Encryption | `prefer` | `prefer` encrypts when the server offers it, `require` refuses to connect without it, `verify-full` also checks the certificate and the name, `disable` never encrypts. |
| `allow_private_targets` | Allow loopback and link-local targets | `false` | See "Addresses the checks refuse" below. |
| `timeout_seconds` | Timeout (seconds) | `5` | Time after which the service is reported down if it has not answered. Between 1 and 60. |

### Failure reasons, both engines

| `reason` | What happened |
|---|---|
| `connect` | The port is closed, the server is unreachable, or it refused the connection for a reason other than the account (starting up, too many connections). |
| `auth` | The account was refused: wrong password, no `pg_hba.conf` line, host pattern that does not cover the DumbMonit machine. |
| `query` | Connected and authenticated, but the query failed: missing table, database in read-only, syntax error. |
| `payload` | The query worked but returned something other than the expected value. |
| `timeout` | The connection or the query did not finish in time — the "up but slow" case. |

## MQTT

**MQTT broker** — connects to the broker, subscribes to a topic, and can wait
for a retained message. Examples: Mosquitto, the broker behind Home Assistant,
Zigbee2MQTT, ESPHome sensors.

### Setup

1. In the address, write the broker, and the port if it is not the usual one:
   "broker.home.lan" or "broker.home.lan:1884".
2. Tick "Encrypted connection" for a broker listening on 8883, and fill in the
   credential if it requires an account.
3. Connecting is already worth monitoring: a broker that refuses your account,
   or that has stopped answering, is the reason the whole house went quiet.
4. To go further, name a topic: the check then subscribes to it, and a broker
   that refuses the subscription tells you the account lost its access rights.
5. Tick "Expect a retained message" only for a topic that carries one, typically
   an availability topic. A topic published to now and then looks silent to a
   client that has just connected.

!!! warning
    The check never publishes anything. It also cannot tell how old a retained
    message is: MQTT does not date them, so a value frozen three weeks ago still
    counts as present.

### What it cannot detect

That a given sensor stopped emitting. The retained message the check reads is
whatever the broker kept, possibly three weeks old — MQTT does not timestamp it.
For freshness, watch the automation that consumes it, or use a heartbeat.

### Options

| Key | Label | Default | Help |
|---|---|---|---|
| `tls` | Encrypted connection | `false` | Encrypts the session from the first byte, as brokers do on port 8883. |
| `port` | Port | *(empty)* | Used if the address does not give one. Empty: 1883 in clear text, 8883 encrypted. |
| `topic` | Topic | *(empty)* | Topic the check subscribes to. Empty: it only connects, which already tells you the broker is alive and accepts your account. |
| `expect_message` | Expect a retained message | `false` | A message must arrive on that topic before the timeout. Only retained messages arrive right away: a topic that is merely published to from time to time will look silent. |
| `expect` | Expected content | *(empty)* | Text that message must contain. Filling it in implies expecting a message. |
| `client_id` | Client identifier | *(derived)* | Name announced to the broker. The default derives from the device, so two checks never disconnect each other. |
| `server_name` | Server name (SNI) | *(the host)* | Domain name announced to the broker and checked in the certificate. Set it when the address is an IP. |
| `insecure_tls` | Accept an unverifiable certificate | `false` | An unverifiable chain no longer counts as a failure: the expiry date is still recorded. |
| `allow_private_targets` | Allow loopback and link-local targets | `false` | See "Addresses the checks refuse" below. |
| `timeout_seconds` | Timeout (seconds) | `5` | Time after which the service is reported down if it has not answered. Between 1 and 60. |

### Failure reasons

| `reason` | What happened |
|---|---|
| `connect` | The port is closed or the broker is unreachable. |
| `auth` | The `CONNACK` refused the account, or the broker refused the subscription — an access-control list no longer covers this topic. |
| `protocol` | Something answered but does not speak MQTT 3.1.1, or the broker rejected the client identifier. |
| `payload` | No retained message arrived on the topic, or its content does not contain the expected text. |

## WebSocket

**WebSocket endpoint** — runs the upgrade handshake, and can send a frame and
wait for one: a different failure from a plain HTTP page. Examples: the Home
Assistant API, a live dashboard, a log stream, an endpoint behind a reverse
proxy.

### Setup

1. In the address, paste the full endpoint:
   "wss://home.example.com/api/websocket". Without a scheme, wss is assumed.
2. The check runs the whole opening handshake and verifies the answer the server
   computes from the key it was sent: a reverse proxy that has lost the Upgrade
   header is caught here, where an HTTP check would still see a healthy 200.
3. Nothing else is needed for most endpoints. A status other than 101 is
   reported with its code, so a 401 from an expired token is not mistaken for an
   outage.
4. To go further, fill in "Expected content" with a fragment of the first frame
   the server sends by itself, for example the greeting of the Home Assistant
   API.
5. If the endpoint says nothing until it is spoken to, put a message in "Frame
   to send" and what its answer must contain in "Expected content".

!!! warning
    The check opens and hangs up within a second. A connection that a firewall
    or a proxy timeout cuts after thirty seconds looks perfectly healthy here.

Credentials: none, a username and password (sent as HTTP basic authentication in
the opening request) or a token (sent as `Authorization: Bearer`).

### Options

| Key | Label | Default | Help |
|---|---|---|---|
| `path` | Path | *(from the address)* | Path of the opening request. Taken from the address when it carries one. |
| `port` | Port | *(empty)* | Used if the address does not give one. Empty: 443 for wss, 80 for ws. |
| `send` | Frame to send | *(empty)* | Text frame sent once the connection is open. Empty: nothing is sent. |
| `expect` | Expected content | *(empty)* | Text a received frame must contain. Empty and with nothing to send, only the opening handshake is checked. |
| `subprotocol` | Subprotocol | *(empty)* | Value of Sec-WebSocket-Protocol. The check fails if the server picks a different one. |
| `origin` | Origin | *(empty)* | Value of the Origin header, which some servers require before upgrading. |
| `server_name` | Server name (SNI) | *(the host)* | Domain name announced to the server and checked in the certificate. Set it when the address is an IP. |
| `insecure_tls` | Accept an unverifiable certificate | `false` | An unverifiable chain no longer counts as a failure: the expiry date is still recorded. |
| `allow_private_targets` | Allow loopback and link-local targets | `false` | See "Addresses the checks refuse" below. |
| `timeout_seconds` | Timeout (seconds) | `5` | Time after which the service is reported down if it has not answered. Between 1 and 60. |

### Failure reasons

| `reason` | What happened |
|---|---|
| `connect` | The port is closed or the host is unreachable. |
| `status` | The endpoint answered something other than `101`; `probe_http_status_code` carries the code. |
| `protocol` | It answered `101` but its `Sec-WebSocket-Accept` does not match the key sent, it picked another subprotocol, or it closed the connection at once. |
| `payload` | No frame arrived, or it does not contain the expected text. |

## Addresses the checks refuse

The checks that connect to an address you give them — HTTP, TCP, TLS, SMTP,
PostgreSQL, MySQL, MQTT, WebSocket — report what they see — which is also, word for word, what a server-side request
forgery does. To keep a monitoring admin from reading services that only the
DumbMonit host can reach (the embedded metrics database on `127.0.0.1:8428`,
a cloud provider's metadata service on `169.254.169.254`), the checks refuse
by default:

- loopback addresses (`127.0.0.0/8`, `::1`);
- link-local addresses (`169.254.0.0/16`, `fe80::/10`), including the cloud
  metadata address, and the unspecified address (`0.0.0.0`, `::`).

Private LAN ranges (`10.0.0.0/8`, `172.16.0.0/12`, `192.168.0.0/16`, `fc00::/7`)
are always allowed: monitoring them is the point of a homelab.

The rule applies to the address you typed, to every address a host name
resolves to (checked again at connection time, so a name that changes its
answer cannot slip through), and to every redirect the HTTP check follows. A
refused address shows as a configuration error on the target — no down alert is
sent — naming the option to enable: `allow_private_targets`. Turn it on for a
target that really monitors a service on the DumbMonit host itself.

A redirect to a refused address is reported as a failed measurement
(`reason="connect"`), because it is the monitored service that changed, not the
configuration.

Values found at a `json_path` are never echoed beyond 32 characters in a
failure message.

## Common errors

| Symptom | Likely cause |
|---|---|
| Ping shows a configuration error | The `net.ipv4.ping_group_range` sysctl is missing from the Compose file. See above. |
| HTTP down with `reason="keyword"` | The keyword is searched in the first `max_body_bytes` only, case-insensitively unless `keyword_case_sensitive` is set. |
| HTTP down with `reason="tls"` | Self-signed or private authority: set `insecure_tls`. |
| A check shows a configuration error naming `allow_private_targets` | The address is loopback or link-local. See "Addresses the checks refuse" above. |
| TCP down although `telnet host port` works from your laptop | The DumbMonit container cannot reach the host (Docker network, firewall). Test from inside the container's network. |
| DNS down with `reason="record"` | The answer does not contain every value listed in `expect`. |
| Ping down with `reason="packet_loss"` | Loss above `loss_threshold_percent`; some devices rate-limit ICMP. |
| DNS down with `reason="record"` although `expect` matches | `expect_mode` is `exact` and the answer holds an extra value, or `forbid` matched. |
| SMTP down with `reason="protocol"` on `security = starttls` | The relay does not advertise STARTTLS on this port. Use `tls` for 465, `none` for a plain local relay. |
| A database check down with `reason="auth"` | The account exists but may not connect from the DumbMonit machine: check `pg_hba.conf`, or the host part of the MySQL user. |
| A database check down with `reason="query"` | Connection and login worked. The database is up and refusing to work: read-only, missing table, recovery. |
| MQTT down with `reason="payload"` on a topic that clearly has traffic | Only *retained* messages reach a client that has just subscribed. Publish that topic with the retain flag, or untick "Expect a retained message". |
| WebSocket down with `reason="protocol"` after a `101` | Something on the path answers the upgrade without speaking WebSocket — usually a reverse proxy rule that swallows the `Upgrade` header. |
