//! Licensing, update checks and anonymous telemetry.
//!
//! License tokens are `RGL1.<b64url(payload)>.<b64url(ed25519 signature)>`,
//! signed by the Rowglow license server and verified here offline with the
//! embedded public key. Tokens are bound to this device's id.

use crate::error::{AppError, AppResult};
use crate::state::AppState;
use crate::util::now_secs;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine as _;
use ed25519_dalek::{Signature, VerifyingKey};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};

const PUBLIC_KEY_B64: &str = include_str!("../license_public_key.txt");
const REVALIDATE_EVERY: i64 = 7 * 86_400;

pub const FREE_CONNECTIONS: u32 = 3;
pub const FREE_HISTORY: u32 = 50;
pub const FREE_SNIPPETS: u32 = 5;
pub const FREE_IMPORT_ROWS: u32 = 1_000;
pub const FREE_AI_PER_DAY: u32 = 10;
/// History rows kept on disk for every plan. The free plan only *shows* the
/// newest `FREE_HISTORY`, so a license that reads as free for a while (offline,
/// re-activation) never loses history.
pub const MAX_HISTORY: u32 = 50_000;

/// Launch period: every feature is free for everyone until this moment
/// (2027-02-01 00:00 UTC). The website can move or end it: `releases/latest`
/// returns `freeUntil` (UNIX seconds, or null for "no free period"), which the
/// update check caches under `launch.freeUntil`. This default applies until the
/// app has heard from the server.
pub const FREE_UNTIL_DEFAULT: i64 = 1_801_440_000;
const FREE_UNTIL_KEY: &str = "launch.freeUntil";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenPayload {
    pub v: u32,
    pub lid: String,
    #[serde(default)]
    pub email: String,
    #[serde(default)]
    pub name: String,
    pub plan: String,
    pub device_id: String,
    #[serde(default)]
    pub max_devices: u32,
    pub issued_at: i64,
    #[serde(default)]
    pub updates_until: i64,
    #[serde(default)]
    pub expires_at: Option<i64>,
    pub validate_by: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Limits {
    pub connections: Option<u32>,
    pub history: Option<u32>,
    pub snippets: Option<u32>,
    pub table_export: bool,
    /// Rows per CSV import (None = unlimited).
    pub import_rows: Option<u32>,
    /// AI requests per day (None = unlimited).
    pub ai_per_day: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LicenseStatus {
    pub plan: String,
    pub licensed: bool,
    pub email: Option<String>,
    pub name: Option<String>,
    pub key_hint: Option<String>,
    pub updates_until: Option<i64>,
    pub expires_at: Option<i64>,
    pub validate_by: Option<i64>,
    pub last_validated: Option<i64>,
    pub message: Option<String>,
    pub device_id: String,
    pub limits: Limits,
    /// Set while the free launch period is on (`plan` is then "launch").
    pub free_until: Option<i64>,
}

pub fn verify_token(token: &str) -> AppResult<TokenPayload> {
    let bad = || AppError::license("The license token is invalid");
    let mut parts = token.trim().splitn(3, '.');
    let (prefix, payload_b64, sig_b64) = (
        parts.next().ok_or_else(bad)?,
        parts.next().ok_or_else(bad)?,
        parts.next().ok_or_else(bad)?,
    );
    if prefix != "RGL1" {
        return Err(bad());
    }
    let key_bytes = STANDARD.decode(PUBLIC_KEY_B64.trim()).map_err(|_| bad())?;
    let key_arr: [u8; 32] = key_bytes.try_into().map_err(|_| bad())?;
    let key = VerifyingKey::from_bytes(&key_arr).map_err(|_| bad())?;
    let sig_bytes = URL_SAFE_NO_PAD.decode(sig_b64).map_err(|_| bad())?;
    let sig = Signature::from_slice(&sig_bytes).map_err(|_| bad())?;
    let signed = format!("RGL1.{payload_b64}");
    key.verify_strict(signed.as_bytes(), &sig).map_err(|_| bad())?;
    let payload = URL_SAFE_NO_PAD.decode(payload_b64).map_err(|_| bad())?;
    serde_json::from_slice(&payload).map_err(|_| bad())
}

fn hashed_device_id(seed: &str) -> String {
    let digest = Sha256::digest(format!("rowglow:{seed}").as_bytes());
    crate::util::hex(&digest)[..32].to_string()
}

/// This computer's ID, derived from the OS machine ID on every call so that
/// copying app.db to another computer doesn't carry the license along. Only
/// when the OS has no machine ID is a random one stored (and then reused).
pub fn device_id(state: &AppState) -> String {
    if let Ok(uid) = machine_uid::get() {
        if !uid.trim().is_empty() {
            return hashed_device_id(uid.trim());
        }
    }
    if let Ok(Some(id)) = state.store.kv_get("deviceId") {
        return id;
    }
    let id = hashed_device_id(&crate::util::new_id());
    let _ = state.store.kv_set("deviceId", &id);
    id
}

pub fn install_id(state: &AppState) -> String {
    if let Ok(Some(id)) = state.store.kv_get("installId") {
        return id;
    }
    let id = crate::util::new_id();
    let _ = state.store.kv_set("installId", &id);
    id
}

fn free_limits() -> Limits {
    Limits {
        connections: Some(FREE_CONNECTIONS),
        history: Some(FREE_HISTORY),
        snippets: Some(FREE_SNIPPETS),
        table_export: false,
        import_rows: Some(FREE_IMPORT_ROWS),
        ai_per_day: Some(FREE_AI_PER_DAY),
    }
}

fn pro_limits() -> Limits {
    Limits {
        connections: None,
        history: None,
        snippets: None,
        table_export: true,
        import_rows: None,
        ai_per_day: None,
    }
}

fn mask_key(key: &str) -> String {
    let k = key.trim();
    if k.len() > 6 {
        format!("••••-{}", &k[k.len() - 5..])
    } else {
        "••••".into()
    }
}

/// End of the free launch period, or None when there is none (any more).
pub fn free_until(state: &AppState) -> Option<i64> {
    let until = match state.store.kv_get(FREE_UNTIL_KEY).ok().flatten() {
        Some(v) if v == "none" => return None,
        Some(v) => v.parse::<i64>().unwrap_or(FREE_UNTIL_DEFAULT),
        None => FREE_UNTIL_DEFAULT,
    };
    (now_secs() < until).then_some(until)
}

/// Remembers the launch period from a `releases/latest` answer (present on
/// success and on `no_release`). A missing field leaves the cached value alone.
fn remember_free_until(state: &AppState, body: &serde_json::Value) {
    let Some(v) = body.get("freeUntil") else { return };
    let value = match v {
        serde_json::Value::Null => "none".to_string(),
        serde_json::Value::Number(n) => match n.as_i64() {
            Some(t) => t.to_string(),
            None => return,
        },
        _ => return,
    };
    let _ = state.store.kv_set(FREE_UNTIL_KEY, &value);
}

pub fn status(state: &AppState) -> LicenseStatus {
    let device = device_id(state);
    let token = state.store.kv_get("license.token").ok().flatten();
    let key = state.store.kv_get("license.key").ok().flatten();
    let last_validated = state
        .store
        .kv_get("license.lastValidated")
        .ok()
        .flatten()
        .and_then(|v| v.parse::<i64>().ok());
    let launch = free_until(state);
    let free = |message: Option<String>| LicenseStatus {
        // During the launch period everyone gets every feature, without a license.
        plan: if launch.is_some() { "launch".into() } else { "free".into() },
        licensed: false,
        email: None,
        name: None,
        key_hint: None,
        updates_until: None,
        expires_at: None,
        validate_by: None,
        last_validated: None,
        message,
        device_id: device.clone(),
        limits: if launch.is_some() { pro_limits() } else { free_limits() },
        free_until: launch,
    };
    let Some(token) = token else {
        return free(None);
    };
    let payload = match verify_token(&token) {
        Ok(p) => p,
        Err(_) => return free(Some("Stored license could not be verified. Please activate again.".into())),
    };
    if payload.device_id != device {
        return free(Some("This license was activated on a different device.".into()));
    }
    let now = now_secs();
    if let Some(exp) = payload.expires_at {
        if now > exp {
            return free(Some("Your license has expired.".into()));
        }
    }
    // Pro is a perpetual license: being offline never turns it off. The app
    // re-validates about once a week when online, and only an explicit answer
    // from the license server (revoked / deactivated) removes it.
    LicenseStatus {
        plan: payload.plan.clone(),
        licensed: true,
        email: Some(payload.email.clone()),
        name: Some(payload.name.clone()),
        key_hint: key.as_deref().map(mask_key),
        updates_until: Some(payload.updates_until),
        expires_at: payload.expires_at,
        validate_by: Some(payload.validate_by),
        last_validated,
        message: None,
        device_id: device,
        limits: pro_limits(),
        free_until: None,
    }
}

/// Whether this install has a valid license (not just the launch period).
pub fn is_licensed(state: &AppState) -> bool {
    status(state).licensed
}

/// Whether Pro features are available: a license, or the free launch period.
/// Every limit check goes through this.
pub fn has_pro(state: &AppState) -> bool {
    let st = status(state);
    st.licensed || st.free_until.is_some()
}

fn platform() -> &'static str {
    match std::env::consts::OS {
        "macos" => "darwin",
        other => other,
    }
}

async fn api_error(resp: reqwest::Response) -> AppError {
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.unwrap_or(json!({}));
    let message = body["message"]
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| format!("License server returned {status}"));
    let mut e = AppError::license(message);
    if let Some(code) = body["error"].as_str() {
        e.code = Some(code.to_string());
    }
    e
}

