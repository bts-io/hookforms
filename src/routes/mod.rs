//! HTTP routes. Each file owns its paths and exposes `router()`, merged in
//! `app.rs`, so later steps only edit their own file.
//!
//! | file              | Python source                      | step |
//! |-------------------|------------------------------------|------|
//! | `root.rs`         | `main.py` (`/`, `/health`)         | 1    |
//! | `keys.rs`         | `routers/auth.py`                  | 2    |
//! | `inboxes.rs`      | `routers/webhooks.py` (inbox CRUD) | 2    |
//! | `hooks.rs`        | `routers/webhooks.py` (receive)    | 3    |
//! | `events.rs`       | `routers/webhooks.py` (events)     | 3    |
//! | `channels.rs`     | `routers/channels.py` (channels)   | 4    |
//! | `email_config.rs` | `routers/channels.py` (provider)   | 5    |
//!
//! Responses use `envelope.rs` (`{"data": ...}` and the paginated form).

pub mod channels;
pub mod email_config;
pub mod envelope;
pub mod events;
pub mod hooks;
pub mod inboxes;
pub mod keys;
pub mod root;
