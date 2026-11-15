//! HTTP server exposing the published lock, signature, manifest, and UI.

use std::path::Path;

use axum::response::IntoResponse;

use crate::config::Cfg;
use crate::ui::{index_html, ui_json, UiState};

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

pub fn build_router(serve_dir: &std::path::Path, cfg: &Cfg, ui: UiState) -> axum::Router {
    let serve_dir = std::sync::Arc::new(serve_dir.to_path_buf());
    let file_route = |path: &'static str| {
        let dir = serve_dir.clone();
        axum::routing::get(move || {
            let dir = dir.clone();
            async move { serve_file(&dir, path) }
        })
    };
    let cfg = std::sync::Arc::new(cfg.clone());
    axum::Router::new()
        .route(
            "/",
            axum::routing::get(|| async {
                (
                    [(axum::http::header::CONTENT_TYPE, "text/html; charset=utf-8")],
                    index_html().to_string(),
                )
            }),
        )
        .route(
            "/ui.json",
            axum::routing::get({
                let cfg = cfg.clone();
                move || {
                    let cfg = cfg.clone();
                    async move {
                        let body = ui_json(&ui, &cfg);
                        (
                            [(axum::http::header::CONTENT_TYPE, "application/json")],
                            body.to_string(),
                        )
                            .into_response()
                    }
                }
            }),
        )
        .route("/healthz", axum::routing::get(|| async { "ok" }))
        .route("/flake.lock", file_route("flake.lock"))
        .route("/flake.lock.minisig", file_route("flake.lock.minisig"))
        .route("/manifest.json", file_route("manifest.json"))
        .route(
            "/logs/{host}",
            axum::routing::get({
                let dir = serve_dir.clone();
                move |path: axum::extract::Path<String>| {
                    let dir = dir.clone();
                    async move {
                        // Only simple filenames under serve_dir/logs, never
                        // anything that could escape the directory.
                        let host = path.0;
                        let safe = !host.is_empty()
                            && !host.split('.').any(|seg| {
                                seg.is_empty()
                                    || seg == ".."
                                    || seg.contains('/')
                                    || seg.contains('\\')
                            })
                            && host.chars().all(|c| {
                                c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.'
                            })
                            && host.ends_with(".log");
                        if !safe {
                            return (axum::http::StatusCode::BAD_REQUEST, "bad log name\n")
                                .into_response();
                        }
                        let mut resp = serve_file(&dir.join("logs"), &host);
                        resp.headers_mut().insert(
                            axum::http::header::CONTENT_TYPE,
                            "text/plain; charset=utf-8".parse().unwrap(),
                        );
                        resp
                    }
                }
            }),
        )
        .layer(tower_http::trace::TraceLayer::new_for_http())
}
