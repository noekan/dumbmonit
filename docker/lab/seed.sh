#!/usr/bin/env bash
# Seeds a running DumbMonit with the test-lab devices and notification channels
# (see docker-compose.lab.yml). Idempotent: a device whose kind + address already
# exists, or a channel with the same name, is left alone.
#
#   docker/lab/seed.sh                       # http://localhost:8080, password dumbmonit-dev-2026
#   DUMBMONIT_PASSWORD=... docker/lab/seed.sh
#   DUMBMONIT_URL=http://192.168.10.254:8080 DUMBMONIT_USER=admin docker/lab/seed.sh
#
# Needs curl and python3 (for JSON parsing) — nothing else.
set -euo pipefail

BASE="${DUMBMONIT_URL:-http://localhost:8080}"
PASSWORD="${DUMBMONIT_PASSWORD:-dumbmonit-dev-2026}"
USERNAME="${DUMBMONIT_USER:-}"

COOKIE="$(mktemp)"
trap 'rm -f "$COOKIE"' EXIT

api() { # method path [json-body]
  local method="$1" path="$2" body="${3:-}"
  if [ -n "$body" ]; then
    curl -sS -b "$COOKIE" -c "$COOKIE" -X "$method" -H 'Content-Type: application/json' \
      --data "$body" "$BASE/api$path"
  else
    curl -sS -b "$COOKIE" -c "$COOKIE" -X "$method" "$BASE/api$path"
  fi
}

json_escape() { python3 -c 'import json,sys; print(json.dumps(sys.argv[1]))' "$1"; }

# ── Login ─────────────────────────────────────────────────────────────────────
login_body="{\"password\": $(json_escape "$PASSWORD")"
[ -n "$USERNAME" ] && login_body="$login_body, \"username\": $(json_escape "$USERNAME")"
login_body="$login_body}"
status="$(curl -sS -o /dev/null -w '%{http_code}' -c "$COOKIE" -H 'Content-Type: application/json' \
  --data "$login_body" "$BASE/api/auth/login")"
if [ "$status" != "204" ] && [ "$status" != "200" ]; then
  echo "login failed on $BASE (HTTP $status): set DUMBMONIT_PASSWORD / DUMBMONIT_USER" >&2
  exit 1
fi
echo "logged in on $BASE"

# ── Devices ───────────────────────────────────────────────────────────────────
existing_targets="$(api GET /targets)"

target_exists() { # kind address
  python3 -c '
import json, sys
kind, address = sys.argv[1], sys.argv[2]
targets = json.load(sys.stdin)
sys.exit(0 if any(t["kind"] == kind and t["address"] == address for t in targets) else 1)
' "$1" "$2" <<<"$existing_targets"
}

add_target() { # name kind address interval credential-json tags-json
  local name="$1" kind="$2" address="$3" interval="$4" credential="$5" tags="$6"
  if target_exists "$kind" "$address"; then
    echo "  = $name ($kind $address) already there"
    return
  fi
  local body
  body="{\"name\": $(json_escape "$name"), \"kind\": $(json_escape "$kind"), \"address\": $(json_escape "$address"),
         \"interval_secs\": $interval, \"credential\": $credential, \"tags\": $tags}"
  local reply
  reply="$(api POST /targets "$body")"
  local id
  id="$(python3 -c 'import json,sys; d=json.load(sys.stdin); print(d.get("id") or "")' <<<"$reply" 2>/dev/null || true)"
  if [ -n "$id" ]; then
    echo "  + $name ($kind $address) -> id $id"
  else
    echo "  ! $name ($kind $address): $reply" >&2
  fi
}

echo "devices:"
add_target "Lab UPS"        snmp "snmp-ups"     30 '{"type":"snmp_community","community":"ups"}'     '{}'
add_target "Lab printer"    snmp "snmp-printer" 60 '{"type":"snmp_community","community":"printer"}' '{}'
add_target "Lab switch"     snmp "snmp-switch"  30 '{"type":"snmp_community","community":"switch"}'  '{}'
add_target "Lab Proxmox VE" proxmox "http://fake-pve:8006" 60 \
  '{"type":"api_token","token":"monitoring@pve!dumbmonit=8f3a1c9e-1ab0-4000-8000-d0bb0000c0de"}' '{}'
add_target "Lab Proxmox Backup" pbs "http://fake-pbs:8007" 60 \
  '{"type":"api_token","token":"monitoring@pbs!dumbmonit=5c1d2e3f-1ab0-4000-8000-d0bb0000c0de"}' '{}'
add_target "Lab Synology"   synology "fake-synology" 60 \
  '{"type":"username_password","username":"monitoring","password":"lab-password"}' '{"scheme":"http","port":"5000"}'
add_target "Lab victim (nginx)" http "http://lab-victim/" 30 '{"type":"none"}' '{}'
add_target "Lab Dex"        http "http://dex:5556/dex/healthz" 60 '{"type":"none"}' '{}'

# ── Notification channels ─────────────────────────────────────────────────────
existing_channels="$(api GET /notify/channels)"

channel_exists() { # name
  python3 -c '
import json, sys
name = sys.argv[1]
channels = json.load(sys.stdin)
sys.exit(0 if any(c["name"] == name for c in channels) else 1)
' "$1" <<<"$existing_channels"
}

add_channel() { # name kind settings-json secrets-json
  local name="$1" kind="$2" settings="$3" secrets="$4"
  if channel_exists "$name"; then
    echo "  = $name ($kind) already there"
    return
  fi
  local body="{\"name\": $(json_escape "$name"), \"kind\": $(json_escape "$kind"), \"enabled\": true,
               \"settings\": $settings, \"secrets\": $secrets}"
  local reply
  reply="$(api POST /notify/channels "$body")"
  local id
  id="$(python3 -c 'import json,sys; d=json.load(sys.stdin); print(d.get("id") or "")' <<<"$reply" 2>/dev/null || true)"
  if [ -n "$id" ]; then
    echo "  + $name ($kind) -> id $id"
  else
    echo "  ! $name ($kind): $reply" >&2
  fi
}

echo "channels:"
add_channel "Lab mailpit" smtp \
  '{"host":"mailpit","security":"none","port":1025,"from":"dumbmonit@lab.local","to":["admin@lab.local"]}' '{}'
add_channel "Lab ntfy" ntfy '{"topic":"dumbmonit-lab","server_url":"http://ntfy:80"}' '{}'

echo "done — mail lands in http://localhost:8025, pushes in http://localhost:8090/dumbmonit-lab"
