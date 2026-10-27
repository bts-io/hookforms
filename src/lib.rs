//! HookForms self-hosted service: receives webhook form submissions and forwards
//! them as notifications. It replaces the Python version (git tag `v0.1.0-python`)
//! and answers the same HTTP API.
//!
//! Module map:
//! - core: `config`, `db`, `error`, `state`, `limiter`, `middleware`,
//!   `security`, `app`, `routes::root`, `routes::envelope`
//! - keys and inboxes: `auth`, `models`, `routes::keys`, `routes::inboxes`
//! - intake and events: `routes::hooks`, `routes::events`
//! - notification channels: `channels/`, `routes::channels`
//! - email providers: `email/`, `routes::email_config`
//! - maintenance: `cleanup`, `import`

pub mod app;
pub mod auth;
pub mod channels;
pub mod cleanup;
pub mod config;
pub mod cors;
pub mod db;
pub mod email;
pub mod error;
pub mod import;
pub mod limiter;
pub mod middleware;
pub mod models;
pub mod routes;
pub mod security;
pub mod state;

pub use app::build as build_app;
pub use state::AppState;
