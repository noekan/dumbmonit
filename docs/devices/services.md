# Services: HTTP, TCP, DNS, ping, TLS

Five monitors watch **services** rather than equipment, Uptime Kuma style: a
web page, a port, a DNS name, a host, a certificate. Each service gets a history
bar, a response time and an availability percentage on its device page.

## How service state works

A service can answer and still be down: a `500`, a missing keyword, an expired
certificate. So these monitors write a point on every check, `probe_success`
being 1 or 0, instead of going silent like a hardware device. That is what
makes an availability percentage possible: `avg_over_time(dumbmonit_probe_success[30d])`.

| Situation | What happens |
|---|---|
| Invalid option or address, ICMP sockets not allowed (see Ping) | Configuration error: nothing is written, the device page shows the error, no "down" alert. |
| Connection refused, timeout, TLS refused, `500`, keyword absent, total packet loss | `probe_success = 0`: the outage is recorded, dated and counted. |
| All good | `probe_success = 1`. |

Every monitor has its own timeout (`timeout_seconds`, 5 s by default, 60 s at
most), shorter than the server's `DUMBMONIT_PROBE_TIMEOUT_SECS`: interrupted by
the scheduler, it could not write its zero.

Built-in rules that apply to all five: Service down (3 minutes), Service
flapping (more than six state changes in thirty minutes), Slow service (more
than 3 s for ten minutes). For `http` and `tls`: Certificate expiring soon
(14 days) and Certificate expired.

### Metrics

All gauges, all prefixed `dumbmonit_`, all labelled `probe="http|tcp|dns|ping|tls"`
in addition to `target`, `host` and `tag_*`.

| Metric | Monitors | Meaning |
|---|---|---|
| `probe_success` | all | 1 if the service answers correctly, 0 otherwise. |
| `probe_duration_seconds` | all | Total duration of the check. |
| `probe_failure_info` | all | Presence (1) with a `reason` label: `dns`, `connect`, `timeout`, `tls`, `cert_expired`, `status`, `keyword`, `json`, `body`, `packet_loss`, `record`. |
| `probe_http_status_code` | http | Status code obtained. |
| `probe_http_first_byte_seconds` | http | Time to the response headers. |
| `probe_http_content_bytes` | http | Body size. |
| `probe_connect_seconds` | tcp, tls, http | TCP connection establishment. |
| `probe_tls_handshake_seconds` | tls, http | TLS negotiation alone. |
| `probe_tls_version_info` | tls, http | Presence; `version` label. |
| `probe_ssl_cert_expiry_days` | tls, http | Days before expiry, negative if expired. |
| `probe_ssl_cert_valid` | tls, http | 1 if the chain leads to a known authority. |
| `probe_ssl_cert_issuer_info` | tls, http | Presence; `issuer` label. |
| `probe_dns_lookup_seconds` | dns | Resolution time. |
| `probe_dns_answer_records` | dns | Records of the requested type. |
| `probe_icmp_rtt_seconds`, `_min_seconds`, `_max_seconds` | ping | Average, shortest and longest round trip. |
| `probe_icmp_packet_loss_ratio` | ping | Loss between 0 and 1. |
| `probe_icmp_packets_sent`, `probe_icmp_packets_received` | ping | Echoes sent and received. |

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
| `timeout_seconds` | Timeout (seconds) | `5` | Time after which the service is reported down if it has not answered. Between 1 and 60. |

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

## Addresses the checks refuse

The HTTP, TCP and TLS checks connect to whatever address you give them and
report what they see — which is also, word for word, what a server-side request
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
| HTTP, TCP or TLS shows a configuration error naming `allow_private_targets` | The address is loopback or link-local. See "Addresses the checks refuse" above. |
| TCP down although `telnet host port` works from your laptop | The DumbMonit container cannot reach the host (Docker network, firewall). Test from inside the container's network. |
| DNS down with `reason="record"` | The answer does not contain every value listed in `expect`. |
| Ping down with `reason="packet_loss"` | Loss above `loss_threshold_percent`; some devices rate-limit ICMP. |
