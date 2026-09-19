# Uptime Medics

Lightweight, high-performance uptime monitoring: a Rust backend (`axum` + `tokio` + `reqwest` +
`sqlx`/SQLite + `lettre`) and a zero-dependency HTML/JavaScript frontend.

* Probe any HTTP endpoint with `HEAD`, `GET`, `POST`, `PATCH`, `PUT`, `DELETE` or `OPTIONS`,
  custom headers, a request payload, a strict timeout and an exact expected status code.
* **No sign-up needed**: anyone can submit a public monitor. The landing page juggles 10 random
  community monitors every 10 seconds alongside system health metrics.
* **Accounts** (email + password, argon2id) own private monitors that can be edited, paused and
  deleted from a dashboard.
* **SMTP alerts** when a monitor fails twice in a row, plus recovery notifications, with
  exponential-backoff retries and flap suppression.
* **Uptime history** for the last 24 hours, 7 days, and 30 days, with SQLite-backed summaries,
  interactive charts, and recent checks for public and private monitors.
* **Free courses** at `/courses`: VPS reliability, cloud resilience, self hosting, Cloudflare
  Tunnel, and Nginx with Certbot on a VPS, including configuration examples and practice tasks.
* **SSRF guardrails**, per-IP rate limiting for anonymous submissions, batched SQLite writes and
  automatic 30-day log retention by default.

## Quick start with Docker

Requirements: Docker with Compose v2.

```bash
cp .env.example .env            # set JWT_SECRET (and SMTP_* if you want email alerts)
docker compose up -d --build    # first build compiles the Rust binary (a few minutes)
open http://localhost:8080      # or the APP_PORT you configured in .env
```

Useful commands:

```bash
docker compose logs -f uptime-medics          # follow logs
docker compose ps                             # health status (healthcheck built into the binary)
docker compose exec uptime-medics uptime-medics smtp-test you@example.com   # verify SMTP config
docker compose down                           # stop (data is kept in the `uptime_data` volume)
docker compose down -v                        # stop and delete all data
```

The SQLite database lives in the named volume `uptime_data` (mounted at `/data`). To use a bind
mount instead, replace the volume with e.g. `./data:/data` and make sure the directory is writable
by uid `10001` (the container runs as the unprivileged `medic` user).

If port 8080 is already used on your machine, set `APP_PORT=8787` (any free port) in `.env`.

### Testing alert emails locally

```bash
docker compose --profile dev up -d --build    # also starts Mailpit (web inbox on :8025)
```

Then put these in `.env` and restart the app: `SMTP_HOST=mailpit`, `SMTP_PORT=1025`,
`SMTP_TLS=none`, `SMTP_FROM_ADDRESS=alerts@uptime-medics.local`. Every alert shows up at
http://localhost:8025.

### Running the smoke test

```bash
scripts/smoke_test.sh http://localhost:8080
```

### Makefile shortcuts

`make help` lists every task. The most useful ones:

| Target | What it does |
| --- | --- |
| `make up` / `make up-dev` | Build and start the stack (`up-dev` adds Mailpit). Honours `APP_PORT`. |
| `make down` / `make down-clean` | Stop the stack, optionally deleting the data volume. |
| `make logs`, `make ps` | Follow logs, show container health. |
| `make smoke` | Run the API smoke test against `BASE_URL`. |
| `make smtp-test TO=you@example.com` | Send a test email through the running container. |
| `make test`, `make test-unit`, `make test-integration` | Run the Rust test suites. |
| `make check` | Formatting check, clippy with warnings as errors, and tests. |

When `cargo` is not installed, every Rust task transparently runs inside the official `rust`
Docker image with cached volumes, so only Docker is required.

## Configuration

Everything is configured through environment variables (a `.env` file is loaded automatically).

