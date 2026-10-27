-- Mirrors Alembic revision 0002 of the Python version.

CREATE TABLE notification_channels (
    id         TEXT PRIMARY KEY NOT NULL,
    inbox_id   TEXT NOT NULL REFERENCES webhook_inboxes (id) ON DELETE CASCADE,
    -- email, discord, slack, teams, telegram, ntfy, webhook
    type       TEXT NOT NULL,
    label      TEXT,
    config     TEXT NOT NULL DEFAULT '{}',
    is_active  INTEGER DEFAULT 1,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f', 'now') || '000Z')
);

CREATE INDEX ix_notification_channels_inbox_id ON notification_channels (inbox_id);
CREATE INDEX ix_notification_channels_type ON notification_channels (type);

CREATE TABLE email_providers (
    id         TEXT PRIMARY KEY NOT NULL,
    -- NULL = global default provider
    inbox_id   TEXT UNIQUE REFERENCES webhook_inboxes (id) ON DELETE CASCADE,
    -- gmail, resend, sendgrid, smtp
    type       TEXT NOT NULL,
    config     TEXT NOT NULL DEFAULT '{}',
    is_active  INTEGER DEFAULT 1,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f', 'now') || '000Z')
);
