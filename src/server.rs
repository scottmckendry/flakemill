//! HTTP server exposing the published lock, signature, and manifest.

use std::path::Path;

use axum::response::IntoResponse;

fn serve_file(dir: &Path, name: &str) -> axum::response::Response {
    match std::fs::read(dir.join(name)) {
        Ok(bytes) => (
            [(axum::http::header::CONTENT_TYPE, "application/octet-stream")],
            bytes,
        )
            .into_response(),
        Err(_) => (axum::http::StatusCode::NOT_FOUND, "not yet published\n").into_response(),
    }
}

pub fn build_router(serve_dir: &std::path::Path) -> axum::Router {
    let serve_dir = std::sync::Arc::new(serve_dir.to_path_buf());
    let file_route = |path: &'static str| {
        let dir = serve_dir.clone();
        axum::routing::get(move || {
            let dir = dir.clone();
            async move { serve_file(&dir, path) }
        })
    };
    axum::Router::new()
        .route("/healthz", axum::routing::get(|| async { "ok" }))
        .route("/flake.lock", file_route("flake.lock"))
        .route("/flake.lock.minisig", file_route("flake.lock.minisig"))
        .route("/manifest.json", file_route("manifest.json"))
        .layer(tower_http::trace::TraceLayer::new_for_http())
}