| Variable | Default | Description |
| --- | --- | --- |
| `HOST` / `PORT` | `0.0.0.0` / `8080` | Bind address of the HTTP server. |
| `DATABASE_URL` | `sqlite://data/uptime_medics.db` | SQLite file (created on first start, WAL mode, `busy_timeout` 5000 ms). |
| `JWT_SECRET` | random per start | Secret used to sign session cookies. **Set it in production.** |
| `SESSION_TTL_HOURS` | `168` | Session lifetime. |
| `PUBLIC_BASE_URL` | – | Public URL; used for links in alert emails. `https://` enables the `Secure` cookie flag. |
| `COOKIE_SECURE` | derived | Force the `Secure` flag on the session cookie on/off. |
| `TRUST_PROXY_HEADERS` | `false` | Trust `X-Forwarded-For` / `X-Real-IP` for rate limiting (behind a reverse proxy only). |
| `SMTP_HOST`, `SMTP_PORT` | – / `587` | SMTP server. Alerts are disabled while `SMTP_HOST` is empty. |
| `SMTP_USERNAME`, `SMTP_PASSWORD` | – | SMTP credentials (`SMTP_USER` / `SMTP_PASS` are accepted as aliases). |
| `SMTP_FROM_ADDRESS` | – | Sender, e.g. `Uptime Medics <alerts@example.com>`. Required when `SMTP_HOST` is set. |
| `SMTP_TLS` | `starttls` (`tls` for port 465) | `starttls`, `tls` (implicit) or `none`. |
| `PROBE_CONCURRENCY` | `100` | Maximum simultaneous probes. |
| `SCHEDULER_TICK_MS` | `1000` | How often due monitors are collected. |
| `LOG_FLUSH_INTERVAL_SECONDS` / `LOG_BATCH_SIZE` | `5` / `100` | Check logs are written in batch transactions of up to `LOG_BATCH_SIZE` rows every flush interval. |
| `ANON_LOG_RETENTION_HOURS` | `720` | Daily cleanup deletes older anonymous checks. Keep at least 720 hours for 30-day history. |
| `REGISTERED_LOG_RETENTION_DAYS` | `30` | Same for registered monitors (`0` keeps them forever). |
| `ANON_RATE_LIMIT_PER_HOUR` | `10` | Anonymous monitor creations allowed per client IP (token bucket). |
| `ALLOW_PRIVATE_TARGETS` | `false` | Permit private/loopback targets (disables SSRF protection; LAN self-hosting only). |
| `CORS_ALLOWED_ORIGINS` | – | Comma-separated browser origins allowed to call the API cross-origin. |
| `RUST_LOG` | `info` | Log filter, e.g. `debug` or `uptime_medics=debug,tower_http=debug`. |

## API

All endpoints speak JSON. Errors look like `{"error": "bad_request", "message": "..."}`.

| Method | Path | Auth | Description |
| --- | --- | --- | --- |
| `POST` | `/api/auth/register` | – | `{ "email", "password" }` → creates the account and a session cookie. |
| `POST` | `/api/auth/login` | – | Same body; returns an HTTP-only session cookie. |
| `POST` | `/api/auth/logout` | cookie | Invalidates the server-side session and clears the cookie. |
| `GET` | `/api/auth/me` | cookie | Current user. |
| `GET` | `/api/public/random-monitors` | – | 10 random active public monitors with latest latency. |
| `GET` | `/api/public/stats` | – | Landing-page metrics (cached 10 s). |
| `POST` | `/api/monitors` | optional | Create a monitor (anonymous and public when unauthenticated, private when logged in). |
| `GET` | `/api/monitors/user` | cookie | Monitors owned by the current user. |
| `GET` | `/api/monitors/:id` | optional | Monitor details (public monitors are redacted: no headers, body or alert email). |
| `PATCH` | `/api/monitors/:id` | cookie | Edit an owned monitor (`is_active` pauses/resumes). |
| `DELETE` | `/api/monitors/:id` | cookie | Delete an owned monitor and its history. |
| `GET` | `/api/monitors/:id/logs` | optional | Last 50 checks (public monitors or owner). |
| `GET` | `/api/monitors/:id/history?range=24h` | optional | Full-period uptime summary, chart buckets and latest 50 checks in that period. `range`: `24h` (default), `7d`, or `30d`. Public monitors or owner only. |
| `GET` | `/api/health` | – | Liveness/readiness (checks the database). |

Create-monitor payload (only `url` is required):

```json
{
  "name": "Production API Health",
  "url": "https://api.example.com/v1/health",
  "method": "POST",
  "headers": {"Content-Type": "application/json", "X-Ping": "Uptime Medics"},
  "body": "{\"check\": true}",
  "check_interval_seconds": 60,
  "timeout_seconds": 10,
  "expected_status_code": 200,
  "alert_email": "ops-alerts@example.com"
}
```

`headers` may be given as a JSON object or as a string containing one. Limits: interval 30 s to
24 h, timeout 1–60 s, expected status 100–599, body ≤ 64 KB, ≤ 25 headers.

### Uptime history and courses

Open a community monitor, a recent submission, or **History** on the dashboard to switch between
24 hours, 7 days, and 30 days. Each view includes uptime, total/failed checks, average latency,
and a chart whose intervals can be selected with the mouse or keyboard.

The history API aggregates **all recorded checks** in the selected rolling UTC window, not just
the latest 50 shown in the detail table. Uptime is `100 × successful checks / total checks`;
latency is averaged across all checks, including failures. It is a sampled check success rate,
not a measurement of exact outage duration. Missing checks and pauses do not count as success
or failure. Empty periods have `null` uptime and latency, displayed as “No data”.