/// The license/update server, or an error when this build has none configured.
fn server(state: &AppState) -> AppResult<String> {
    let base = state.api_base();
    if base.is_empty() {
        Err(AppError::license(
            "This build of Rowglow has no license server configured.",
        ))
    } else {
        Ok(base)
    }
}

fn store_token(state: &AppState, token: &str, key: Option<&str>) -> AppResult<()> {
    let payload = verify_token(token)?;
    if payload.device_id != device_id(state) {
        return Err(AppError::license("The license server returned a token for another device"));
    }
    state.store.kv_set("license.token", token)?;
    if let Some(k) = key {
        state.store.kv_set("license.key", k.trim())?;
    }
    state.store.kv_set("license.lastValidated", &now_secs().to_string())?;
    Ok(())
}

pub async fn activate(state: &AppState, key: &str) -> AppResult<LicenseStatus> {
    let key = key.trim().to_uppercase();
    if key.is_empty() {
        return Err(AppError::invalid("Enter a license key"));
    }
    let url = format!("{}/api/v1/licenses/activate", server(state)?);
    let resp = state
        .http
        .post(url)
        .json(&json!({
            "key": key,
            "deviceId": device_id(state),
            "deviceName": crate::os::device_name(),
            "platform": platform(),
            "appVersion": state.app_version,
        }))
        .send()
        .await?;
    if !resp.status().is_success() {
        return Err(api_error(resp).await);
    }
    let body: serde_json::Value = resp.json().await?;
    let token = body["token"]
        .as_str()
        .ok_or_else(|| AppError::license("License server returned no token"))?;
    store_token(state, token, Some(&key))?;
    Ok(status(state))
}

