#!/usr/bin/env sh
# End-to-end smoke test against a running Uptime Medics instance.
# Usage: scripts/smoke_test.sh [base_url]   (default http://localhost:8080)
set -eu

BASE="${1:-http://localhost:8080}"
JAR="$(mktemp)"
trap 'rm -f "$JAR"' EXIT
STAMP="$(date +%s)"
FAILED=0

check() { # name, expected_status, actual_status, body
  if [ "$2" = "$3" ]; then
    printf '  ok   %-52s -> %s\n' "$1" "$3"
  else
    printf '  FAIL %-52s -> %s (expected %s)\n%s\n' "$1" "$3" "$2" "$4"
    FAILED=1
  fi
}

request() { # method path [json]
  if [ $# -ge 3 ]; then
    curl -sS -o "$JAR.body" -w '%{http_code}' -X "$1" "$BASE$2" -H 'Content-Type: application/json' -b "$JAR" -c "$JAR" --data "$3"
  else
    curl -sS -o "$JAR.body" -w '%{http_code}' -X "$1" "$BASE$2" -b "$JAR" -c "$JAR"
  fi
}

json_field() { # field  (reads $JAR.body)
  sed -n 's/.*"'"$1"'":"\{0,1\}\([^,"}]*\)"\{0,1\}.*/\1/p' "$JAR.body" | head -1
}

echo "Smoke testing $BASE"

code=$(request GET /api/health); check "GET /api/health" 200 "$code" "$(cat "$JAR.body")"
code=$(request GET /); check "GET / (landing page)" 200 "$code" ""
code=$(request GET /api/public/random-monitors); check "GET /api/public/random-monitors" 200 "$code" "$(cat "$JAR.body")"
code=$(request GET /api/public/stats); check "GET /api/public/stats" 200 "$code" "$(cat "$JAR.body")"
code=$(request GET /api/nope); check "GET /api/nope (json 404)" 404 "$code" "$(cat "$JAR.body")"

code=$(request POST /api/monitors '{"url":"https://example.com/","method":"GET","name":"Smoke anon"}')
check "POST /api/monitors (anonymous)" 201 "$code" "$(cat "$JAR.body")"
ANON_ID=$(json_field id)

code=$(request POST /api/monitors '{"url":"http://127.0.0.1/","method":"GET"}')
check "POST /api/monitors loopback target blocked" 400 "$code" "$(cat "$JAR.body")"
code=$(request POST /api/monitors '{"url":"ftp://example.com/"}')
check "POST /api/monitors ftp scheme rejected" 400 "$code" "$(cat "$JAR.body")"
code=$(request POST /api/monitors '{"url":"https://example.com/","method":"TRACE"}')
check "POST /api/monitors bad method rejected" 400 "$code" "$(cat "$JAR.body")"
code=$(request POST /api/monitors '{"url":"https://example.com/","check_interval_seconds":5}')
check "POST /api/monitors interval < 30 rejected" 400 "$code" "$(cat "$JAR.body")"
code=$(request POST /api/monitors 'not json')
check "POST /api/monitors malformed json" 400 "$code" "$(cat "$JAR.body")"

code=$(request GET "/api/monitors/$ANON_ID"); check "GET /api/monitors/:id (public)" 200 "$code" "$(cat "$JAR.body")"
code=$(request GET "/api/monitors/$ANON_ID/logs"); check "GET /api/monitors/:id/logs (public)" 200 "$code" "$(cat "$JAR.body")"
code=$(request GET /api/monitors/user); check "GET /api/monitors/user unauthenticated" 401 "$code" "$(cat "$JAR.body")"
code=$(request DELETE "/api/monitors/$ANON_ID"); check "DELETE anonymous monitor unauthenticated" 401 "$code" "$(cat "$JAR.body")"

EMAIL="smoke-$STAMP@example.com"
code=$(request POST /api/auth/register "{\"email\":\"$EMAIL\",\"password\":\"correct horse battery\"}")
check "POST /api/auth/register" 201 "$code" "$(cat "$JAR.body")"
code=$(request POST /api/auth/register "{\"email\":\"$EMAIL\",\"password\":\"correct horse battery\"}")
check "POST /api/auth/register duplicate" 409 "$code" "$(cat "$JAR.body")"
code=$(request GET /api/auth/me); check "GET /api/auth/me" 200 "$code" "$(cat "$JAR.body")"

code=$(request POST /api/monitors '{"url":"https://example.com/","method":"HEAD","name":"Smoke owned","alert_email":"ops@example.com","headers":{"X-Ping":"Uptime Medics"}}')
check "POST /api/monitors (registered, private)" 201 "$code" "$(cat "$JAR.body")"
OWNED_ID=$(json_field id)
code=$(request GET /api/monitors/user); check "GET /api/monitors/user" 200 "$code" "$(cat "$JAR.body")"
code=$(request PATCH "/api/monitors/$OWNED_ID" '{"is_active":false,"name":"Smoke owned (paused)"}')
check "PATCH /api/monitors/:id" 200 "$code" "$(cat "$JAR.body")"
code=$(request GET "/api/monitors/$OWNED_ID/logs"); check "GET /api/monitors/:id/logs (owner)" 200 "$code" "$(cat "$JAR.body")"
code=$(request DELETE "/api/monitors/$OWNED_ID"); check "DELETE /api/monitors/:id (owner)" 204 "$code" "$(cat "$JAR.body")"
code=$(request GET "/api/monitors/$OWNED_ID"); check "GET deleted monitor" 404 "$code" "$(cat "$JAR.body")"

code=$(request POST /api/auth/logout); check "POST /api/auth/logout" 204 "$code" ""
code=$(request GET /api/auth/me); check "GET /api/auth/me after logout" 401 "$code" "$(cat "$JAR.body")"
code=$(request POST /api/auth/login "{\"email\":\"$EMAIL\",\"password\":\"wrong password\"}")
check "POST /api/auth/login wrong password" 401 "$code" "$(cat "$JAR.body")"
code=$(request POST /api/auth/login "{\"email\":\"$EMAIL\",\"password\":\"correct horse battery\"}")
check "POST /api/auth/login" 200 "$code" "$(cat "$JAR.body")"

rm -f "$JAR.body"
if [ "$FAILED" -eq 0 ]; then echo "All smoke tests passed."; else echo "Some smoke tests FAILED."; exit 1; fi
