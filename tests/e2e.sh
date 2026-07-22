#!/usr/bin/env bash
#
# End-to-end check of the RustDesk API server against a live instance.
#
#   ./tests/e2e.sh
#
# Boots a server on a throwaway SQLite database, drives it with the exact
# request shapes a real RustDesk client sends, and asserts the response details
# the client is strict about.
set -uo pipefail

export PATH="$HOME/.cargo/bin:$PATH"
cd "$(dirname "$0")/.."

SCRATCH="$(mktemp -d)"
trap 'rm -rf "$SCRATCH"' EXIT
DB="$SCRATCH/e2e.db"
PORT="${PORT:-21199}"
BASE="http://127.0.0.1:$PORT"
export RDAPI_DB="sqlite://$DB"
export RDAPI_BIND="127.0.0.1:$PORT"

PASS=0; FAIL=0
check() { # check <label> <expected-substring> <actual>
  if [[ "$3" == *"$2"* ]]; then echo "  PASS  $1"; PASS=$((PASS+1));
  else echo "  FAIL  $1"; echo "        want substring: $2"; echo "        got: $3"; FAIL=$((FAIL+1)); fi
}
check_status() { # check_status <label> <expected> <actual>
  if [[ "$3" == "$2" ]]; then echo "  PASS  $1 (HTTP $3)"; PASS=$((PASS+1));
  else echo "  FAIL  $1 — want HTTP $2, got HTTP $3"; FAIL=$((FAIL+1)); fi
}

echo "=== building ==="
cargo build --quiet || exit 1
BIN=./target/debug/rustdesk-api

echo
echo "=== CLI: create users ==="
$BIN user add alice --password hunter2 --admin --email alice@example.com
$BIN user add bob   --password bobpass
$BIN user list

echo
echo "=== start server ==="
$BIN serve > "$SCRATCH/server.log" 2>&1 &
SERVER_PID=$!
trap 'kill $SERVER_PID 2>/dev/null; rm -rf "$SCRATCH"' EXIT

for _ in $(seq 1 50); do
  curl -sf "$BASE/health" >/dev/null 2>&1 && break
  sleep 0.1
done
check "health endpoint responds" '"status":"ok"' "$(curl -s $BASE/health)"

echo
echo "=== device self-registration (no auth) ==="
SYSINFO=$(curl -s -X POST "$BASE/api/sysinfo" -H 'Content-Type: application/json' -d '{
  "id":"123456789","uuid":"dGVzdC11dWlk","hostname":"DESKTOP-ALPHA","username":"alice",
  "os":"windows / Windows 11 Pro","version":"1.4.0","cpu":"Intel i7, 8/16 cores","memory":"32GB"}')
check "sysinfo returns exact SYSINFO_UPDATED token" "SYSINFO_UPDATED" "$SYSINFO"

curl -s -X POST "$BASE/api/sysinfo" -H 'Content-Type: application/json' -d '{
  "id":"987654321","uuid":"dXVpZC10d28=","hostname":"mac-studio","username":"bob",
  "os":"macos / macOS 14.5","version":"1.4.0","cpu":"Apple M2","memory":"16GB"}' > /dev/null
curl -s -X POST "$BASE/api/sysinfo" -H 'Content-Type: application/json' -d '{
  "id":"555000111","uuid":"dXVpZC10aHJlZQ==","hostname":"ubuntu-box","username":"svc",
  "os":"linux / Ubuntu 24.04","version":"1.3.8","cpu":"AMD Ryzen","memory":"8GB"}' > /dev/null

echo "  -> registered devices:"
$BIN device list | sed 's/^/     /'

echo
echo "=== heartbeat ==="
HB=$(curl -s -X POST "$BASE/api/heartbeat" -H 'Content-Type: application/json' \
  -d '{"id":"123456789","uuid":"dGVzdC11dWlk","ver":1004000,"modified_at":0}')
check "known device heartbeat returns bare {}" "{}" "$HB"
HB_NEW=$(curl -s -X POST "$BASE/api/heartbeat" -H 'Content-Type: application/json' \
  -d '{"id":"000000000","uuid":"dW5rbm93bg==","ver":1004000,"modified_at":0}')
check "unknown device is asked for sysinfo" '"sysinfo"' "$HB_NEW"

echo
echo "=== login-options (TLS probe) ==="
check "login-options returns empty array" "[]" "$(curl -s $BASE/api/login-options)"

