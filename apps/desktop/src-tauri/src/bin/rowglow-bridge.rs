//! Development bridge: serves the same RPC dispatcher over HTTP on
//! 127.0.0.1:1425 so the UI can run (and be tested) in a regular browser via
//! `pnpm dev`. Only accepts requests from the Vite dev server origin.
//! Build: `cargo run --features bridge --bin rowglow-bridge`

use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode};
use axum::routing::post;
use axum::{Json, Router};
use rowglow_lib::state::AppState;
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;
use tower_http::cors::CorsLayer;

#[derive(Deserialize)]
struct Req {
    method: String,
    #[serde(default)]
    params: Value,
}

const ALLOWED: [&str; 2] = ["http://localhost:1420", "http://127.0.0.1:1420"];

async fn handle(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<Req>,
) -> (StatusCode, Json<Value>) {
    let origin = headers
        .get("origin")
        .and_then(|o| o.to_str().ok())
        .unwrap_or("");
    if !origin.is_empty() && !ALLOWED.contains(&origin) {
        return (StatusCode::FORBIDDEN, Json(json!({ "error": { "kind": "internal", "message": "origin not allowed" } })));
    }
    match rowglow_lib::rpc::dispatch(&state, &req.method, req.params).await {
        Ok(v) => (StatusCode::OK, Json(json!({ "ok": v }))),
        Err(e) => (StatusCode::OK, Json(json!({ "error": e }))),
    }
}

#[tokio::main]
async fn main() {
    rowglow_lib::init_crypto();
    let data_dir = std::env::var("ROWGLOW_DATA_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| {
            dirs::data_dir()
                .unwrap_or_else(std::env::temp_dir)
                .join("app.rowglow.desktop-dev")
        });
    let state = Arc::new(AppState::new(data_dir.clone(), true).expect("init state"));
    tokio::spawn(state.clone().background_tasks());
    let cors = CorsLayer::new()
        .allow_origin(ALLOWED.map(|o| o.parse::<HeaderValue>().unwrap()))
        .allow_methods([Method::POST])
        .allow_headers([axum::http::header::CONTENT_TYPE]);
    let app = Router::new()
        .route("/rpc", post(handle))
        .layer(cors)
        .with_state(state);
    let addr = "127.0.0.1:1425";
    let listener = tokio::net::TcpListener::bind(addr).await.expect("bind 127.0.0.1:1425");
    println!("Rowglow dev bridge on http://{addr} (data: {})", data_dir.display());
    axum::serve(listener, app).await.expect("serve");
}
