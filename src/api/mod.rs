//! HTTP delivery layer: router, handlers, extractors, middleware and error mapping.

pub mod assets;
pub mod dto;
pub mod error;
pub mod extractors;
pub mod handlers;
pub mod middleware;

use axum::http::{header, HeaderValue, Method, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::Router;
use tower_http::cors::{AllowOrigin, CorsLayer};
use tower_http::trace::TraceLayer;

use crate::state::AppState;

pub fn router(state: AppState) -> Router {
    let api = Router::new()
        .route("/health", get(handlers::public::health))
        .route(
            "/public/random-monitors",
            get(handlers::public::random_monitors),
        )
        .route("/public/stats", get(handlers::public::stats))
        .route("/auth/register", post(handlers::auth::register))
        .route("/auth/login", post(handlers::auth::login))
        .route("/auth/logout", post(handlers::auth::logout))
        .route("/auth/me", get(handlers::auth::me))
        .route("/monitors", post(handlers::monitors::create))
        .route("/monitors/user", get(handlers::monitors::list_mine))
        .route(
            "/monitors/{id}",
            get(handlers::monitors::get_one)
                .patch(handlers::monitors::update)
                .delete(handlers::monitors::delete),
        )
        .route("/monitors/{id}/logs", get(handlers::monitors::logs))
        .route("/monitors/{id}/history", get(handlers::monitors::history))
        .fallback(api_not_found);

    let mut router = Router::new();
    for &(route, page, title) in assets::PAGES {
        if route == "/" {
            router = router.route(route, get(assets::serve_landing));
            continue;
        }
        router = router.route(
            route,
            get(move || async move { assets::serve_page(page, title) }),
        );
    }
    let mut router = router
        .route("/index.html", get(assets::serve_landing))
        .route("/robots.txt", get(assets::serve_robots))
        .route("/ads.txt", get(assets::serve_ads_txt))
        .route("/sitemap.xml", get(assets::serve_sitemap))
        .route("/.well-known/{*path}", get(assets::serve_agent_doc))
        .route("/auth.md", get(assets::serve_agent_doc))
        .route("/llms.txt", get(assets::serve_agent_doc))
        .nest("/api", api)
        .fallback(assets::asset_or_404)
        .layer(axum::middleware::from_fn(middleware::security_headers))
        .layer(TraceLayer::new_for_http());

    if !state.config.cors_allowed_origins.is_empty() {
        let origins: Vec<HeaderValue> = state
            .config
            .cors_allowed_origins
            .iter()
            .filter_map(|o| HeaderValue::from_str(o).ok())
            .collect();
        let cors = CorsLayer::new()
            .allow_origin(AllowOrigin::list(origins))
            .allow_methods([
                Method::GET,
                Method::POST,
                Method::PATCH,
                Method::DELETE,
                Method::OPTIONS,
            ])
            .allow_headers([header::CONTENT_TYPE])
            .allow_credentials(true);
        router = router.layer(cors);
    }

    router.with_state(state)
}

async fn api_not_found() -> impl IntoResponse {
    (
        StatusCode::NOT_FOUND,
        axum::Json(serde_json::json!({
            "error": "not_found",
            "message": "No such API endpoint"
        })),
    )
}