echo
echo "=== login ==="
BAD=$(curl -s -X POST "$BASE/api/login" -H 'Content-Type: application/json' \
  -d '{"username":"alice","password":"wrong","id":"111","uuid":"eA==","type":"account","deviceInfo":{"os":"macos","type":"client","name":"laptop"}}')
check "wrong password returns an error body" '"error"' "$BAD"

LOGIN=$(curl -s -X POST "$BASE/api/login" -H 'Content-Type: application/json' \
  -d '{"username":"alice","password":"hunter2","id":"111222333","uuid":"bGFwdG9w","autoLogin":true,"type":"account","deviceInfo":{"os":"macos","type":"client","name":"laptop"}}')
check "login returns type=access_token" '"type":"access_token"' "$LOGIN"
check "login user payload has is_admin bool" '"is_admin":true' "$LOGIN"
check "login user payload has info object" '"info":{}' "$LOGIN"
TOKEN=$(echo "$LOGIN" | sed -n 's/.*"access_token":"\([^"]*\)".*/\1/p')
echo "  -> token: ${TOKEN:0:16}..."
AUTH="Authorization: Bearer $TOKEN"

echo
echo "=== currentUser ==="
CU=$(curl -s -X POST "$BASE/api/currentUser" -H "$AUTH" -H 'Content-Type: application/json' -d '{"id":"111222333","uuid":"bGFwdG9w"}')
check "currentUser returns a BARE payload (no user wrapper)" '"name":"alice"' "$CU"
if [[ "$CU" == *'"user"'* ]]; then echo "  FAIL  currentUser must not wrap in 'user'"; FAIL=$((FAIL+1)); else echo "  PASS  currentUser is not wrapped"; PASS=$((PASS+1)); fi

BADTOK=$(curl -s -o /dev/null -w '%{http_code}' -X POST "$BASE/api/currentUser" -H 'Authorization: Bearer nonsense' -H 'Content-Type: application/json' -d '{}')
check_status "invalid token gets 401 (triggers client logout)" "401" "$BADTOK"

echo
echo "=== address book: protocol selection ==="
PERSONAL=$(curl -s -X POST "$BASE/api/ab/personal" -H "$AUTH" -H 'Content-Length: 0')
check "personal returns a guid (selects modern protocol)" '"guid"' "$PERSONAL"
PGUID=$(echo "$PERSONAL" | sed -n 's/.*"guid":"\([^"]*\)".*/\1/p')
echo "  -> personal guid: $PGUID"

PERSONAL2=$(curl -s -X POST "$BASE/api/ab/personal" -H "$AUTH" -H 'Content-Length: 0')
check "personal guid is stable across calls" "$PGUID" "$PERSONAL2"

check "settings returns max_peer_one_ab" '"max_peer_one_ab":0' "$(curl -s -X POST "$BASE/api/ab/settings" -H "$AUTH" -H 'Content-Length: 0')"

echo
echo "=== address book: shared profiles ==="
PROFILES=$(curl -s -X POST "$BASE/api/ab/shared/profiles?current=1&pageSize=100" -H "$AUTH" -H 'Content-Length: 0')
echo "  -> $PROFILES"
check "shared profiles include 'total' (else client drops data)" '"total"' "$PROFILES"
check "shared book is named All Workstations" 'All Workstations' "$PROFILES"
check "shared book is read-only (rule=1)" '"rule":1' "$PROFILES"
if [[ "$PROFILES" == *"$PGUID"* ]]; then echo "  FAIL  personal book must NOT appear in shared profiles"; FAIL=$((FAIL+1)); else echo "  PASS  personal book excluded from shared profiles"; PASS=$((PASS+1)); fi

echo
echo "=== address book: the actual feature — all workstations visible ==="
PEERS=$(curl -s -X POST "$BASE/api/ab/peers?current=1&pageSize=100&ab=all-workstations" -H "$AUTH" -H 'Content-Length: 0')
echo "  -> $PEERS"
check "peers response carries total" '"total":3' "$PEERS"
check "alpha workstation present" 'DESKTOP-ALPHA' "$PEERS"
check "mac workstation present" 'mac-studio' "$PEERS"
check "linux workstation present" 'ubuntu-box' "$PEERS"
check "windows platform mapped" '"platform":"Windows"' "$PEERS"
check "macos platform mapped to 'Mac OS'" '"platform":"Mac OS"' "$PEERS"
check "linux platform mapped" '"platform":"Linux"' "$PEERS"
check "forceAlwaysRelay is the STRING 'false'" '"forceAlwaysRelay":"false"' "$PEERS"