Responses contain `range`, `start`, `end`, `bucket_seconds`, `retention_hours`, `summary`,
`buckets`, and `recent_checks`. The 24-hour, 7-day and 30-day charts use 24 one-hour, 28 six-hour,
and 30 one-day buckets respectively. Window and bucket boundaries are `(start, end]`. Summary
and bucket fields include `total_checks`, `successful_checks`, `failed_checks`, `uptime_percent`,
`avg_response_time_ms`, `first_checked_at`, and `last_checked_at`. Timestamps are UTC in the API
and displayed in the browser's local time. Unsupported ranges return a JSON `400` error.

History uses the existing indexed SQLite `check_logs` table and survives app restarts; no new
database or schema migration is needed. Both public and private monitors retain 30 days by
default. Existing explicit retention settings are respected; a shorter setting is called out in
the history view. New monitors accumulate history from their first check, and previously pruned
data cannot be recovered. Fresh checks appear after the normal log flush (5 seconds by default).

The public **Courses** page includes five courses, each with three lessons, prerequisites,
practice tasks, and official reference links. Course completion is optional and remembered in
the current browser; monitoring history is stored on the server in SQLite.

## How it works

* **Scheduler** – a `tokio` interval loop selects monitors whose `last_checked_at` is older than
  their interval and probes them concurrently (bounded by a semaphore, one in-flight probe per
  monitor) through a shared `reqwest` client (rustls, keep-alive pooling). A probe succeeds only if
  the connection, TLS handshake and response headers complete within the timeout and the status
  code equals `expected_status_code`. Redirects are followed (max 5 hops) unless a 3xx status is
  expected.
* **Incidents** – a monitor becomes `DOWN` after 2 consecutive failures and `UP` on the next
  success. Each transition is emailed to `alert_email` (if set) with up to 4 delivery attempts
  (2 s, 4 s, 8 s backoff). Monitors that change state more than 4 times in 30 minutes are
  considered flapping and their notifications are suppressed until they settle. Editing a
  monitor's URL, method, headers, body or expected status resets its failure streak and probes it
  immediately; its status is kept, so a DOWN monitor still gets a recovery notification.
* **Storage** – SQLite in WAL mode with `synchronous=NORMAL`, `foreign_keys=ON` and a 5 s busy
  timeout. Check logs are buffered in memory and written in batch transactions (up to 100 rows
  every 5 s). A daily job prunes old logs and expired sessions and runs `PRAGMA optimize`.
* **Security** – argon2id password hashes; JWT session cookies (`HttpOnly`, `SameSite=Lax`,
  optional `Secure`) backed by server-side session rows so logout really invalidates them.
  SSRF protection blocks loopback, private, link-local (incl. `169.254.169.254`), CGNAT,
  multicast, reserved and IPv6-mapped ranges at URL validation, at DNS resolution time (a custom
  resolver filters answers on every probe, defeating DNS rebinding) and on every redirect hop.
  Anonymous creations are limited to 10 per hour per IP. Public monitor details never expose
  custom headers, payloads or alert emails. Security headers and a CSP (`script-src 'self'`, no
  inline scripts) are set on all responses.
* **Frontend** – four server-composed pages (landing `/`, dashboard `/dashboard`, login
  `/login`, courses `/courses`) built from `static/html/layout.html`, one fragment per page and shared partials
  (header, footer, monitor form, modals). Plain CSS files and vanilla ES modules, no frameworks,
  no build step. The whole `static/` tree is embedded into the binary for release builds; debug
  builds read it from disk so edits show up on refresh. The community list refreshes every 10 s;
  the dashboard offers pause/edit/delete and per-monitor uptime history for 24 hours, 7 days,
  and 30 days.

## Development

```bash
cargo run                     # http://localhost:8080 with ./data/uptime_medics.db
ALLOW_PRIVATE_TARGETS=true cargo run   # allow monitoring localhost while developing
cargo test                    # unit tests + integration tests
```

### Tests

Unit tests live next to the code (`#[cfg(test)]` modules in `src/`). Integration tests live in
`tests/` and drive the real application in-process: each test starts the HTTP server on a random
port with its own temporary SQLite database, a local HTTP target to probe, and a fake SMTP server
that captures the emails the app sends.

