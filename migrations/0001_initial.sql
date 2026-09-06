-- Uptime Medics initial schema (PRD section 3) + server-side sessions.
-- PRAGMAs (WAL, synchronous=NORMAL, foreign_keys=ON, busy_timeout=5000) are applied per
-- connection from src/db.rs because they cannot run inside a migration transaction.

CREATE TABLE users (
    id TEXT PRIMARY KEY,                       -- UUIDv4
    email TEXT UNIQUE NOT NULL,
    password_hash TEXT NOT NULL,               -- argon2id PHC string
    created_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP
);

-- Server-side session records; the JWT in the cookie carries the session id so that
-- POST /api/auth/logout can genuinely invalidate a session.
CREATE TABLE sessions (
    id TEXT PRIMARY KEY,                       -- UUIDv4 (JWT `sid` claim)
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    created_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
    expires_at DATETIME NOT NULL
);

CREATE TABLE monitors (
    id TEXT PRIMARY KEY,                       -- UUIDv4
    user_id TEXT NULL REFERENCES users(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    url TEXT NOT NULL,
    method TEXT NOT NULL CHECK(method IN ('HEAD', 'GET', 'POST', 'PATCH', 'PUT', 'DELETE', 'OPTIONS')),
    headers TEXT NULL,                         -- JSON object string of header name -> value
    body TEXT NULL,                            -- raw request payload
    check_interval_seconds INTEGER NOT NULL DEFAULT 60 CHECK(check_interval_seconds >= 30),
    timeout_seconds INTEGER NOT NULL DEFAULT 10 CHECK(timeout_seconds <= 60),
    expected_status_code INTEGER NOT NULL DEFAULT 200,
    alert_email TEXT NULL,
    is_anonymous BOOLEAN NOT NULL DEFAULT 1,
    is_active BOOLEAN NOT NULL DEFAULT 1,
    current_status TEXT NOT NULL DEFAULT 'PENDING' CHECK(current_status IN ('UP', 'DOWN', 'PENDING')),
    consecutive_failures INTEGER NOT NULL DEFAULT 0,
    last_checked_at DATETIME NULL,
    created_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE check_logs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    monitor_id TEXT NOT NULL REFERENCES monitors(id) ON DELETE CASCADE,
    status_code INTEGER NULL,
    response_time_ms INTEGER NOT NULL,
    is_successful BOOLEAN NOT NULL,
    error_message TEXT NULL,
    checked_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE INDEX idx_monitors_next_check ON monitors(is_active, last_checked_at);
CREATE INDEX idx_monitors_anonymous ON monitors(is_anonymous, is_active);
CREATE INDEX idx_monitors_user ON monitors(user_id);
CREATE INDEX idx_logs_monitor_time ON check_logs(monitor_id, checked_at DESC);
CREATE INDEX idx_sessions_user ON sessions(user_id);