echo
echo "=== second user sees the same workstations ==="
BLOGIN=$(curl -s -X POST "$BASE/api/login" -H 'Content-Type: application/json' \
  -d '{"username":"bob","password":"bobpass","id":"444","uuid":"Yg==","type":"account","deviceInfo":{"os":"linux","type":"client","name":"bobpc"}}')
BTOKEN=$(echo "$BLOGIN" | sed -n 's/.*"access_token":"\([^"]*\)".*/\1/p')
BAUTH="Authorization: Bearer $BTOKEN"
BPEERS=$(curl -s -X POST "$BASE/api/ab/peers?current=1&pageSize=100&ab=all-workstations" -H "$BAUTH" -H 'Content-Length: 0')
check "bob sees all 3 workstations too" '"total":3' "$BPEERS"
BPERSONAL=$(curl -s -X POST "$BASE/api/ab/personal" -H "$BAUTH" -H 'Content-Length: 0')
BGUID=$(echo "$BPERSONAL" | sed -n 's/.*"guid":"\([^"]*\)".*/\1/p')
if [[ "$BGUID" != "$PGUID" && -n "$BGUID" ]]; then echo "  PASS  bob gets his own personal book"; PASS=$((PASS+1)); else echo "  FAIL  personal books collide"; FAIL=$((FAIL+1)); fi

echo
echo "=== shared book is genuinely read-only ==="
RO=$(curl -s -X POST "$BASE/api/ab/peer/add/all-workstations" -H "$AUTH" -H 'Content-Type: application/json' -d '{"id":"999","tags":[]}')
check "writing to shared book is refused" '"error"' "$RO"
RO2=$(curl -s -X DELETE "$BASE/api/ab/peer/all-workstations" -H "$AUTH" -H 'Content-Type: application/json' -d '["123456789"]')
check "deleting from shared book is refused" '"error"' "$RO2"
STILL=$(curl -s -X POST "$BASE/api/ab/peers?ab=all-workstations" -H "$AUTH" -H 'Content-Length: 0')
check "shared book unchanged after refused writes" '"total":3' "$STILL"

echo
echo "=== personal book read/write cycle ==="
ADD=$(curl -s -o /dev/null -w '%{http_code}' -X POST "$BASE/api/ab/peer/add/$PGUID" -H "$AUTH" -H 'Content-Type: application/json' \
  -d '{"id":"123456789","alias":"Reception PC","tags":["office"],"note":"front desk"}')
check_status "add peer to personal book" "200" "$ADD"
ADDBODY=$(curl -s -X POST "$BASE/api/ab/peer/add/$PGUID" -H "$AUTH" -H 'Content-Type: application/json' -d '{"id":"987654321","alias":"Design Mac","tags":[]}')
if [[ -z "$ADDBODY" ]]; then echo "  PASS  successful mutation returns ZERO-length body"; PASS=$((PASS+1)); else echo "  FAIL  mutation body must be empty, got: $ADDBODY"; FAIL=$((FAIL+1)); fi

MINE=$(curl -s -X POST "$BASE/api/ab/peers?ab=$PGUID" -H "$AUTH" -H 'Content-Length: 0')
echo "  -> $MINE"
check "personal book has 2 peers" '"total":2' "$MINE"
check "alias persisted" 'Reception PC' "$MINE"
check "tags persisted" '"tags":["office"]' "$MINE"
check "note persisted" '"note":"front desk"' "$MINE"

echo
echo "  -- sparse update (only the changed field is sent) --"
curl -s -X PUT "$BASE/api/ab/peer/update/$PGUID" -H "$AUTH" -H 'Content-Type: application/json' -d '{"id":"123456789","alias":"Lobby PC"}' > /dev/null
UPD=$(curl -s -X POST "$BASE/api/ab/peers?ab=$PGUID" -H "$AUTH" -H 'Content-Length: 0')
check "alias updated" 'Lobby PC' "$UPD"
check "untouched note preserved by sparse update" '"note":"front desk"' "$UPD"
check "untouched tags preserved by sparse update" '"tags":["office"]' "$UPD"