pub async fn refresh(state: &AppState) -> AppResult<LicenseStatus> {
    let Some(token) = state.store.kv_get("license.token")? else {
        return Ok(status(state));
    };
    let url = format!("{}/api/v1/licenses/validate", server(state)?);
    let resp = state.http.post(url).json(&json!({ "token": token })).send().await?;
    if !resp.status().is_success() {
        return Err(api_error(resp).await);
    }
    let body: serde_json::Value = resp.json().await?;
    if body["valid"].as_bool() == Some(true) {
        if let Some(t) = body["token"].as_str() {
            store_token(state, t, None)?;
        }
        Ok(status(state))
    } else {
        let reason = body["reason"].as_str().unwrap_or("invalid").to_string();
        state.store.kv_delete("license.token")?;
        let mut st = status(state);
        st.message = Some(match reason.as_str() {
            "revoked" => "This license has been revoked.".into(),
            "deactivated" => "This device was deactivated from your account.".into(),
            _ => "This license is no longer valid.".into(),
        });
        Ok(st)
    }
}

pub async fn deactivate(state: &AppState) -> AppResult<LicenseStatus> {
    let key = state.store.kv_get("license.key")?;
    let mut warning = None;
    if let Some(key) = key {
        if state.api_base().is_empty() {
            warning = Some(
                "Removed locally. No license server is configured, so free the seat from your account page.".to_string(),
            );
        } else {
            let url = format!("{}/api/v1/licenses/deactivate", state.api_base());
            let res = state
                .http
                .post(url)
                .json(&json!({ "key": key, "deviceId": device_id(state) }))
                .send()
                .await;
            if res.is_err() {
                warning = Some(
                    "Removed locally, but the license server couldn't be reached — free the seat from your account page."
                        .to_string(),
                );
            }
        }
    }
    state.store.kv_delete("license.token")?;
    state.store.kv_delete("license.key")?;
    state.store.kv_delete("license.lastValidated")?;
    let mut st = status(state);
    st.message = warning;
    Ok(st)
}

