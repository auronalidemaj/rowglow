use serde::Serialize;
use std::fmt;

/// Error type returned to the UI. Serialized as JSON so the frontend can show
/// server error codes, hints and the error position inside the SQL editor.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppError {
    /// db | connection | cancelled | invalid | not_found | license | network | internal
    pub kind: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    /// 1-based character position in the statement (Postgres reports this).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position: Option<u32>,
}

pub type AppResult<T> = Result<T, AppError>;

impl AppError {
    pub fn new(kind: &str, message: impl Into<String>) -> Self {
        AppError {
            kind: kind.to_string(),
            message: message.into(),
            code: None,
            detail: None,
            hint: None,
            position: None,
        }
    }
    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new("invalid", message)
    }
    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new("not_found", message)
    }
    pub fn internal(message: impl Into<String>) -> Self {
        Self::new("internal", message)
    }
    pub fn license(message: impl Into<String>) -> Self {
        Self::new("license", message)
    }
    pub fn with_code(mut self, code: impl Into<String>) -> Self {
        self.code = Some(code.into());
        self
    }
}

impl fmt::Display for AppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for AppError {}

/// Joins an error with its source chain: "error connecting to server: Connection refused (os error 61)".
fn chain(err: &dyn std::error::Error) -> String {
    let mut out = err.to_string();
    let mut src = err.source();
    while let Some(s) = src {
        let s_str = s.to_string();
        if !out.contains(&s_str) {
            out.push_str(": ");
            out.push_str(&s_str);
        }
        src = s.source();
    }
    out
}

impl From<tokio_postgres::Error> for AppError {
    fn from(e: tokio_postgres::Error) -> Self {
        if let Some(db) = e.as_db_error() {
            let position = match db.position() {
                Some(tokio_postgres::error::ErrorPosition::Original(p)) => Some(*p),
                _ => None,
            };
            let cancelled = db.code().code() == "57014";
            AppError {
                kind: if cancelled { "cancelled".into() } else { "db".into() },
                message: db.message().to_string(),
                code: Some(db.code().code().to_string()),
                detail: db.detail().map(|s| s.to_string()),
                hint: db.hint().map(|s| s.to_string()),
                position,
            }
        } else {
            AppError::new("connection", chain(&e))
        }
    }
}

impl From<mysql_async::Error> for AppError {
    fn from(e: mysql_async::Error) -> Self {
        match &e {
            mysql_async::Error::Server(se) => {
                // 1317 = query execution was interrupted (KILL QUERY)
                let kind = if se.code == 1317 { "cancelled" } else { "db" };
                AppError {
                    kind: kind.into(),
                    message: se.message.clone(),
                    code: Some(format!("{} ({})", se.code, se.state)),
                    detail: None,
                    hint: None,
                    position: None,
                }
            }
            _ => AppError::new("connection", chain(&e)),
        }
    }
}

impl From<rusqlite::Error> for AppError {
    fn from(e: rusqlite::Error) -> Self {
        match &e {
            rusqlite::Error::SqliteFailure(err, msg) => {
                let kind = if err.code == rusqlite::ErrorCode::OperationInterrupted {
                    "cancelled"
                } else {
                    "db"
                };
                AppError {
                    kind: kind.into(),
                    message: msg.clone().unwrap_or_else(|| err.to_string()),
                    code: Some(format!("{:?}", err.code)),
                    detail: None,
                    hint: None,
                    position: None,
                }
            }
            _ => AppError::new("db", e.to_string()),
        }
    }
}

impl From<duckdb::Error> for AppError {
    fn from(e: duckdb::Error) -> Self {
        match &e {
            duckdb::Error::DuckDBFailure(_, msg) => {
                let message = msg.clone().unwrap_or_else(|| e.to_string());
                let kind = if message.contains("INTERRUPT") || message.contains("Interrupted") {
                    "cancelled"
                } else {
                    "db"
                };
                AppError::new(kind, message)
            }
            _ => AppError::new("db", e.to_string()),
        }
    }
}

impl From<tiberius::error::Error> for AppError {
    fn from(e: tiberius::error::Error) -> Self {
        use tiberius::error::Error as T;
        match &e {
            T::Server(t) => {
                // 18456 = login failed; 4060 = cannot open database
                let kind = if matches!(t.code(), 18456 | 4060 | 18452) { "connection" } else { "db" };
                let mut err = AppError::new(kind, t.message().to_string()).with_code(t.code().to_string());
                if t.line() > 0 {
                    err.detail = Some(format!("Line {}", t.line()));
                }
                err
            }
            T::Io { .. } | T::Tls(_) | T::Routing { .. } | T::Protocol(_) => AppError::new("connection", chain(&e)),
            _ => AppError::new("db", e.to_string()),
        }
    }
}

impl From<serde_json::Error> for AppError {
    fn from(e: serde_json::Error) -> Self {
        AppError::invalid(format!("Invalid parameters: {e}"))
    }
}

impl From<std::io::Error> for AppError {
    fn from(e: std::io::Error) -> Self {
        AppError::internal(e.to_string())
    }
}

impl From<reqwest::Error> for AppError {
    fn from(e: reqwest::Error) -> Self {
        AppError::new("network", chain(&e))
    }
}

impl From<csv::Error> for AppError {
    fn from(e: csv::Error) -> Self {
        AppError::internal(e.to_string())
    }
}

impl From<tokio::task::JoinError> for AppError {
    fn from(e: tokio::task::JoinError) -> Self {
        AppError::internal(e.to_string())
    }
}