echo
echo "  -- tags --"
curl -s -X POST "$BASE/api/ab/tag/add/$PGUID" -H "$AUTH" -H 'Content-Type: application/json' -d '{"name":"office","color":4288585374}' > /dev/null
curl -s -X POST "$BASE/api/ab/tag/add/$PGUID" -H "$AUTH" -H 'Content-Type: application/json' -d '{"name":"servers","color":4278238420}' > /dev/null
TAGS=$(curl -s -X POST "$BASE/api/ab/tags/$PGUID" -H "$AUTH" -H 'Content-Length: 0')
echo "  -> $TAGS"
if [[ "$TAGS" == "["* ]]; then echo "  PASS  tags endpoint returns a BARE array"; PASS=$((PASS+1)); else echo "  FAIL  tags must be a bare array, got: $TAGS"; FAIL=$((FAIL+1)); fi
check "tag colour preserved as int" '"color":4288585374' "$TAGS"

curl -s -X PUT "$BASE/api/ab/tag/rename/$PGUID" -H "$AUTH" -H 'Content-Type: application/json' -d '{"old":"office","new":"hq"}' > /dev/null
RENAMED=$(curl -s -X POST "$BASE/api/ab/tags/$PGUID" -H "$AUTH" -H 'Content-Length: 0')
check "tag renamed" '"name":"hq"' "$RENAMED"
REPEERS=$(curl -s -X POST "$BASE/api/ab/peers?ab=$PGUID" -H "$AUTH" -H 'Content-Length: 0')
check "rename cascaded into peer tag lists" '"tags":["hq"]' "$REPEERS"

curl -s -X DELETE "$BASE/api/ab/tag/$PGUID" -H "$AUTH" -H 'Content-Type: application/json' -d '["hq"]' > /dev/null
DELTAGS=$(curl -s -X POST "$BASE/api/ab/tags/$PGUID" -H "$AUTH" -H 'Content-Length: 0')
if [[ "$DELTAGS" != *'"hq"'* ]]; then echo "  PASS  tag deleted"; PASS=$((PASS+1)); else echo "  FAIL  tag not deleted"; FAIL=$((FAIL+1)); fi
STRIPPED=$(curl -s -X POST "$BASE/api/ab/peers?ab=$PGUID" -H "$AUTH" -H 'Content-Length: 0')
check "deleted tag stripped from peers" '"tags":[]' "$STRIPPED"

echo
echo "  -- delete peer --"
curl -s -X DELETE "$BASE/api/ab/peer/$PGUID" -H "$AUTH" -H 'Content-Type: application/json' -d '["987654321"]' > /dev/null
LEFT=$(curl -s -X POST "$BASE/api/ab/peers?ab=$PGUID" -H "$AUTH" -H 'Content-Length: 0')
check "peer deleted from personal book" '"total":1' "$LEFT"

echo
echo "=== cross-user isolation ==="
STEAL=$(curl -s -X POST "$BASE/api/ab/peers?ab=$PGUID" -H "$BAUTH" -H 'Content-Length: 0')
check "bob cannot read alice's personal book" '"error"' "$STEAL"
STEALW=$(curl -s -X POST "$BASE/api/ab/peer/add/$PGUID" -H "$BAUTH" -H 'Content-Type: application/json' -d '{"id":"1","tags":[]}')
check "bob cannot write to alice's personal book" '"error"' "$STEALW"

echo
echo "=== pagination ==="
PAGE=$(curl -s -X POST "$BASE/api/ab/peers?current=1&pageSize=2&ab=all-workstations" -H "$AUTH" -H 'Content-Length: 0')
check "page 1 reports full total" '"total":3' "$PAGE"
COUNT=$(echo "$PAGE" | grep -o '"id":"' | wc -l | tr -d ' ')
check "page 1 returns pageSize items" "2" "$COUNT"
PAGE2=$(curl -s -X POST "$BASE/api/ab/peers?current=2&pageSize=2&ab=all-workstations" -H "$AUTH" -H 'Content-Length: 0')
COUNT2=$(echo "$PAGE2" | grep -o '"id":"' | wc -l | tr -d ' ')
check "page 2 returns the remainder" "1" "$COUNT2"

echo
echo "=== 'Accessible devices' tab ==="
# The client joins these three by NAME: peer.device_group_name must equal a
# group's name exactly, and `total` must be present or `data` is never read.
DG=$(curl -s "$BASE/api/device-group/accessible?current=1&pageSize=100" -H "$BAUTH")
echo "  -> $DG"
check "device group list carries total" '"total"' "$DG"
check "device group is named All Workstations" '"name":"All Workstations"' "$DG"

