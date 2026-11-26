//! Rowglow core library: database drivers, local storage, licensing and the
//! RPC dispatcher. Used by the Tauri app (`main.rs`) and by the dev-only HTTP
//! bridge (`bin/rowglow-bridge.rs`) that lets the UI run in a normal browser.

pub mod ai;
pub mod crashlog;
pub mod discovery;
pub mod drivers;
pub mod entra;
pub mod error;
pub mod export;
pub mod import;
pub mod license;
pub mod model;
pub mod os;
pub mod rpc;
pub mod safety;
pub mod secrets;
pub mod ssh;
pub mod state;
pub mod store;
pub mod util;

/// Must run once at startup, before any TLS connection is made.
pub fn init_crypto() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}
