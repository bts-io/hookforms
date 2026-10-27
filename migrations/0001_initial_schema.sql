-- Mirrors Alembic revision 0001 of the Python version.
-- UUIDs are stored as lowercase hyphenated TEXT.
-- Timestamps are UTC RFC 3339 TEXT with six fractional digits and a Z suffix
-- (for example 2026-10-27T15:28:02.123456Z), so text order equals time order.
-- Booleans are INTEGER 0 or 1. Postgres ARRAY and JSON columns are JSON TEXT.

CREATE TABLE api_keys (
    id           TEXT PRIMARY KEY NOT NULL,
    name         TEXT NOT NULL,
    key_hash     TEXT NOT NULL UNIQUE,
    key_prefix   TEXT,
    scopes       TEXT DEFAULT '[]',
    is_active    INTEGER DEFAULT 1,
    last_used_at TEXT,
    created_at   TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f', 'now') || '000Z'),
    updated_at   TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f', 'now') || '000Z')
);

CREATE INDEX ix_api_keys_key_prefix ON api_keys (key_prefix);

CREATE TABLE webhook_inboxes (
    id                   TEXT PRIMARY KEY NOT NULL,
    slug                 TEXT NOT NULL UNIQUE,
    description          TEXT,
    forward_url          TEXT,
    notify_email         TEXT,
    email_subject_prefix TEXT,
    sender_name          TEXT,
    turnstile_secret     TEXT,
    is_active            INTEGER DEFAULT 1,
    created_at           TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f', 'now') || '000Z'),
    updated_at           TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f', 'now') || '000Z')
);

CREATE INDEX ix_webhook_inboxes_slug ON webhook_inboxes (slug);

CREATE TABLE webhook_events (
    id           TEXT PRIMARY KEY NOT NULL,
    inbox_id     TEXT NOT NULL REFERENCES webhook_inboxes (id) ON DELETE CASCADE,
    method       TEXT NOT NULL,
    headers      TEXT DEFAULT '{}',
    body         TEXT,
    query_params TEXT DEFAULT '{}',
    source_ip    TEXT,
    received_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f', 'now') || '000Z')
);

CREATE INDEX ix_webhook_events_inbox_id ON webhook_events (inbox_id);
CREATE INDEX ix_webhook_events_received_at ON webhook_events (received_at);
