//! Single RPC dispatcher used by the Tauri `rpc` command and the dev HTTP
//! bridge. Every method takes a JSON object and returns JSON.

use crate::discovery;
use crate::drivers::{Channel, Session};
use crate::error::{AppError, AppResult};
use crate::export;
use crate::license;
use crate::model::*;
use crate::ssh::{SshAuth, SshParams};
use crate::state::AppState;
use crate::util::{first_keyword, new_id};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;

fn ok<T: serde::Serialize>(v: T) -> AppResult<Value> {
    serde_json::to_value(v).map_err(|e| AppError::internal(e.to_string()))
}

fn p<T: serde::de::DeserializeOwned>(params: &Value) -> AppResult<T> {
    let v = if params.is_null() { json!({}) } else { params.clone() };
    Ok(serde_json::from_value(v)?)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct IdParam {
    id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionParam {
    session_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SaveConnection {
    connection: ConnectionConfig,
    #[serde(default)]
    password: Option<String>,
    #[serde(default)]
    clear_password: bool,
    /// SSH password or private-key passphrase.
    #[serde(default)]
    ssh_password: Option<String>,
    #[serde(default)]
    clear_ssh_password: bool,
    /// Redis Sentinel: password for the Sentinels themselves.
    #[serde(default)]
    sentinel_password: Option<String>,
    #[serde(default)]
    clear_sentinel_password: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TestConnection {
    connection: ConnectionConfig,
    #[serde(default)]
    password: Option<String>,
    #[serde(default)]
    ssh_password: Option<String>,
    #[serde(default)]
    sentinel_password: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct OpenSession {
    connection_id: String,
    #[serde(default)]
    database: Option<String>,
    #[serde(default)]
    password: Option<String>,
    #[serde(default)]
    save_password: bool,
    #[serde(default)]
    ssh_password: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SchemaParam {
    session_id: String,
    #[serde(default)]
    schema: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TableParam {
    session_id: String,
    schema: String,
    table: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DdlParam {
    session_id: String,
    schema: String,
    name: String,
    #[serde(default)]
    kind: String,
    #[serde(default)]
    signature: Option<String>,
}

fn default_true() -> bool {
    true
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ExecuteParam {
    session_id: String,
    statements: Vec<String>,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default = "default_true")]
    stop_on_error: bool,
    #[serde(default)]
    record_history: bool,
    #[serde(default)]
    channel: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TxParam {
    session_id: String,
    statements: Vec<String>,
    /// Staged edits: every statement must affect exactly one row or the
    /// whole transaction is rolled back.
    #[serde(default)]
    expect_one_row: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct HistoryParam {
    #[serde(default)]
    connection_id: Option<String>,
    #[serde(default)]
    search: Option<String>,
    #[serde(default)]
    limit: Option<u32>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SnippetParam {
    snippet: Snippet,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ExportParam {
    session_id: String,
    sql: String,
    path: String,
    format: String,
    #[serde(default)]
    schema: String,
    #[serde(default)]
    table: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WriteParam {
    path: String,
    contents: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PathParam {
    path: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct KeyParam {
    key: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SettingsPatch {
    patch: serde_json::Map<String, Value>,
}

fn same_host(a: &str, b: &str) -> bool {
    let norm = |h: &str| match h.trim() {
        "" | "localhost" | "127.0.0.1" | "::1" | "0.0.0.0" => "localhost".to_string(),
        other => other.to_lowercase(),
    };
    norm(a) == norm(b)
}

pub fn ssh_secret_id(connection_id: &str) -> String {
    format!("{connection_id}:ssh")
}

pub fn sentinel_secret_id(connection_id: &str) -> String {
    format!("{connection_id}:sentinel")
}

/// Default private keys tried when an SSH key connection doesn't name one.
fn default_ssh_key() -> String {
    for name in ["id_ed25519", "id_ecdsa", "id_rsa"] {
        if let Some(p) = dirs::home_dir().map(|h| h.join(".ssh").join(name)) {
            if p.exists() {
                return p.to_string_lossy().into_owned();
            }
        }
    }
    "~/.ssh/id_ed25519".into()
}

/// SSH settings for a connection, with its stored secret and trusted host key.
fn ssh_params(st: &AppState, cfg: &ConnectionConfig, secret: Option<String>) -> AppResult<Option<SshParams>> {
    if !cfg.ssh_enabled || cfg.engine.is_file() {
        return Ok(None);
    }
    if cfg.ssh_host.trim().is_empty() || cfg.ssh_user.trim().is_empty() {
        return Err(AppError::invalid("Enter the SSH host and user, or turn off the SSH tunnel"));
    }
    let secret = match secret {
        Some(s) => Some(s),
        None if !cfg.id.is_empty() => st.secrets.get(&st.store, &ssh_secret_id(&cfg.id))?,
        None => None,
    };
    let port = if cfg.ssh_port == 0 { 22 } else { cfg.ssh_port };
    let auth = if cfg.ssh_auth == "key" {
        let path = if cfg.ssh_key_path.trim().is_empty() { default_ssh_key() } else { cfg.ssh_key_path.trim().to_string() };
        SshAuth::Key {
            path,
            passphrase: secret.filter(|s| !s.is_empty()),
        }
    } else {
        SshAuth::Password(secret.unwrap_or_default())
    };
    Ok(Some(SshParams {
        host: cfg.ssh_host.trim().to_string(),
        port,
        user: cfg.ssh_user.trim().to_string(),
        auth,
        known_fingerprint: st.store.known_host(cfg.ssh_host.trim(), port)?,
    }))
}

async fn open_session(
    st: &AppState,
    cfg: &ConnectionConfig,
    password: Option<&str>,
    database: Option<&str>,
    ssh_secret: Option<String>,
) -> AppResult<Session> {
    let ssh = ssh_params(st, cfg, ssh_secret)?;
    let mut cfg = cfg.clone();
    if cfg.engine == Engine::Redis && cfg.options.redis_mode() == "sentinel" && cfg.sentinel_password.is_none() && !cfg.id.is_empty() {
        cfg.sentinel_password = st.secrets.get(&st.store, &sentinel_secret_id(&cfg.id))?;
    }
    let id = new_id();
    tokio::time::timeout(Duration::from_secs(40), Session::open(id, &cfg, password, database, ssh))
        .await
        .map_err(|_| AppError::new("connection", "Timed out while connecting"))?
}

fn validate_connection(c: &ConnectionConfig) -> AppResult<()> {
    if c.name.trim().is_empty() {
        return Err(AppError::invalid("Give the connection a name"));
    }
    match c.engine {
        Engine::Sqlite if c.file_path.trim().is_empty() => Err(AppError::invalid("Choose a SQLite database file")),
        Engine::Duckdb if c.file_path.trim().is_empty() => {
            Err(AppError::invalid("Choose a DuckDB database file, or enter :memory: for an in-memory database"))
        }
        _ if c.ssh_enabled && !c.engine.is_file() && (c.ssh_host.trim().is_empty() || c.ssh_user.trim().is_empty()) => {
            Err(AppError::invalid("Enter the SSH host and user, or turn off the SSH tunnel"))
        }
        Engine::Redis if c.ssh_enabled && c.options.redis_mode() != "standalone" => Err(AppError::invalid(
            "SSH tunnels reach a single server, so they can't be used with Redis Cluster or Sentinel. Turn off the SSH tunnel or choose Single server.",
        )),
        Engine::Redis if !matches!(c.options.redis_mode(), "standalone" | "cluster" | "sentinel") => {
            Err(AppError::invalid("Choose Single server, Cluster or Sentinel"))
        }
        Engine::Mssql if !matches!(c.options.mssql_auth(), "sql" | "windows" | "integrated" | "entra-cli" | "entra-sp") => {
            Err(AppError::invalid("Choose how to sign in to SQL Server"))
        }
        Engine::Mssql if c.options.mssql_auth() == "entra-sp" && c.options.tenant.trim().is_empty() => {
            Err(AppError::invalid("Enter the Microsoft Entra tenant ID for the service principal"))
        }
        Engine::Mssql if !c.options.tenant.trim().is_empty() && !crate::entra::valid_tenant(c.options.tenant.trim()) => {
            Err(AppError::invalid("The tenant should be a tenant ID or a domain like contoso.onmicrosoft.com"))
        }
        _ => Ok(()),
    }
}

pub async fn dispatch(state: &Arc<AppState>, method: &str, params: Value) -> AppResult<Value> {
    let st = state.as_ref();
    match method {
        // ---------------------------------------------------------------- app
        "app.info" => ok(json!({
            "version": st.app_version,
            "platform": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
            "dataDir": st.data_dir.to_string_lossy(),
            "apiBase": st.api_base(),
            "bridge": st.is_bridge,
            "debug": cfg!(debug_assertions),
            "secretsInFile": st.secrets.in_file(&st.store),
            "recoveredDb": st.recovered_db.as_ref().map(|p| p.to_string_lossy().into_owned()),
        })),
        "settings.get" => ok(Value::Object(st.settings())),
        "settings.set" => {
            let SettingsPatch { patch } = p(&params)?;
            let mut cur = st.settings();
            for (k, v) in patch {
                if v.is_null() {
                    cur.remove(&k);
                } else {
                    cur.insert(k, v);
                }
            }
            st.store.kv_set("settings", &Value::Object(cur.clone()).to_string())?;
            ok(Value::Object(cur))
        }

        // -------------------------------------------------------- connections
        "connections.list" => ok(st.store.list_connections()?),
        "connections.get" => {
            let IdParam { id } = p(&params)?;
            let mut c = st.store.get_connection(&id)?;
            c.has_password = st
                .secrets
                .get(&st.store, &id)?
                .map(|p| !p.is_empty())
                .unwrap_or(false);
            c.has_ssh_secret = st
                .secrets
                .get(&st.store, &ssh_secret_id(&id))?
                .map(|p| !p.is_empty())
                .unwrap_or(false);
            ok(c)
        }
        "connections.save" => {
            let SaveConnection {
                mut connection,
                mut password,
                clear_password,
                ssh_password,
                clear_ssh_password,
                sentinel_password,
                clear_sentinel_password,
            } = p(&params)?;
            validate_connection(&connection)?;
            // A pasted mongodb:// address with user:password@ must not keep the
            // password in the saved host: it goes to the keychain like any other.
            if connection.engine == Engine::Mongodb {
                if let Some((host, user, pass)) = crate::drivers::mongo::split_uri_credentials(&connection.host) {
                    connection.host = host;
                    if connection.username.trim().is_empty() {
                        connection.username = user;
                    }
                    if password.as_deref().is_none_or(str::is_empty) {
                        password = Some(pass);
                    }
                }
            }
            let is_new = connection.id.is_empty();
            // A made-up id is a new connection too, as far as the limit goes.
            let adds_one = is_new || st.store.get_connection(&connection.id).is_err();
            if adds_one && !license::has_pro(st) {
                let n = st.store.count_connections()?;
                if n >= license::FREE_CONNECTIONS as i64 {
                    return Err(AppError::license(format!(
                        "The free plan includes {} saved connections. Upgrade to Rowglow Pro for unlimited connections.",
                        license::FREE_CONNECTIONS
                    ))
                    .with_code("limit_connections"));
                }
            }
            st.store.upsert_connection(&mut connection)?;
            if clear_password {
                st.secrets.delete(&st.store, &connection.id)?;
            } else if let Some(pw) = password.filter(|p| !p.is_empty()) {
                st.secrets.set(&st.store, &connection.id, &pw)?;
            }
            let ssh_id = ssh_secret_id(&connection.id);
            if clear_ssh_password || !connection.ssh_enabled {
                st.secrets.delete(&st.store, &ssh_id)?;
            } else if let Some(pw) = ssh_password.filter(|p| !p.is_empty()) {
                st.secrets.set(&st.store, &ssh_id, &pw)?;
            }
            let sentinel_id = sentinel_secret_id(&connection.id);
            if clear_sentinel_password || connection.engine != Engine::Redis || connection.options.redis_mode() != "sentinel" {
                st.secrets.delete(&st.store, &sentinel_id)?;
            } else if let Some(pw) = sentinel_password.filter(|p| !p.is_empty()) {
                st.secrets.set(&st.store, &sentinel_id, &pw)?;
            }
            ok(st.store.get_connection(&connection.id)?)
        }
        "connections.delete" => {
            let IdParam { id } = p(&params)?;
            let mut sessions = st.sessions.write().await;
            sessions.retain(|_, s| s.connection.id != id);
            drop(sessions);
            st.secrets.delete(&st.store, &id)?;
            st.secrets.delete(&st.store, &ssh_secret_id(&id))?;
            st.secrets.delete(&st.store, &sentinel_secret_id(&id))?;
            st.store.delete_connection(&id)?;
            ok(json!({ "ok": true }))
        }
        "connections.test" => {
            let TestConnection {
                mut connection,
                password,
                ssh_password,
                sentinel_password,
            } = p(&params)?;
            validate_connection(&connection)?;
            if sentinel_password.as_deref().is_some_and(|p| !p.is_empty()) {
                connection.sentinel_password = sentinel_password;
            }
            let pw = match password {
                Some(p) => Some(p),
                None if !connection.id.is_empty() => st.secrets.get(&st.store, &connection.id)?,
                None => None,
            };
            let s = open_session(st, &connection, pw.as_deref(), None, ssh_password).await?;
            ok(s.info.clone())
        }
        "connections.createSqlite" => {
            let PathParam { path } = p(&params)?;
            crate::drivers::sqlite::create_file(&path)?;
            ok(json!({ "ok": true }))
        }
        "connections.createFile" => {
            #[derive(Deserialize)]
            struct CreateFile {
                engine: Engine,
                path: String,
            }
            let CreateFile { engine, path } = p(&params)?;
            match engine {
                Engine::Sqlite => crate::drivers::sqlite::create_file(&path)?,
                Engine::Duckdb => {
                    tokio::task::spawn_blocking(move || crate::drivers::duckdb::create_file(&path)).await??
                }
                _ => return Err(AppError::invalid("Only SQLite and DuckDB databases are files")),
            }
            ok(json!({ "ok": true }))
        }

        // --------------------------------------------------------------- ssh
        "ssh.trust" => {
            #[derive(Deserialize)]
            struct Trust {
                host: String,
                port: u16,
                fingerprint: String,
            }
            let Trust { host, port, fingerprint } = p(&params)?;
            if !fingerprint.starts_with("SHA256:") {
                return Err(AppError::invalid("Unexpected host key fingerprint"));
            }
            st.store.trust_host(host.trim(), if port == 0 { 22 } else { port }, &fingerprint)?;
            ok(json!({ "ok": true }))
        }
        "ssh.forget" => {
            #[derive(Deserialize)]
            struct Forget {
                host: String,
                port: u16,
            }
            let Forget { host, port } = p(&params)?;
            st.store.forget_host(host.trim(), if port == 0 { 22 } else { port })?;
            ok(json!({ "ok": true }))
        }

        // ---------------------------------------------------------------- ai
        "ai.settings" => {
            let settings = crate::ai::load_settings(st);
            let mut keys = serde_json::Map::new();
            for pr in crate::ai::PROVIDERS.iter().filter(|p| p.needs_key || p.id == "custom") {
                let has = st
                    .secrets
                    .get(&st.store, &crate::ai::key_secret(pr.id))?
                    .map(|k| !k.trim().is_empty())
                    .unwrap_or(false);
                keys.insert(pr.id.to_string(), json!(has));
            }
            let per_day = if license::has_pro(st) { None } else { Some(license::FREE_AI_PER_DAY) };
            ok(json!({
                "settings": settings,
                "keys": keys,
                "providers": crate::ai::PROVIDERS,
                "perDay": per_day,
                "usedToday": crate::ai::used_today(st),
            }))
        }
        "ai.saveSettings" => {
            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Save {
                settings: crate::ai::AiSettings,
                #[serde(default)]
                api_key: Option<String>,
                #[serde(default)]
                clear_key: bool,
            }
            let Save { settings, api_key, clear_key } = p(&params)?;
            crate::ai::save_settings(st, &settings)?;
            let secret = crate::ai::key_secret(&settings.provider);
            if clear_key {
                st.secrets.delete(&st.store, &secret)?;
            } else if let Some(k) = api_key.map(|k| k.trim().to_string()).filter(|k| !k.is_empty()) {
                if settings.provider == "anthropic" && !k.starts_with("sk-ant-") {
                    return Err(AppError::invalid("That doesn't look like an Anthropic API key (they start with sk-ant-)"));
                }
                st.secrets.set(&st.store, &secret, &k)?;
            }
            ok(json!({ "ok": true }))
        }
        "ai.models" => {
            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Models {
                settings: crate::ai::AiSettings,
                #[serde(default)]
                api_key: Option<String>,
            }
            let Models { settings, api_key } = p(&params)?;
            let key = match api_key.filter(|k| !k.trim().is_empty()) {
                Some(k) => k,
                None => st.secrets.get(&st.store, &crate::ai::key_secret(&settings.provider))?.unwrap_or_default(),
            };
            let models = crate::ai::list_models(&settings, &settings.provider, &key).await?;
            let recommended = crate::ai::recommended(&settings.provider, &models);
            ok(json!({ "models": models, "recommended": recommended }))
        }
        "ai.test" => {
            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Test {
                settings: crate::ai::AiSettings,
                #[serde(default)]
                api_key: Option<String>,
            }
            let Test { settings, api_key } = p(&params)?;
            let key = match api_key.filter(|k| !k.trim().is_empty()) {
                Some(k) => k,
                None => st.secrets.get(&st.store, &crate::ai::key_secret(&settings.provider))?.unwrap_or_default(),
            };
            let name = crate::ai::test_provider(&settings, &key).await?;
            ok(json!({ "ok": true, "model": name }))
        }
        "ai.ollamaModels" => {
            #[derive(Deserialize)]
            struct Url {
                #[serde(default)]
                url: String,
            }
            let Url { url } = p(&params)?;
            ok(crate::ai::ollama_models(&url).await?)
        }
        "ai.start" => {
            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Start {
                session_id: String,
                task: crate::ai::Task,
                #[serde(default)]
                question: String,
                #[serde(default)]
                sql: String,
                #[serde(default)]
                error: String,
                #[serde(default)]
                schema: String,
            }
            let Start { session_id, task, question, sql, error, schema } = p(&params)?;
            match task {
                crate::ai::Task::Generate if question.trim().is_empty() => {
                    return Err(AppError::invalid("Describe what you want the query to do"))
                }
                crate::ai::Task::Explain | crate::ai::Task::Fix if sql.trim().is_empty() => {
                    return Err(AppError::invalid("There's no SQL to work with"))
                }
                _ => {}
            }
            let s = st.session(&session_id).await?;
            if !s.engine().is_sql() {
                return Err(AppError::invalid("The AI assistant works with SQL databases"));
            }
            let settings = crate::ai::load_settings(st);
            let pr = crate::ai::provider(&settings.provider)?;
            let key = st.secrets.get(&st.store, &crate::ai::key_secret(pr.id))?.unwrap_or_default();
            if pr.needs_key && key.trim().is_empty() {
                return Err(AppError::invalid(format!(
                    "Set up the AI assistant first: add your {} API key in Settings → AI, or choose another provider.",
                    pr.name
                ))
                .with_code("ai_not_configured"));
            }
            if settings.model_for(pr.id).trim().is_empty() {
                return Err(AppError::invalid(format!("Choose a {} model in Settings → AI first", pr.name)).with_code("ai_not_configured"));
            }
            let api_key = Some(key).filter(|k| !k.trim().is_empty());
            if !license::has_pro(st) && crate::ai::used_today(st) >= license::FREE_AI_PER_DAY {
                return Err(AppError::license(format!(
                    "The free plan includes {} AI requests a day. Upgrade to Rowglow Pro for unlimited requests.",
                    license::FREE_AI_PER_DAY
                ))
                .with_code("limit_ai"));
            }
            let schema = if schema.trim().is_empty() { s.info.default_schema.clone() } else { schema };
            let hint = format!("{question} {sql}");
            let (schema_text, tables) = crate::ai::schema_context(&s, &schema, &hint).await?;
            let user = crate::ai::user_message(task, &question, &sql, &error);
            let provider = settings.provider.clone();
            let model = settings.model_for(&provider);
            let job_id = crate::ai::start(st, crate::ai::Request { settings, api_key, schema_text, user });
            let per_day = if license::has_pro(st) { None } else { Some(license::FREE_AI_PER_DAY) };
            ok(json!({
                "jobId": job_id,
                "tables": tables,
                "provider": provider,
                "model": model,
                "perDay": per_day,
                "usedToday": crate::ai::used_today(st),
            }))
        }
        "ai.poll" => {
            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Poll {
                job_id: String,
            }
            let Poll { job_id } = p(&params)?;
            match crate::ai::poll(&job_id) {
                Some(s) => ok(s),
                None => Err(AppError::not_found("That AI request is no longer available")),
            }
        }
        "ai.cancel" => {
            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Cancel {
                job_id: String,
            }
            let Cancel { job_id } = p(&params)?;
            ok(json!({ "ok": crate::ai::cancel(&job_id) }))
        }

        // ------------------------------------------------------------- radar
        "radar.scan" => {
            let mut found = discovery::scan().await;
            let saved = st.store.list_connections()?;
            for d in found.iter_mut() {
                d.already_saved = saved.iter().any(|c| {
                    c.engine == d.engine
                        && same_host(&c.host, &d.host)
                        && (c.port == d.port || (c.port == 0 && d.port == c.engine.default_port()))
                });
            }
            ok(found)
        }

        // ----------------------------------------------------------- session
        "session.open" => {
            let OpenSession {
                connection_id,
                database,
                password,
                save_password,
                ssh_password,
            } = p(&params)?;
            let cfg = st.store.get_connection(&connection_id)?;
            let pw = match &password {
                Some(p) => Some(p.clone()),
                None => st.secrets.get(&st.store, &connection_id)?,
            };
            let session = open_session(st, &cfg, pw.as_deref(), database.as_deref(), ssh_password).await?;
            if save_password {
                if let Some(p) = password.filter(|p| !p.is_empty()) {
                    st.secrets.set(&st.store, &connection_id, &p)?;
                }
            }
            let info = session.info.clone();
            st.sessions
                .write()
                .await
                .insert(session.id.clone(), Arc::new(session));
            st.store.touch_connection(&connection_id)?;
            ok(info)
        }
        "session.close" => {
            let SessionParam { session_id } = p(&params)?;
            st.sessions.write().await.remove(&session_id);
            ok(json!({ "ok": true }))
        }
        "session.useSchema" => {
            let SchemaParam { session_id, schema } = p(&params)?;
            st.session(&session_id).await?.use_schema(&schema).await?;
            ok(json!({ "ok": true }))
        }

        // -------------------------------------------------------------- meta
        "meta.databases" => {
            let SessionParam { session_id } = p(&params)?;
            ok(st.session(&session_id).await?.databases().await?)
        }
        "meta.schemas" => {
            let SessionParam { session_id } = p(&params)?;
            ok(st.session(&session_id).await?.schemas().await?)
        }
        "meta.objects" => {
            let SchemaParam { session_id, schema } = p(&params)?;
            ok(st.session(&session_id).await?.objects(&schema).await?)
        }
        "meta.structure" => {
            let TableParam {
                session_id,
                schema,
                table,
            } = p(&params)?;
            ok(st.session(&session_id).await?.structure(&schema, &table).await?)
        }
        "meta.ddl" => {
            let DdlParam {
                session_id,
                schema,
                name,
                kind,
                signature,
            } = p(&params)?;
            ok(st
                .session(&session_id)
                .await?
                .ddl(&schema, &name, &kind, signature.as_deref())
                .await?)
        }
        "meta.relations" => {
            let SchemaParam { session_id, schema } = p(&params)?;
            let s = st.session(&session_id).await?;
            let relations = s.relations(&schema).await?;
            let tables: Vec<CompletionTable> = s
                .completions(Some(schema.as_str()))
                .await?
                .into_iter()
                .filter(|t| t.schema == schema || (s.engine() == Engine::Sqlite && t.schema == "main"))
                .collect();
            ok(json!({ "tables": tables, "relations": relations }))
        }
        "meta.completions" => {
            let SchemaParam { session_id, schema } = p(&params)?;
            let s = st.session(&session_id).await?;
            ok(s.completions(Some(schema.as_str()).filter(|s| !s.is_empty())).await?)
        }

        // ------------------------------------------------------------- redis
        "redis.overview" => {
            let SessionParam { session_id } = p(&params)?;
            let s = st.session(&session_id).await?;
            ok(s.with_redis(|r| async move { r.overview().await }).await?)
        }
        "redis.scan" => {
            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Scan {
                session_id: String,
                #[serde(default)]
                cursor: String,
                #[serde(default)]
                pattern: String,
                #[serde(default)]
                count: Option<usize>,
                #[serde(default)]
                kind: Option<String>,
            }
            let Scan { session_id, cursor, pattern, count, kind } = p(&params)?;
            let s = st.session(&session_id).await?;
            let (cursor, pattern, kind) = (cursor.as_str(), pattern.as_str(), kind.as_deref());
            ok(s.with_redis(|r| async move { r.scan(cursor, pattern, count.unwrap_or(500), kind).await }).await?)
        }
        "redis.get" => {
            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Get {
                session_id: String,
                key: String,
                #[serde(default)]
                key_b64: Option<String>,
            }
            let Get { session_id, key, key_b64 } = p(&params)?;
            let s = st.session(&session_id).await?;
            let (key, key_b64) = (&key, &key_b64);
            ok(s.with_redis(|r| async move { r.get(key, key_b64).await }).await?)
        }
        "redis.op" => {
            let session_id = params.get("sessionId").and_then(Value::as_str).unwrap_or_default().to_string();
            let op: crate::drivers::redis::RedisOp = p(&params)?;
            let s = st.session(&session_id).await?;
            let summary = format!("-- Redis: {}", params.get("op").and_then(Value::as_str).unwrap_or("change"));
            let r = s.redis().await?.op(op).await?;
            let _ = st.store.add_history(
                &HistoryEntry {
                    id: 0,
                    connection_id: s.connection.id.clone(),
                    database: s.info.database.clone(),
                    sql: format!("{summary} {}", params.get("key").and_then(Value::as_str).unwrap_or_default()),
                    duration_ms: 0.0,
                    rows: r.get("changed").and_then(Value::as_i64),
                    error: None,
                    created_at: 0,
                },
                Some(license::MAX_HISTORY),
            );
            ok(r)
        }
        "redis.command" => {
            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Command {
                session_id: String,
                line: String,
                #[serde(default)]
                confirmed: bool,
            }
            let Command { session_id, line, confirmed } = p(&params)?;
            let s = st.session(&session_id).await?;
            let res = s.redis().await?.command(&line, confirmed).await;
            let (dur, err) = match &res {
                Ok(v) => (v.get("durationMs").and_then(Value::as_f64).unwrap_or(0.0), v["reply"]["t"].as_str().filter(|t| *t == "error").map(|_| v["reply"]["v"].as_str().unwrap_or("error").to_string())),
                Err(e) => (0.0, Some(e.message.clone())),
            };
            if !matches!(&res, Err(e) if e.code.as_deref() == Some("confirm_required")) {
                let _ = st.store.add_history(
                    &HistoryEntry {
                        id: 0,
                        connection_id: s.connection.id.clone(),
                        database: s.info.database.clone(),
                        sql: line.clone(),
                        duration_ms: dur,
                        rows: None,
                        error: err,
                        created_at: 0,
                    },
                    Some(license::MAX_HISTORY),
                );
            }
            ok(res?)
        }

        // ----------------------------------------------------------- mongodb
        m if m.starts_with("mongo.") => {
            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct MongoReq {
                session_id: String,
                #[serde(default)]
                db: String,
                #[serde(default)]
                collection: String,
                #[serde(default)]
                filter: String,
                #[serde(default)]
                sort: String,
                #[serde(default)]
                projection: String,
                #[serde(default)]
                skip: u64,
                #[serde(default)]
                limit: Option<i64>,
                #[serde(default)]
                pipeline: String,
                #[serde(default)]
                docs: Vec<String>,
                #[serde(default)]
                id: String,
                #[serde(default)]
                doc: String,
                #[serde(default)]
                ids: Vec<String>,
                #[serde(default)]
                command: String,
                #[serde(default)]
                confirmed: bool,
                #[serde(default)]
                name: String,
            }
            let r: MongoReq = p(&params)?;
            let s = st.session(&r.session_id).await?;
            let m = s.mongo().await?;
            let db = if r.db.is_empty() { m.default_db.clone() } else { r.db.clone() };
            let history = |sql: String, rows: Option<i64>| {
                let _ = st.store.add_history(
                    &HistoryEntry {
                        id: 0,
                        connection_id: s.connection.id.clone(),
                        database: db.clone(),
                        sql,
                        duration_ms: 0.0,
                        rows,
                        error: None,
                        created_at: 0,
                    },
                    Some(license::MAX_HISTORY),
                );
            };
            let out = match m {
                _ if method == "mongo.databases" => json!(s.databases().await?),
                _ if method == "mongo.collections" => m.collections(&db).await?,
                _ if method == "mongo.find" => {
                    m.find(&db, &r.collection, &r.filter, &r.sort, &r.projection, r.skip, r.limit.unwrap_or(50)).await?
                }
                _ if method == "mongo.aggregate" => m.aggregate(&db, &r.collection, &r.pipeline).await?,
                _ if method == "mongo.indexes" => m.indexes(&db, &r.collection).await?,
                _ if method == "mongo.insert" => {
                    let v = m.insert(&db, &r.collection, &r.docs).await?;
                    history(format!("// insert into {}.{}", db, r.collection), v["inserted"].as_i64());
                    v
                }
                _ if method == "mongo.replace" => {
                    let v = m.replace(&db, &r.collection, &r.id, &r.doc).await?;
                    history(format!("// replace {} in {}.{}", r.id, db, r.collection), Some(1));
                    v
                }
                _ if method == "mongo.delete" => {
                    let v = m.delete(&db, &r.collection, &r.ids).await?;
                    history(format!("// delete {} document(s) from {}.{}", r.ids.len(), db, r.collection), v["deleted"].as_i64());
                    v
                }
                _ if method == "mongo.createCollection" => {
                    m.create_collection(&db, &r.name).await?;
                    history(format!("db.createCollection({:?})", r.name), None);
                    json!({ "ok": true })
                }
                _ if method == "mongo.dropCollection" => {
                    m.drop_collection(&db, &r.name).await?;
                    history(format!("db.{}.drop()", r.name), None);
                    json!({ "ok": true })
                }
                _ if method == "mongo.command" => {
                    let v = m.command(&db, &r.command, r.confirmed).await?;
                    if v["write"].as_bool() == Some(true) {
                        history(format!("db.runCommand({})", r.command.trim()), None);
                    }
                    v
                }
                _ => return Err(AppError::invalid(format!("Unknown method: {method}"))),
            };
            ok(out)
        }

        // ------------------------------------------------------------- query
        "query.execute" => {
            let ExecuteParam {
                session_id,
                statements,
                limit,
                stop_on_error,
                record_history,
                channel,
            } = p(&params)?;
            let session = st.session(&session_id).await?;
            let ch = if channel.as_deref() == Some("aux") {
                Channel::Aux
            } else {
                Channel::Editor
            };
            let limit = limit.unwrap_or(2000).max(1);
            let keep = Some(license::MAX_HISTORY);
            let mut outcomes = Vec::new();
            for sql in statements {
                // Read-only connections: refuse writes in the core as well as at
                // the database session level (which a user could switch off).
                if session.info.read_only && crate::safety::is_write_for(session.engine(), &sql) {
                    outcomes.push(StatementOutcome {
                        sql,
                        result: None,
                        error: Some(
                            AppError::invalid("This connection is read-only, so Rowglow blocked this statement. Edit the connection to allow writes.")
                                .with_code("read_only"),
                        ),
                    });
                    break;
                }
                let res = session.execute(ch, &sql, limit).await;
                if record_history {
                    let (dur, rows, err) = match &res {
                        Ok(r) => (
                            r.duration_ms,
                            if r.has_rows {
                                Some(r.rows.len() as i64)
                            } else {
                                r.rows_affected.map(|n| n as i64)
                            },
                            None,
                        ),
                        Err(e) => (0.0, None, Some(e.message.clone())),
                    };
                    let _ = st.store.add_history(
                        &HistoryEntry {
                            id: 0,
                            connection_id: session.connection.id.clone(),
                            database: session.info.database.clone(),
                            sql: sql.clone(),
                            duration_ms: dur,
                            rows,
                            error: err,
                            created_at: 0,
                        },
                        keep,
                    );
                }
                match res {
                    Ok(r) => outcomes.push(StatementOutcome {
                        sql,
                        result: Some(r),
                        error: None,
                    }),
                    Err(e) => {
                        let stop = stop_on_error || e.kind == "connection" || e.kind == "cancelled";
                        outcomes.push(StatementOutcome {
                            sql,
                            result: None,
                            error: Some(e),
                        });
                        if stop {
                            break;
                        }
                    }
                }
            }
            ok(outcomes)
        }
        "query.transaction" => {
            let TxParam {
                session_id,
                statements,
                expect_one_row,
            } = p(&params)?;
            let session = st.session(&session_id).await?;
            if session.info.read_only {
                return Err(AppError::invalid("This connection is read-only"));
            }
            if statements.is_empty() {
                return ok(json!({ "ok": true, "results": [] }));
            }
            let res = session.transaction(&statements, expect_one_row).await;
            let summary = statements.join(";\n");
            let (ok_flag, payload) = match res {
                Ok(results) => (
                    true,
                    json!({ "ok": true, "results": results }),
                ),
                Err((idx, e)) => (
                    false,
                    json!({ "ok": false, "failedIndex": idx, "error": e }),
                ),
            };
            let _ = st.store.add_history(
                &HistoryEntry {
                    id: 0,
                    connection_id: session.connection.id.clone(),
                    database: session.info.database.clone(),
                    sql: format!("-- Applied staged changes\n{summary};"),
                    duration_ms: 0.0,
                    rows: Some(statements.len() as i64),
                    error: if ok_flag {
                        None
                    } else {
                        payload["error"]["message"].as_str().map(str::to_string)
                    },
                    created_at: 0,
                },
                Some(license::MAX_HISTORY),
            );
            ok(payload)
        }
        "query.cancel" => {
            let SessionParam { session_id } = p(&params)?;
            st.session(&session_id).await?.cancel().await?;
            ok(json!({ "ok": true }))
        }
        "query.classify" => {
            #[derive(Deserialize)]
            struct Sql {
                sql: String,
            }
            let Sql { sql } = p(&params)?;
            ok(json!({ "keyword": first_keyword(&sql) }))
        }

        // ---------------------------------------------------- history/snippets
        "history.list" => {
            let HistoryParam {
                connection_id,
                search,
                limit,
            } = p(&params)?;
            // The free plan shows the newest FREE_HISTORY entries; everything stays on disk.
            let limit = limit.unwrap_or(200);
            let limit = if license::has_pro(st) { limit } else { limit.min(license::FREE_HISTORY) };
            ok(st.store.list_history(connection_id.as_deref(), search.as_deref().filter(|s| !s.is_empty()), limit)?)
        }
        "history.clear" => {
            let HistoryParam { connection_id, .. } = p(&params)?;
            st.store.clear_history(connection_id.as_deref())?;
            ok(json!({ "ok": true }))
        }
        "snippets.list" => ok(st.store.list_snippets()?),
        "snippets.save" => {
            let SnippetParam { mut snippet } = p(&params)?;
            if snippet.name.trim().is_empty() {
                return Err(AppError::invalid("Give the query a name"));
            }
            let is_new = snippet.id.is_empty() || !st.store.snippet_exists(&snippet.id)?;
            if is_new && !license::has_pro(st) && st.store.count_snippets()? >= license::FREE_SNIPPETS as i64 {
                return Err(AppError::license(format!(
                    "The free plan includes {} saved queries. Upgrade to Rowglow Pro for unlimited.",
                    license::FREE_SNIPPETS
                ))
                .with_code("limit_snippets"));
            }
            st.store.save_snippet(&mut snippet)?;
            ok(snippet)
        }
        "snippets.delete" => {
            let IdParam { id } = p(&params)?;
            st.store.delete_snippet(&id)?;
            ok(json!({ "ok": true }))
        }

        // ------------------------------------------------------------ export
        "export.table" => {
            let ExportParam {
                session_id,
                sql,
                path,
                format,
                schema,
                table,
            } = p(&params)?;
            if !license::has_pro(st) {
                return Err(AppError::license(
                    "Exporting whole tables is a Rowglow Pro feature. You can still copy or export loaded rows.",
                )
                .with_code("limit_export"));
            }
            let session = st.session(&session_id).await?;
            let rows = export::export(
                &session,
                &sql,
                std::path::Path::new(&path),
                &format,
                &schema,
                &table,
            )
            .await?;
            ok(json!({ "rows": rows, "path": path }))
        }
        "import.preview" => {
            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Preview {
                path: String,
                #[serde(default)]
                delimiter: Option<String>,
                #[serde(default)]
                has_header: Option<bool>,
            }
            let Preview { path, delimiter, has_header } = p(&params)?;
            ok(tokio::task::spawn_blocking(move || crate::import::preview(&path, delimiter.as_deref(), has_header)).await??)
        }
        "import.sqlType" => {
            #[derive(Deserialize)]
            struct T {
                engine: Engine,
                kind: String,
            }
            let T { engine, kind } = p(&params)?;
            ok(crate::import::sql_type(engine, &kind))
        }
        "import.run" => {
            let req: crate::import::ImportRequest = p(&params)?;
            let session = st.session(&req.session_id).await?;
            if session.info.read_only {
                return Err(AppError::invalid("This connection is read-only, so Rowglow can't import into it.").with_code("read_only"));
            }
            let max = if license::has_pro(st) { None } else { Some(license::FREE_IMPORT_ROWS as u64) };
            let summary = format!("-- Imported {} into {}.{}", req.path, req.schema, req.table);
            let rows = crate::import::run(&session, req, max).await?;
            let _ = st.store.add_history(
                &HistoryEntry {
                    id: 0,
                    connection_id: session.connection.id.clone(),
                    database: session.info.database.clone(),
                    sql: summary,
                    duration_ms: 0.0,
                    rows: Some(rows as i64),
                    error: None,
                    created_at: 0,
                },
                Some(license::MAX_HISTORY),
            );
            ok(json!({ "rows": rows }))
        }
        "import.progress" => {
            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Job {
                job_id: String,
            }
            let Job { job_id } = p(&params)?;
            let (done, total) = crate::import::progress(&job_id).unwrap_or((0, 0));
            ok(json!({ "done": done, "total": total }))
        }
        "import.cancel" => {
            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Job {
                job_id: String,
            }
            let Job { job_id } = p(&params)?;
            crate::import::cancel(&job_id);
            ok(json!({ "ok": true }))
        }
        "fs.writeText" => {
            let WriteParam { path, contents } = p(&params)?;
            tokio::fs::write(&path, contents).await?;
            ok(json!({ "ok": true }))
        }
        "fs.writeBase64" => {
            #[derive(Deserialize)]
            struct WriteB64 {
                path: String,
                data: String,
            }
            let WriteB64 { path, data } = p(&params)?;
            use base64::Engine as _;
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(data.trim())
                .map_err(|e| AppError::invalid(format!("Invalid data: {e}")))?;
            tokio::fs::write(&path, bytes).await?;
            ok(json!({ "ok": true }))
        }
        "fs.readText" => {
            let PathParam { path } = p(&params)?;
            let meta = tokio::fs::metadata(&path).await?;
            if meta.len() > 20 * 1024 * 1024 {
                return Err(AppError::invalid("File is larger than 20 MB"));
            }
            ok(tokio::fs::read_to_string(&path).await?)
        }

        // ----------------------------------------------------------- license
        "license.status" => ok(license::status(st)),
        "license.activate" => {
            let KeyParam { key } = p(&params)?;
            ok(license::activate(st, &key).await?)
        }
        "license.deactivate" => ok(license::deactivate(st).await?),
        "license.refresh" => ok(license::refresh(st).await?),
        "updates.check" => ok(license::check_updates(st).await?),

        _ => Err(AppError::invalid(format!("Unknown method: {method}"))),
    }
}
