//! Router assembly. Each route module exposes `router()` and is merged here.
//!
//! Layer order matches Starlette's: unhandled-error stripping (Starlette's
//! `ServerErrorMiddleware`, outermost) > rate limit > size limit > CORS > routes.

use axum::Router;
use axum::middleware::{from_fn, from_fn_with_state};

use crate::cors::cors;
use crate::error;
use crate::middleware::{rate_limit, size_limit, unhandled_error};
use crate::routes;
use crate::state::AppState;

pub fn build(state: AppState) -> Router {
    let routes: Router = Router::new()
        .merge(routes::root::router())
        .merge(routes::keys::router())
        .merge(routes::inboxes::router())
        .merge(routes::hooks::router())
        .merge(routes::events::router())
        .merge(routes::channels::router())
        .merge(routes::email_config::router())
        .fallback(error::not_found)
        .method_not_allowed_fallback(error::method_not_allowed)
        .with_state(state.clone());
    // The middleware wraps the whole router as one service: `Router::layer`
    // would wrap each route, where axum adds `Allow` to whatever the method
    // router's fallback answers (a CORS preflight included).
    Router::new()
        .fallback_service(routes)
        .layer(from_fn_with_state(state.clone(), cors))
        .layer(from_fn(size_limit))
        .layer(from_fn_with_state(state, rate_limit))
        .layer(from_fn(unhandled_error))
}
