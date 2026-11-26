//! Microsoft Entra ID (Azure AD) access tokens for Azure SQL / SQL Server.
//!
//! Two ways to get a token, neither of which needs Rowglow to be registered
//! as an app in the user's tenant:
//! * **Azure CLI** — reuses the account the user signed in to with `az login`
//!   (works with MFA and conditional access, since `az` handles the browser
//!   sign-in). We run `az account get-access-token`, nothing else.
//! * **Service principal** — the OAuth 2.0 client-credentials flow with an
//!   application (client) ID and client secret.
//!
//! Tokens are only ever sent to SQL Server during login; they are never
//! stored on disk by Rowglow.

use crate::error::{AppError, AppResult};
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// The resource (audience) Azure SQL tokens are issued for.
pub const SQL_RESOURCE: &str = "https://database.windows.net/";
pub const SQL_SCOPE: &str = "https://database.windows.net/.default";
pub const DEFAULT_AUTHORITY: &str = "https://login.microsoftonline.com";

/// The sign-in authority. Debug builds can point at a local mock (tests).
pub fn authority() -> String {
    #[cfg(debug_assertions)]
    if let Ok(a) = std::env::var("ROWGLOW_ENTRA_AUTHORITY") {
        if !a.trim().is_empty() {
            return a.trim().to_string();
        }
    }
    DEFAULT_AUTHORITY.to_string()
}

#[derive(Debug, Clone)]
pub struct Token {
    pub access_token: String,
    /// Unix seconds.
    pub expires_at: i64,
}

impl Token {
    /// Still valid for at least five more minutes?
    pub fn fresh(&self) -> bool {
        self.expires_at - now() > 300
    }
}

fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// Tenant IDs are GUIDs or domain names; anything else is rejected before it
/// reaches a URL or a command line.
pub fn valid_tenant(t: &str) -> bool {
    // Must start with a letter or digit so it can never be read as a flag.
    t.len() <= 128
        && t.chars().next().is_some_and(|c| c.is_ascii_alphanumeric())
        && t.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.')
}

fn http() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .user_agent(concat!("Rowglow/", env!("CARGO_PKG_VERSION")))
            .build()
            .expect("http client")
    })
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: Option<String>,
    expires_in: Option<serde_json::Value>,
    error: Option<String>,
    error_description: Option<String>,
}

/// Client-credentials flow (service principal with a client secret).
pub async fn service_principal(authority: &str, tenant: &str, client_id: &str, secret: &str) -> AppResult<Token> {
    let tenant = tenant.trim();
    if !valid_tenant(tenant) {
        return Err(AppError::invalid("Enter your Microsoft Entra tenant ID (or domain, like contoso.onmicrosoft.com)"));
    }
    if client_id.trim().is_empty() || secret.is_empty() {
        return Err(AppError::invalid("Enter the application (client) ID and the client secret"));
    }
    let url = format!("{}/{}/oauth2/v2.0/token", authority.trim_end_matches('/'), tenant);
    let res = http()
        .post(&url)
        .form(&[
            ("grant_type", "client_credentials"),
            ("client_id", client_id.trim()),
            ("client_secret", secret),
            ("scope", SQL_SCOPE),
        ])
        .send()
        .await
        .map_err(|e| AppError::new("connection", format!("Couldn't reach Microsoft Entra ID: {e}")))?;
    let status = res.status();
    let body: TokenResponse = res
        .json()
        .await
        .map_err(|e| AppError::new("connection", format!("Unexpected reply from Microsoft Entra ID ({status}): {e}")))?;
    match body.access_token {
        Some(t) if status.is_success() => {
            let secs = match body.expires_in {
                Some(serde_json::Value::Number(n)) => n.as_i64().unwrap_or(3600),
                Some(serde_json::Value::String(s)) => s.parse().unwrap_or(3600),
                _ => 3600,
            };
            Ok(Token { access_token: t, expires_at: now() + secs })
        }
        _ => {
            let code = body.error.unwrap_or_else(|| status.to_string());
            // The description's first line is the readable part ("AADSTS7000215: Invalid client secret…").
            let desc = body
                .error_description
                .as_deref()
                .and_then(|d| d.lines().next())
                .unwrap_or("")
                .to_string();
            Err(AppError::new("auth", format!("Microsoft Entra ID refused the sign-in ({code}). {desc}").trim().to_string())
                .with_code("entra_failed"))
        }
    }
}

