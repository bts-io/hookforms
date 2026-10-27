-- The Postgres schema the Python version's Alembic migrations 0001 and 0002
-- create (taken from `pg_dump --schema-only` after `alembic upgrade head`).
CREATE TABLE alembic_version (
    version_num character varying(32) NOT NULL PRIMARY KEY
);
INSERT INTO alembic_version VALUES ('0002');

CREATE TABLE api_keys (
    id uuid NOT NULL PRIMARY KEY,
    name character varying(255) NOT NULL,
    key_hash character varying(255) NOT NULL UNIQUE,
    key_prefix character varying(12),
    scopes character varying[] DEFAULT '{}'::character varying[],
    is_active boolean DEFAULT true,
    last_used_at timestamp with time zone,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL
);
CREATE INDEX ix_api_keys_key_prefix ON api_keys USING btree (key_prefix);

CREATE TABLE webhook_inboxes (
    id uuid NOT NULL PRIMARY KEY,
    slug character varying(100) NOT NULL,
    description character varying(500),
    forward_url text,
    notify_email character varying(500),
    email_subject_prefix character varying(200),
    sender_name character varying(200),
    turnstile_secret character varying(200),
    is_active boolean DEFAULT true,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL
);
CREATE UNIQUE INDEX ix_webhook_inboxes_slug ON webhook_inboxes USING btree (slug);

CREATE TABLE webhook_events (
    id uuid NOT NULL PRIMARY KEY,
    inbox_id uuid NOT NULL REFERENCES webhook_inboxes(id) ON DELETE CASCADE,
    method character varying(10) NOT NULL,
    headers json DEFAULT '{}'::json,
    body json,
    query_params json DEFAULT '{}'::json,
    source_ip character varying(45),
    received_at timestamp with time zone DEFAULT now() NOT NULL
);
CREATE INDEX ix_webhook_events_inbox_id ON webhook_events USING btree (inbox_id);
CREATE INDEX ix_webhook_events_received_at ON webhook_events USING btree (received_at);

CREATE TABLE notification_channels (
    id uuid NOT NULL PRIMARY KEY,
    inbox_id uuid NOT NULL REFERENCES webhook_inboxes(id) ON DELETE CASCADE,
    type character varying(50) NOT NULL,
    label character varying(200),
    config json DEFAULT '{}'::json NOT NULL,
    is_active boolean DEFAULT true,
    created_at timestamp with time zone DEFAULT now() NOT NULL
);
CREATE INDEX ix_notification_channels_inbox_id ON notification_channels USING btree (inbox_id);
CREATE INDEX ix_notification_channels_type ON notification_channels USING btree (type);

CREATE TABLE email_providers (
    id uuid NOT NULL PRIMARY KEY,
    inbox_id uuid UNIQUE REFERENCES webhook_inboxes(id) ON DELETE CASCADE,
    type character varying(50) NOT NULL,
    config json DEFAULT '{}'::json NOT NULL,
    is_active boolean DEFAULT true,
    created_at timestamp with time zone DEFAULT now() NOT NULL
);