| File | Covers |
| --- | --- |
| `tests/common/mod.rs` | The harness: `TestApp`, cookie-aware `TestClient`, `TargetServer`, `FakeSmtp`. |
| `tests/api_auth.rs` | Register/login/logout, session invalidation, cookie flags, forged tokens. |
| `tests/api_monitors.rs` | Creation defaults and validation, public redaction, ownership, PATCH/DELETE, SSRF blocks, rate limiting. |
| `tests/api_public.rs` | Health, security headers, the random pool, stats. |
| `tests/frontend.rs` | Page composition (layout + partials), every referenced asset and module import resolves, asset types, `html/` never exposed. |
| `tests/history.rs` | Full-window aggregation, UTC boundaries, empty periods, visibility, retention and SQLite persistence. |
| `tests/scheduler_alerts.rs` | Probe loop, incident detection (2 failures), alert and recovery emails, flap suppression, pause/resume, batching, shutdown flush, cleanup. |
| `tests/security.rs` | SSRF block lists and DNS filtering, rate limiter, argon2/JWT/cookie primitives. |
| `tests/domain.rs` | Incident state machine, entity helpers and serialisation, every validation rule. |
| `tests/infrastructure.rs` | SQLite init/WAL/migrations/constraints, SMTP delivery and backoff, HTTP client redirect policies. |

```bash
make test                     # everything
make test-integration         # only tests/
cargo test --test scheduler_alerts   # one file
```

Subcommands: `uptime-medics smtp-test <recipient>` sends a test email with the configured SMTP
settings; `uptime-medics healthcheck` exits 0 when the local server is healthy.

## Project layout

The code is organised in layers; each layer only depends on the ones below it.

```
src/
├── main.rs                  start-up, CLI subcommands (smtp-test, healthcheck), graceful shutdown
├── config.rs                environment configuration
├── state.rs                 AppState: the composition root that wires everything together
├── api/                     HTTP delivery layer (axum)
│   ├── mod.rs               router
│   ├── assets.rs            embedded static files + page composition (layout, pages, partials)
│   ├── handlers/            auth.rs, monitors.rs, public.rs
│   ├── dto.rs               request payloads
│   ├── extractors.rs        AuthUser / OptionalAuthUser, cookie and client-IP helpers
│   ├── middleware.rs        security headers + CSP
│   └── error.rs             ApiError -> JSON responses
├── services/                application logic
│   ├── probe.rs             HTTP probe execution
│   ├── scheduler.rs         probe loop, batched log writer, daily cleanup
│   ├── alerts.rs            DOWN/recovery notifications, flap detection
│   ├── history.rs           SQLite aggregation for rolling uptime windows
│   └── session.rs           session create / resolve / revoke
├── security/                cross-cutting protection
│   ├── auth.rs              argon2id hashing, JWT tokens, cookie attributes
│   ├── ssrf.rs              URL validation, IP block lists, filtering DNS resolver, redirect policy
│   └── ratelimit.rs         per-IP token bucket
├── infrastructure/          adapters to the outside world
│   ├── db.rs                SQLite pool + embedded migrations (migrations/)
│   ├── http_client.rs       shared reqwest client for probes
│   └── mailer.rs            lettre SMTP transport with backoff
└── domain/                  entities and rules, no I/O
    ├── monitor.rs           Monitor and its read models, statuses, allowed methods
    ├── user.rs              User, CurrentUser, UserResponse
    ├── check_log.rs         CheckLog, CheckLogInsert
    ├── history.rs           history ranges, summaries and chart buckets
    ├── stats.rs             StatsResponse
    └── validation.rs        input rules (methods, intervals, headers, emails, passwords)

src/lib.rs                   library root (the binary and the tests share it)
tests/                       integration tests (see Tests below)
migrations/                  SQL schema
scripts/smoke_test.sh        end-to-end API smoke test
Makefile                     developer tasks (`make help`)

static/                      frontend, embedded into the binary (src/api/assets.rs)
├── html/
│   ├── layout.html          document shell: <head>, header, {{content}}, footer, page module
│   ├── pages/               landing, dashboard, login, courses (one HTML fragment per page)
│   └── partials/            header, footer, monitor-form, logs-modal, edit-modal ({{> name}})
├── css/
│   ├── base.css             design tokens, reset, header/footer/layout
│   ├── components.css       cards, forms, buttons, badges, rows, tables, modals, toasts
│   └── pages/               landing.css, dashboard.css, login.css, courses.css
├── js/
│   ├── api.js               fetch wrapper + ApiError
│   ├── ui.js                escaping/formatting, badges, monitor rows, toasts, modals, storage
│   ├── auth.js              current user, header rendering, logout, auth guard
│   ├── monitors.js          shared create form and history entry points
│   ├── history.js           period selection, uptime chart and recent checks
│   └── pages/               landing.js, dashboard.js, login.js, courses.js (entry modules)
└── img/favicon.svg
```

Pages are served at `/`, `/dashboard`, `/login` and `/courses`; files under `css/`, `js/` and `img/` are
served as-is (`html/` fragments are never exposed). Adding a page means adding
`html/pages/<name>.html`, `css/pages/<name>.css`, `js/pages/<name>.js` and one entry in
`PAGES` in `src/api/assets.rs`.