/// Where the Azure CLI usually lives. GUI apps get a minimal PATH on macOS,
/// so check the common install locations before PATH.
pub fn az_bin() -> Option<PathBuf> {
    let mut c: Vec<PathBuf> = Vec::new();
    #[cfg(target_os = "macos")]
    c.extend(["/opt/homebrew/bin/az", "/usr/local/bin/az"].map(PathBuf::from));
    #[cfg(target_os = "linux")]
    c.extend(["/usr/bin/az", "/usr/local/bin/az", "/snap/bin/az"].map(PathBuf::from));
    #[cfg(windows)]
    for var in ["ProgramW6432", "ProgramFiles", "ProgramFiles(x86)"] {
        if let Some(dir) = std::env::var_os(var) {
            c.push(PathBuf::from(dir).join(r"Microsoft SDKs\Azure\CLI2\wbin\az.cmd"));
        }
    }
    #[cfg(unix)]
    if let Some(home) = dirs::home_dir() {
        c.push(home.join(".local/bin/az"));
    }
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            #[cfg(windows)]
            c.push(dir.join("az.cmd"));
            #[cfg(not(windows))]
            c.push(dir.join("az"));
        }
    }
    c.into_iter().find(|p| p.is_file())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CliToken {
    access_token: String,
    #[serde(rename = "expires_on")]
    expires_on: Option<i64>,
}

/// Parses `az account get-access-token --output json`.
pub fn parse_cli_token(stdout: &[u8]) -> AppResult<Token> {
    let t: CliToken = serde_json::from_slice(stdout)
        .map_err(|e| AppError::new("auth", format!("Unexpected output from the Azure CLI: {e}")))?;
    Ok(Token {
        access_token: t.access_token,
        // Older CLI versions only print a local-time `expiresOn`; assume the usual hour.
        expires_at: t.expires_on.unwrap_or_else(|| now() + 3000),
    })
}

/// Token for the account signed in to the Azure CLI (`az login`).
pub async fn azure_cli(az: Option<PathBuf>, tenant: Option<&str>) -> AppResult<Token> {
    let Some(az) = az.or_else(az_bin) else {
        return Err(AppError::new(
            "auth",
            "The Azure CLI isn't installed. Install it (brew install azure-cli on macOS), run az login, then connect again.",
        )
        .with_code("az_missing"));
    };
    let mut args: Vec<String> = ["account", "get-access-token", "--resource", SQL_RESOURCE, "--output", "json"]
        .map(String::from)
        .to_vec();
    if let Some(t) = tenant.map(str::trim).filter(|t| !t.is_empty()) {
        if !valid_tenant(t) {
            return Err(AppError::invalid("The tenant should be a tenant ID or a domain like contoso.onmicrosoft.com"));
        }
        args.push("--tenant".into());
        args.push(t.into());
    }
    let out = tokio::time::timeout(Duration::from_secs(60), crate::os::command(&az).args(&args).output())
        .await
        .map_err(|_| AppError::new("auth", "The Azure CLI didn't answer within a minute"))?
        .map_err(|e| AppError::new("auth", format!("Couldn't run the Azure CLI: {e}")))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let line = err
            .lines()
            .map(str::trim)
            .find(|l| l.starts_with("ERROR"))
            .or_else(|| err.lines().map(str::trim).find(|l| !l.is_empty()))
            .unwrap_or("unknown error")
            .trim_start_matches("ERROR:")
            .trim()
            .to_string();
        let hint = if err.contains("az login") || err.to_lowercase().contains("expired") {
            " Run az login in a terminal, then connect again."
        } else {
            ""
        };
        return Err(AppError::new("auth", format!("The Azure CLI couldn't get a token: {line}.{hint}")).with_code("entra_failed"));
    }
    parse_cli_token(&out.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tenants() {
        assert!(valid_tenant("72f988bf-86f1-41af-91ab-2d7cd011db47"));
        assert!(valid_tenant("contoso.onmicrosoft.com"));
        assert!(!valid_tenant("evil/../x"));
        assert!(!valid_tenant("a b"));
        assert!(!valid_tenant("--output"));
        assert!(!valid_tenant(""));
    }

    #[test]
    fn parses_cli_output() {
        let t = parse_cli_token(
            br#"{"accessToken":"eyJ0","expiresOn":"2030-01-01 10:00:00.000000","expires_on":1893492000,"subscription":"x","tenant":"t","tokenType":"Bearer"}"#,
        )
        .unwrap();
        assert_eq!(t.access_token, "eyJ0");
        assert_eq!(t.expires_at, 1893492000);
        assert!(t.fresh());
        let old = parse_cli_token(br#"{"accessToken":"a","expiresOn":"2030-01-01 10:00:00.000000"}"#).unwrap();
        assert!(old.fresh());
    }
}