/// Background re-validation (every 7 days) — silent on network errors.
pub async fn maybe_refresh(state: &AppState) {
    let st = status(state);
    if !st.licensed {
        return;
    }
    let due = st
        .last_validated
        .map(|t| now_secs() - t > REVALIDATE_EVERY)
        .unwrap_or(true);
    if due {
        let _ = refresh(state).await;
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateInfo {
    pub version: String,
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub pub_date: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub critical: bool,
    #[serde(default)]
    pub update_available: bool,
}

pub async fn check_updates(state: &AppState) -> AppResult<UpdateInfo> {
    let url = format!(
        "{}/api/v1/releases/latest?platform={}&arch={}&current={}",
        server(state)?,
        platform(),
        std::env::consts::ARCH,
        state.app_version
    );
    let resp = state.http.get(url).send().await?;
    let ok = resp.status().is_success();
    let body: serde_json::Value = resp.json().await.unwrap_or(json!({}));
    remember_free_until(state, &body);
    if !ok {
        // Nothing published yet: that's "up to date", not a network problem.
        if body["error"].as_str() == Some("no_release") {
            return Ok(UpdateInfo {
                version: state.app_version.to_string(),
                notes: String::new(),
                pub_date: None,
                url: None,
                critical: false,
                update_available: false,
            });
        }
        return Err(AppError::new("network", "Couldn't reach the update server"));
    }
    let mut info: UpdateInfo =
        serde_json::from_value(body).map_err(|e| AppError::new("network", format!("Unexpected answer from the update server: {e}")))?;
    if let Some(u) = &info.url {
        if u.starts_with('/') {
            info.url = Some(format!("{}{}", state.api_base(), u));
        }
    }
    Ok(info)
}

pub fn telemetry_enabled(state: &AppState) -> bool {
    state
        .settings()
        .get("telemetry")
        .and_then(|v| v.as_bool())
        // Opt-in: nothing is sent unless the user turned it on.
        .unwrap_or(false)
}

pub async fn send_telemetry(state: &AppState, event: &str) {
    if !telemetry_enabled(state) {
        return;
    }
    if state.api_base().is_empty() {
        return;
    }
    let url = format!("{}/api/v1/telemetry", state.api_base());
    let _ = state
        .http
        .post(url)
        .json(&json!({
            "installId": install_id(state),
            "event": event,
            "appVersion": state.app_version,
            "platform": platform(),
            "arch": std::env::consts::ARCH,
            "osVersion": crate::os::os_version(),
            "licensed": is_licensed(state),
        }))
        .send()
        .await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_garbage_tokens() {
        assert!(verify_token("nope").is_err());
        assert!(verify_token("RGL1.e30.AAAA").is_err());
        assert!(verify_token("QRT2.e30.AAAA").is_err());
    }

    #[test]
    fn public_key_is_valid() {
        let bytes = STANDARD.decode(PUBLIC_KEY_B64.trim()).unwrap();
        assert_eq!(bytes.len(), 32);
        let arr: [u8; 32] = bytes.try_into().unwrap();
        VerifyingKey::from_bytes(&arr).unwrap();
    }

    #[test]
    fn free_launch_period_follows_the_server() {
        let dir = std::env::temp_dir().join(format!("rowglow-launch-{}", crate::util::new_id()));
        let st = AppState::new(dir.clone(), true).unwrap();
        let now = now_secs();
        // Before hearing from the server: the built-in date.
        let expected = (now < FREE_UNTIL_DEFAULT).then_some(FREE_UNTIL_DEFAULT);
        assert_eq!(free_until(&st), expected);
        assert_eq!(has_pro(&st), expected.is_some());

        // The server extends it: every limit is lifted, without a license.
        remember_free_until(&st, &json!({ "version": "1.2.0", "freeUntil": now + 86_400 }));
        let s = status(&st);
        assert_eq!((s.plan.as_str(), s.licensed, s.free_until), ("launch", false, Some(now + 86_400)));
        assert!(s.limits.connections.is_none() && s.limits.table_export);
        assert!(has_pro(&st));

        // An answer without the field (older server) changes nothing.
        remember_free_until(&st, &json!({ "version": "1.2.0" }));
        assert_eq!(free_until(&st), Some(now + 86_400));

        // The server ends it (null), or the date passes: back to the free plan.
        remember_free_until(&st, &json!({ "error": "no_release", "freeUntil": null }));
        let s = status(&st);
        assert_eq!((s.plan.as_str(), s.free_until), ("free", None));
        assert_eq!(s.limits.connections, Some(FREE_CONNECTIONS));
        assert!(!has_pro(&st));
        remember_free_until(&st, &json!({ "freeUntil": now - 10 }));
        assert!(!has_pro(&st));
        drop(st);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn masks_keys() {
        assert_eq!(mask_key("RGLW-AAAAA-BBBBB-CCCCC-DDDDD"), "••••-DDDDD");
    }
}