GPEERS=$(curl -s "$BASE/api/peers?current=1&pageSize=100&accessible&status=1" -H "$BAUTH")
echo "  -> $GPEERS"
check "group peers carry total" '"total":3' "$GPEERS"
check "peer links to the group by exact name" '"device_group_name":"All Workstations"' "$GPEERS"
check "hostname is under info.device_name (NOT info.hostname)" '"device_name":"DESKTOP-ALPHA"' "$GPEERS"
check "info.os element 0 is a platform token the client matches" '"os":"windows / windows / Windows 11 Pro"' "$GPEERS"
# Debian reports "debian / ..." as element 0, which matches no platform; the
# prefix is what keeps the icon correct.
check "linux device gets a matchable os token" '"os":"linux / linux / Ubuntu 24.04"' "$GPEERS"
check "macos device gets a matchable os token" '"os":"macos / macos / macOS 14.5"' "$GPEERS"
check "info.username present" '"username":"alice"' "$GPEERS"
if [[ "$GPEERS" == *'"info":{"hostname"'* ]]; then echo "  FAIL  info must use device_name, not hostname"; FAIL=$((FAIL+1)); else echo "  PASS  info does not use the wrong hostname key"; PASS=$((PASS+1)); fi

GUSERS=$(curl -s "$BASE/api/users?current=1&pageSize=100&accessible&status=1" -H "$BAUTH")
check "users list is valid and carries total" '"total"' "$GUSERS"
if [[ "$GUSERS" != *'"error"'* ]]; then echo "  PASS  users has no error key (would abort the client pull)"; PASS=$((PASS+1)); else echo "  FAIL  users must not carry an error key"; FAIL=$((FAIL+1)); fi
if [[ "$GPEERS" != *'"error"'* ]]; then echo "  PASS  peers has no error key (would abort the client pull)"; PASS=$((PASS+1)); else echo "  FAIL  peers must not carry an error key"; FAIL=$((FAIL+1)); fi

# `total` must be a JSON number — a string throws client-side and fails the fetch.
if echo "$GPEERS" | grep -q '"total":[0-9]'; then echo "  PASS  total is a JSON number, not a string"; PASS=$((PASS+1)); else echo "  FAIL  total must be numeric"; FAIL=$((FAIL+1)); fi

echo
echo "=== audit sinks + stubs ==="
check_status "audit/conn accepted" "200" "$(curl -s -o /dev/null -w '%{http_code}' -X POST "$BASE/api/audit/conn" -H 'Content-Type: application/json' -d '{"action":"new","ip":"1.2.3.4","id":"1","uuid":"x","conn_id":3,"session_id":1}')"
check_status "audit/file accepted" "200" "$(curl -s -o /dev/null -w '%{http_code}' -X POST "$BASE/api/audit/file" -H 'Content-Type: application/json' -d '{}')"
check_status "audit/alarm accepted" "200" "$(curl -s -o /dev/null -w '%{http_code}' -X POST "$BASE/api/audit/alarm" -H 'Content-Type: application/json' -d '{}')"
check_status "PUT /api/audit accepted" "200" "$(curl -s -o /dev/null -w '%{http_code}' -X PUT "$BASE/api/audit" -H 'Content-Type: application/json' -d '{"guid":"x","note":"n"}')"
# /api/users, /api/peers and /api/device-group/accessible are covered by the
# "Accessible devices" section above.

echo
echo "=== logout ==="
curl -s -X POST "$BASE/api/logout" -H "$AUTH" -H 'Content-Type: application/json' -d '{"id":"111222333","uuid":"bGFwdG9w"}' > /dev/null
AFTER=$(curl -s -o /dev/null -w '%{http_code}' -X POST "$BASE/api/currentUser" -H "$AUTH" -H 'Content-Type: application/json' -d '{}')
check_status "token rejected after logout" "401" "$AFTER"

echo
echo "=== device removal ==="
$BIN device rm 555000111
GONE=$(curl -s -X POST "$BASE/api/ab/peers?ab=all-workstations" -H "$BAUTH" -H 'Content-Length: 0')
check "removed device drops out of the address book" '"total":2' "$GONE"

echo
echo "=== unknown endpoint logging ==="
check_status "unknown route 404s" "404" "$(curl -s -o /dev/null -w '%{http_code}' "$BASE/api/some/future/endpoint")"
check "unknown route was logged for diagnosis" "unhandled request" "$(cat "$SCRATCH/server.log")"

echo
echo "================================"
echo "  PASSED: $PASS    FAILED: $FAIL"
echo "================================"
[[ $FAIL -eq 0 ]]
