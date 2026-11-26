//! AI assistant: writes SQL from a plain-English request, explains a query,
//! or fixes one that failed — with the user's own API key for Claude,
//! ChatGPT, Gemini, Mistral, DeepSeek, Grok, Groq or OpenRouter, any
//! OpenAI-compatible server, or a local model served by Ollama.
//!
//! Privacy and safety:
//! * Nothing is sent until the user sets up a provider and asks something.
//! * What is sent: the request, the database's table and column names, types
//!   and keys (the schema), and — for explain/fix — the SQL and the error
//!   message. Never row data.
//! * The answer is shown to the user, who inserts it into the editor and runs
//!   it themselves. The assistant can't run anything, and the usual guards
//!   (read-only connections, confirmations for destructive statements and
//!   production writes) still apply when they do.
//! * API keys live in the system keychain, like connection passwords, one
//!   per provider.

use crate::drivers::Session;
use crate::error::{AppError, AppResult};
use crate::model::*;
use crate::state::AppState;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

pub const ANTHROPIC_BASE: &str = "https://api.anthropic.com";
pub const ANTHROPIC_VERSION: &str = "2026-10-21";
/// Server-side refusal fallbacks (`fallbacks: "default"`).
pub const FALLBACK_BETA: &str = "server-side-fallback-2026-07-01";
/// Claude Haiku: Anthropic's fastest, lowest-cost model.
pub const DEFAULT_MODEL: &str = "claude-haiku-5-5";
/// Keychain id of the Anthropic key (other providers: `ai:<provider>`).
pub const KEY_SECRET: &str = "ai:anthropic";
pub const OLLAMA_DEFAULT: &str = "http://127.0.0.1:11434";

/// Claude models offered in Settings, cheapest first.
pub const CLAUDE_MODELS: &[(&str, &str, &str)] = &[
    ("claude-haiku-5-5", "Claude Haiku 5.5", "Fastest, lowest cost"),
    ("claude-sonnet-5-5", "Claude Sonnet 5.5", "Balanced"),
    ("claude-opus-5-5", "Claude Opus 5.5", "Most capable"),
];

/// How Rowglow talks to a provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Wire {
    /// Anthropic Messages API.
    Anthropic,
    /// OpenAI Chat Completions, or a server that speaks it.
    Openai,
    /// Google Gemini generateContent.
    Gemini,
    /// Ollama's /api/chat.
    Ollama,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Provider {
    pub id: &'static str,
    pub name: &'static str,
    pub wire: Wire,
    /// API base URL ("" = the user enters it).
    pub base: &'static str,
    /// Where to create an API key.
    pub keys_url: &'static str,
    pub needs_key: bool,
    /// Runs on this computer.
    pub local: bool,
}

pub const PROVIDERS: &[Provider] = &[
    Provider { id: "anthropic", name: "Claude (Anthropic)", wire: Wire::Anthropic, base: ANTHROPIC_BASE, keys_url: "https://console.anthropic.com/settings/keys", needs_key: true, local: false },
    Provider { id: "openai", name: "ChatGPT (OpenAI)", wire: Wire::Openai, base: "https://api.openai.com/v1", keys_url: "https://platform.openai.com/api-keys", needs_key: true, local: false },
    Provider { id: "gemini", name: "Gemini (Google)", wire: Wire::Gemini, base: "https://generativelanguage.googleapis.com/v1beta", keys_url: "https://aistudio.google.com/apikey", needs_key: true, local: false },
    Provider { id: "mistral", name: "Mistral", wire: Wire::Openai, base: "https://api.mistral.ai/v1", keys_url: "https://console.mistral.ai/api-keys", needs_key: true, local: false },
    Provider { id: "deepseek", name: "DeepSeek", wire: Wire::Openai, base: "https://api.deepseek.com/v1", keys_url: "https://platform.deepseek.com/api_keys", needs_key: true, local: false },
    Provider { id: "xai", name: "Grok (xAI)", wire: Wire::Openai, base: "https://api.x.ai/v1", keys_url: "https://console.x.ai", needs_key: true, local: false },
    Provider { id: "groq", name: "Groq", wire: Wire::Openai, base: "https://api.groq.com/openai/v1", keys_url: "https://console.groq.com/keys", needs_key: true, local: false },
    Provider { id: "openrouter", name: "OpenRouter", wire: Wire::Openai, base: "https://openrouter.ai/api/v1", keys_url: "https://openrouter.ai/settings/keys", needs_key: true, local: false },
    Provider { id: "custom", name: "Other (OpenAI-compatible)", wire: Wire::Openai, base: "", keys_url: "", needs_key: false, local: false },
    Provider { id: "ollama", name: "Ollama (on this computer)", wire: Wire::Ollama, base: OLLAMA_DEFAULT, keys_url: "", needs_key: false, local: true },
];

pub fn provider(id: &str) -> AppResult<&'static Provider> {
    PROVIDERS.iter().find(|p| p.id == id).ok_or_else(|| AppError::invalid(format!("Unknown AI provider “{id}”")))
}

/// Keychain id for a provider's API key.
pub fn key_secret(provider: &str) -> String {
    if provider == "anthropic" { KEY_SECRET.to_string() } else { format!("ai:{provider}") }
}

/// Largest schema description sent with a request (~80k tokens).
const MAX_SCHEMA_CHARS: usize = 320_000;
/// Largest editor text sent along with a request.
const MAX_SQL_CHARS: usize = 40_000;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct AiSettings {
    /// A `PROVIDERS` id.
    pub provider: String,
    /// Claude model ID.
    pub model: String,
    pub ollama_url: String,
    pub ollama_model: String,
    /// Chosen model for the other providers, by provider id.
    pub models: std::collections::BTreeMap<String, String>,
    /// Base URL for "custom" (OpenAI-compatible).
    pub custom_url: String,
}

impl Default for AiSettings {
    fn default() -> Self {
        AiSettings {
            provider: "anthropic".into(),
            model: DEFAULT_MODEL.into(),
            ollama_url: OLLAMA_DEFAULT.into(),
            ollama_model: String::new(),
            models: Default::default(),
            custom_url: String::new(),
        }
    }
}

impl AiSettings {
    /// The model to use with `provider`.
    pub fn model_for(&self, provider: &str) -> String {
        match provider {
            "anthropic" => self.model.clone(),
            "ollama" => self.ollama_model.clone(),
            p => self.models.get(p).cloned().unwrap_or_default(),
        }
    }

    /// The API base URL for `provider` (debug builds can override it with
    /// ROWGLOW_AI_BASE_<ID>, used by the tests).
    pub fn base_for(&self, provider: &str) -> String {
        #[cfg(debug_assertions)]
        {
            let var = if provider == "anthropic" {
                "ROWGLOW_ANTHROPIC_BASE".to_string()
            } else {
                format!("ROWGLOW_AI_BASE_{}", provider.to_uppercase())
            };
            if let Ok(b) = std::env::var(var) {
                if !b.trim().is_empty() {
                    return b.trim().trim_end_matches('/').to_string();
                }
            }
        }
        let base = match provider {
            "ollama" => self.ollama_url.as_str(),
            "custom" => self.custom_url.as_str(),
            p => PROVIDERS.iter().find(|x| x.id == p).map(|x| x.base).unwrap_or(""),
        };
        base.trim().trim_end_matches('/').to_string()
    }
}

pub fn load_settings(st: &AppState) -> AiSettings {
    let mut s: AiSettings = st
        .store
        .kv_get("ai")
        .ok()
        .flatten()
        .and_then(|v| serde_json::from_str(&v).ok())
        .unwrap_or_default();
    if !CLAUDE_MODELS.iter().any(|m| m.0 == s.model) {
        s.model = DEFAULT_MODEL.into();
    }
    if s.ollama_url.trim().is_empty() {
        s.ollama_url = OLLAMA_DEFAULT.into();
    }
    if provider(&s.provider).is_err() {
        s.provider = "anthropic".into();
    }
    s
}

fn valid_url(u: &str) -> bool {
    let u = u.trim();
    (u.starts_with("http://") || u.starts_with("https://")) && u.len() > 10
}

pub fn save_settings(st: &AppState, s: &AiSettings) -> AppResult<()> {
    provider(&s.provider)?;
    if s.provider == "ollama" && !valid_url(&s.ollama_url) {
        return Err(AppError::invalid("The Ollama address should start with http:// or https://"));
    }
    if s.provider == "custom" && !valid_url(&s.custom_url) {
        return Err(AppError::invalid("Enter the server's API address, like https://example.com/v1"));
    }
    if s.provider == "anthropic" && !CLAUDE_MODELS.iter().any(|m| m.0 == s.model) {
        return Err(AppError::invalid("Choose a Claude model"));
    }
    st.store.kv_set("ai", &serde_json::to_string(s)?)
}

fn http() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(15))
            // A stream can take a while, but it should never go quiet for long.
            .read_timeout(Duration::from_secs(180))
            .user_agent(concat!("Rowglow/", env!("CARGO_PKG_VERSION")))
            .build()
            .expect("http client")
    })
}

// ---------------------------------------------------------------- daily use

fn today() -> String {
    chrono::Utc::now().format("%Y-%m-%d").to_string()
}

/// Requests made today (UTC).
pub fn used_today(st: &AppState) -> u32 {
    let v: Value = st
        .store
        .kv_get("ai.usage")
        .ok()
        .flatten()
        .and_then(|v| serde_json::from_str(&v).ok())
        .unwrap_or(Value::Null);
    if v["date"].as_str() == Some(today().as_str()) {
        v["count"].as_u64().unwrap_or(0) as u32
    } else {
        0
    }
}

fn count_request(st: &AppState) {
    let n = used_today(st) + 1;
    let _ = st.store.kv_set("ai.usage", &json!({ "date": today(), "count": n }).to_string());
}

// ------------------------------------------------------------------ prompts

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Task {
    Generate,
    Explain,
    Fix,
}

pub fn dialect(engine: Engine, flavor: &str) -> &'static str {
    match engine {
        Engine::Postgres => "PostgreSQL",
        Engine::Mysql if flavor.to_lowercase().contains("maria") => "MariaDB",
        Engine::Mysql => "MySQL",
        Engine::Sqlite => "SQLite",
        Engine::Mssql => "SQL Server (T-SQL)",
        Engine::Duckdb => "DuckDB",
        Engine::Redis => "Redis",
        Engine::Mongodb => "MongoDB",
    }
}

const SYSTEM_PROMPT: &str = r#"You are the SQL assistant inside Rowglow, a desktop database client. Your answer is shown to the user, who reviews it and decides whether to run it; you never see their data.

Reply with JSON matching the given schema: "sql" is the SQL to put in the user's editor and "explanation" is plain language for the user.

When asked to write SQL:
- Use the dialect and only the tables and columns described in the database section. Qualify table names with the schema when the database uses schemas, and quote identifiers that need quoting.
- Prefer a single read-only SELECT. Write INSERT, UPDATE, DELETE or DDL only when the request clearly asks to change data or structure, and then say in the explanation that it changes data.
- Never add statements nobody asked for, such as DROP, TRUNCATE or cleanup.
- For SELECTs that could return a lot of rows, limit them to 100 (LIMIT, or TOP for SQL Server) unless the user asks for everything or the query aggregates.
- If the request can't be answered from this schema, leave "sql" empty and explain what is missing.
- Keep the explanation to one to three short sentences: what the query returns and any assumption you made.

When asked to explain SQL: leave "sql" empty. In "explanation", describe what the statement does and what it returns, in a short paragraph followed by a few short lines for the important details (joins, filters, grouping, anything that changes data). Plain text, no Markdown headings.

When asked to fix SQL: return the corrected SQL in "sql" and say briefly what was wrong. Keep the user's intent and formatting where you can."#;

fn answer_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "sql": { "type": "string", "description": "SQL for the user's editor, or empty" },
            "explanation": { "type": "string", "description": "Plain-language explanation for the user" }
        },
        "required": ["sql", "explanation"],
        "additionalProperties": false
    })
}

/// Describes the current schema: tables, columns with types, primary and
/// foreign keys. Names only — never data. Very large schemas keep full detail
/// for the tables named in `hint` (and their neighbours) and list the rest by name.
pub fn describe_schema(
    dialect: &str,
    database: &str,
    schema: &str,
    tables: &[CompletionTable],
    rel: &SchemaRelations,
    hint: &str,
) -> (String, usize) {
    let pks: HashMap<&str, &Vec<String>> = rel.primary_keys.iter().map(|p| (p.table.as_str(), &p.columns)).collect();
    let mut fks: HashMap<(&str, &str), String> = HashMap::new();
    for fk in &rel.foreign_keys {
        if fk.columns.len() == 1 {
            let target = if fk.ref_schema.is_empty() || fk.ref_schema == schema {
                format!("{}.{}", fk.ref_table, fk.ref_columns.first().cloned().unwrap_or_default())
            } else {
                format!("{}.{}.{}", fk.ref_schema, fk.ref_table, fk.ref_columns.first().cloned().unwrap_or_default())
            };
            fks.insert((fk.table.as_str(), fk.columns[0].as_str()), target);
        }
    }
    let describe = |t: &CompletionTable| -> String {
        let pk = pks.get(t.name.as_str());
        let cols: Vec<String> = t
            .columns
            .iter()
            .map(|(name, ty)| {
                let mut c = if ty.trim().is_empty() { name.clone() } else { format!("{name} {ty}") };
                if pk.is_some_and(|p| p.len() == 1 && p[0] == *name) {
                    c.push_str(" PK");
                }
                if let Some(target) = fks.get(&(t.name.as_str(), name.as_str())) {
                    c.push_str(&format!(" -> {target}"));
                }
                c
            })
            .collect();
        let mut line = format!("- {} ({}): {}", t.name, t.kind, cols.join(", "));
        if let Some(p) = pk.filter(|p| p.len() > 1) {
            line.push_str(&format!("; primary key ({})", p.join(", ")));
        }
        for fk in rel.foreign_keys.iter().filter(|f| f.table == t.name && f.columns.len() > 1) {
            line.push_str(&format!("; ({}) -> {}({})", fk.columns.join(", "), fk.ref_table, fk.ref_columns.join(", ")));
        }
        line
    };

    let mut head = format!("Database: {dialect}.");
    if !database.is_empty() {
        head.push_str(&format!(" Current database: {database}."));
    }
    if !schema.is_empty() && schema != database {
        head.push_str(&format!(" Current schema: {schema}."));
    }
    head.push_str("\nTables and views (\"name type\", PK = primary key, -> = foreign key):\n");

    let full: Vec<String> = tables.iter().map(describe).collect();
    let total: usize = full.iter().map(|l| l.len() + 1).sum();
    if total + head.len() <= MAX_SCHEMA_CHARS {
        return (format!("{head}{}", full.join("\n")), tables.len());
    }
    // Too big: tables mentioned in the request, plus the tables they link to, in full.
    let words: HashSet<String> = hint
        .to_lowercase()
        .split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .filter(|w| w.len() > 2)
        .map(str::to_string)
        .collect();
    let mut wanted: HashSet<&str> = tables
        .iter()
        .filter(|t| {
            let n = t.name.to_lowercase();
            words.contains(&n) || words.iter().any(|w| n.contains(w.as_str()) && w.len() > 3)
        })
        .map(|t| t.name.as_str())
        .collect();
    let linked: Vec<&str> = rel
        .foreign_keys
        .iter()
        .filter_map(|f| {
            if wanted.contains(f.table.as_str()) {
                Some(f.ref_table.as_str())
            } else if wanted.contains(f.ref_table.as_str()) {
                Some(f.table.as_str())
            } else {
                None
            }
        })
        .collect();
    wanted.extend(linked);
    let mut out = head.clone();
    let mut names_only = Vec::new();
    for (t, line) in tables.iter().zip(&full) {
        if wanted.contains(t.name.as_str()) && out.len() + line.len() < MAX_SCHEMA_CHARS * 3 / 4 {
            out.push_str(line);
            out.push('\n');
        } else {
            names_only.push(t.name.as_str());
        }
    }
    if !names_only.is_empty() {
        out.push_str("Other tables (columns not listed): ");
        let mut listed = 0;
        for n in &names_only {
            if out.len() + n.len() + 2 > MAX_SCHEMA_CHARS {
                break;
            }
            if listed > 0 {
                out.push_str(", ");
            }
            out.push_str(n);
            listed += 1;
        }
        if listed < names_only.len() {
            out.push_str(&format!(" … and {} more", names_only.len() - listed));
        }
    }
    (out, tables.len())
}

pub async fn schema_context(s: &Session, schema: &str, hint: &str) -> AppResult<(String, usize)> {
    let engine = s.engine();
    let tables: Vec<CompletionTable> = s
        .completions(Some(schema))
        .await?
        .into_iter()
        .filter(|t| t.schema == schema || (engine == Engine::Sqlite && t.schema == "main"))
        .collect();
    let rel = s.relations(schema).await.unwrap_or_default();
    Ok(describe_schema(dialect(engine, &s.info.flavor), &s.info.database, schema, &tables, &rel, hint))
}

fn clip(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

pub fn user_message(task: Task, question: &str, sql: &str, error: &str) -> String {
    let sql = clip(sql.trim(), MAX_SQL_CHARS);
    match task {
        Task::Generate => {
            let mut m = format!("Write SQL for this request:\n{}", question.trim());
            if !sql.is_empty() {
                m.push_str(&format!(
                    "\n\nThe editor currently contains this SQL (use it if the request refers to it):\n```sql\n{sql}\n```"
                ));
            }
            m
        }
        Task::Explain => format!("Explain this SQL:\n```sql\n{sql}\n```"),
        Task::Fix => {
            let mut m = format!("This SQL failed:\n```sql\n{sql}\n```\n\nError from the database:\n{}", clip(error.trim(), 4000));
            if !question.trim().is_empty() {
                m.push_str(&format!("\n\nThe user adds: {}", question.trim()));
            }
            m
        }
    }
}

// --------------------------------------------------------------------- jobs

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JobState {
    /// "thinking" | "writing" | "done" | "error" | "cancelled"
    pub status: String,
    #[serde(skip)]
    pub text: String,
    pub sql: Option<String>,
    pub explanation: Option<String>,
    pub error: Option<String>,
    pub model: Option<String>,
    pub stop_reason: Option<String>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub elapsed_ms: u64,
}

pub struct Job {
    state: Mutex<JobState>,
    abort: Mutex<Option<tokio::task::AbortHandle>>,
    started: Instant,
}

fn jobs() -> &'static Mutex<HashMap<String, Arc<Job>>> {
    static JOBS: OnceLock<Mutex<HashMap<String, Arc<Job>>>> = OnceLock::new();
    JOBS.get_or_init(|| Mutex::new(HashMap::new()))
}

impl Job {
    fn update(&self, f: impl FnOnce(&mut JobState)) {
        if let Ok(mut s) = self.state.lock() {
            f(&mut s);
        }
    }
}

/// The string value of `field` in a JSON object that may still be streaming
/// in (the closing quote may not have arrived yet).
pub fn partial_field(text: &str, field: &str) -> Option<String> {
    let key = format!("\"{field}\"");
    let at = text.find(&key)? + key.len();
    let rest = text[at..].trim_start().strip_prefix(':')?.trim_start().strip_prefix('"')?;
    let mut out = String::new();
    let mut chars = rest.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => return Some(out),
            '\\' => match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => out.push('\r'),
                Some('b') => out.push('\u{8}'),
                Some('f') => out.push('\u{c}'),
                Some('u') => {
                    let hex: String = chars.by_ref().take(4).collect();
                    if hex.len() < 4 {
                        return Some(out);
                    }
                    if let Some(ch) = u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                        out.push(ch);
                    }
                }
                Some(other) => out.push(other),
                None => return Some(out),
            },
            c => out.push(c),
        }
    }
    Some(out)
}

#[derive(Deserialize)]
struct Answer {
    #[serde(default)]
    sql: String,
    #[serde(default)]
    explanation: String,
}

/// Parses the model's JSON answer; tolerates a Markdown fence or text around
/// it (local models sometimes add one).
pub fn parse_answer(text: &str) -> Option<(String, String)> {
    let t = text.trim();
    let parsed = serde_json::from_str::<Answer>(t).ok().or_else(|| {
        let start = t.find('{')?;
        let end = t.rfind('}')?;
        serde_json::from_str::<Answer>(&t[start..=end]).ok()
    })?;
    let mut sql = parsed.sql.trim().to_string();
    // Strip a ```sql fence inside the field.
    if sql.starts_with("```") {
        sql = sql.trim_start_matches("```sql").trim_start_matches("```").trim_end_matches("```").trim().to_string();
    }
    Some((sql, parsed.explanation.trim().to_string()))
}

pub fn poll(id: &str) -> Option<JobState> {
    let job = jobs().lock().ok()?.get(id).cloned()?;
    let mut s = job.state.lock().ok()?.clone();
    s.elapsed_ms = job.started.elapsed().as_millis() as u64;
    if s.status == "writing" {
        s.sql = partial_field(&s.text, "sql");
        s.explanation = partial_field(&s.text, "explanation");
    }
    Some(s)
}

pub fn cancel(id: &str) -> bool {
    let Some(job) = jobs().lock().ok().and_then(|j| j.get(id).cloned()) else {
        return false;
    };
    if let Some(h) = job.abort.lock().ok().and_then(|mut a| a.take()) {
        h.abort();
    }
    job.update(|s| {
        if s.status != "done" && s.status != "error" {
            s.status = "cancelled".into();
        }
    });
    true
}

pub struct Request {
    pub settings: AiSettings,
    pub api_key: Option<String>,
    pub schema_text: String,
    pub user: String,
}

/// Starts a request in the background; poll it with `poll(id)`.
pub fn start(st: &AppState, req: Request) -> String {
    let id = crate::util::new_id();
    let job = Arc::new(Job {
        state: Mutex::new(JobState { status: "thinking".into(), ..Default::default() }),
        abort: Mutex::new(None),
        started: Instant::now(),
    });
    if let Ok(mut all) = jobs().lock() {
        // Forget finished jobs after 10 minutes.
        all.retain(|_, j| j.started.elapsed() < Duration::from_secs(600));
        all.insert(id.clone(), job.clone());
    }
    count_request(st);
    let j = job.clone();
    let handle = tokio::spawn(async move {
        let result = run(&j, &req).await;
        if let Err(e) = result {
            j.update(|s| {
                s.status = "error".into();
                s.error = Some(e.message);
            });
        }
    });
    if let Ok(mut a) = job.abort.lock() {
        *a = Some(handle.abort_handle());
    }
    id
}

async fn run(job: &Job, req: &Request) -> AppResult<()> {
    let s = &req.settings;
    let p = provider(&s.provider)?;
    let key = req.api_key.as_deref().unwrap_or("");
    let model = s.model_for(p.id);
    let base = s.base_for(p.id);
    match p.wire {
        Wire::Anthropic => run_anthropic(job, &base, key, &model, &req.schema_text, &req.user).await,
        Wire::Ollama => run_ollama(job, s, &req.schema_text, &req.user).await,
        Wire::Gemini => run_gemini(job, &base, key, &model, &req.schema_text, &req.user).await,
        Wire::Openai => run_openai(job, p, &base, key, &model, &req.schema_text, &req.user).await,
    }
}

fn finish(job: &Job) -> AppResult<()> {
    let text = job.state.lock().map(|s| s.text.clone()).unwrap_or_default();
    match parse_answer(&text) {
        Some((sql, explanation)) => {
            job.update(|s| {
                s.status = "done".into();
                s.sql = Some(sql);
                s.explanation = Some(explanation);
            });
            Ok(())
        }
        None => Err(AppError::new("ai", "The model's answer wasn't in the expected format. Try again.")),
    }
}

// ---------------------------------------------------------------- Anthropic

fn anthropic_error(status: reqwest::StatusCode, body: &str) -> AppError {
    let v: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    let msg = v["error"]["message"].as_str().unwrap_or("").to_string();
    let text = match status.as_u16() {
        401 => "Anthropic didn't accept the API key. Check it in Settings → AI.".to_string(),
        403 => format!("This API key isn't allowed to use that model. {msg}"),
        404 => format!("Anthropic doesn't know that model. {msg}"),
        413 => "The request is too large. Try a smaller schema or less SQL.".to_string(),
        429 => "Anthropic's rate limit was reached. Wait a moment and try again.".to_string(),
        500..=599 if status.as_u16() == 529 || msg.to_lowercase().contains("overloaded") => {
            "Anthropic is busy right now. Try again in a moment.".to_string()
        }
        _ => format!("Anthropic returned an error ({status}). {msg}"),
    };
    AppError::new("ai", text.trim().to_string())
}

/// Models that take `fallbacks: "default"` (Claude Haiku has no server-side fallback).
fn uses_fallbacks(model: &str) -> bool {
    matches!(model, "claude-opus-5-5" | "claude-sonnet-5-5")
}

pub fn anthropic_body(model: &str, schema_text: &str, user: &str) -> Value {
    let mut body = json!({
        "model": model,
        "max_tokens": 64000,
        "stream": true,
        "thinking": { "type": "adaptive" },
        "output_config": {
            "effort": "medium",
            "format": { "type": "json_schema", "schema": answer_schema() }
        },
        "system": [
            { "type": "text", "text": SYSTEM_PROMPT },
            // The schema rarely changes between questions, so cache it.
            { "type": "text", "text": schema_text, "cache_control": { "type": "ephemeral" } }
        ],
        "messages": [{ "role": "user", "content": user }]
    });
    if uses_fallbacks(model) {
        body["fallbacks"] = json!("default");
    }
    body
}

pub async fn run_anthropic(job: &Job, base: &str, key: &str, model: &str, schema_text: &str, user: &str) -> AppResult<()> {
    if key.trim().is_empty() {
        return Err(AppError::invalid("Add your Anthropic API key in Settings → AI first").with_code("ai_not_configured"));
    }
    let mut req = http()
        .post(format!("{base}/v1/messages"))
        .header("x-api-key", key.trim())
        .header("anthropic-version", ANTHROPIC_VERSION)
        .json(&anthropic_body(model, schema_text, user));
    if uses_fallbacks(model) {
        req = req.header("anthropic-beta", FALLBACK_BETA);
    }
    let mut res = req
        .send()
        .await
        .map_err(|e| AppError::new("network", format!("Couldn't reach Anthropic: {e}")))?;
    if !res.status().is_success() {
        let status = res.status();
        let body = res.text().await.unwrap_or_default();
        return Err(anthropic_error(status, &body));
    }
    let mut buf: Vec<u8> = Vec::new();
    let mut stop_reason: Option<String> = None;
    let mut refusal_category: Option<String> = None;
    loop {
        let chunk = res
            .chunk()
            .await
            .map_err(|e| AppError::new("network", format!("The connection to Anthropic dropped: {e}")))?;
        let Some(chunk) = chunk else { break };
        buf.extend_from_slice(&chunk);
        // Events are separated by a blank line.
        while let Some(pos) = find_event_end(&buf) {
            let raw: Vec<u8> = buf.drain(..pos.0).collect();
            buf.drain(..pos.1);
            let text = String::from_utf8_lossy(&raw);
            let data: String = text
                .lines()
                .filter_map(|l| l.strip_prefix("data:"))
                .map(|l| l.strip_prefix(' ').unwrap_or(l))
                .collect::<Vec<_>>()
                .join("\n");
            if data.is_empty() {
                continue;
            }
            let Ok(ev) = serde_json::from_str::<Value>(&data) else { continue };
            match ev["type"].as_str().unwrap_or("") {
                "message_start" => {
                    let m = ev["message"]["model"].as_str().map(str::to_string);
                    let input = ev["message"]["usage"]["input_tokens"].as_u64();
                    job.update(|s| {
                        s.model = m;
                        s.input_tokens = input;
                    });
                }
                "content_block_start" => {
                    let kind = ev["content_block"]["type"].as_str().unwrap_or("");
                    job.update(|s| match kind {
                        "text" => s.status = "writing".into(),
                        "thinking" | "redacted_thinking" => {
                            if s.text.is_empty() {
                                s.status = "thinking".into()
                            }
                        }
                        _ => {}
                    });
                }
                "content_block_delta" => {
                    if ev["delta"]["type"] == "text_delta" {
                        if let Some(t) = ev["delta"]["text"].as_str() {
                            job.update(|s| {
                                s.status = "writing".into();
                                s.text.push_str(t);
                            });
                        }
                    }
                }
                "message_delta" => {
                    if let Some(r) = ev["delta"]["stop_reason"].as_str() {
                        stop_reason = Some(r.to_string());
                    }
                    if let Some(c) = ev["delta"]["stop_details"]["category"].as_str() {
                        refusal_category = Some(c.to_string());
                    }
                    let out = ev["usage"]["output_tokens"].as_u64();
                    job.update(|s| {
                        if out.is_some() {
                            s.output_tokens = out;
                        }
                    });
                }
                "error" => {
                    let msg = ev["error"]["message"].as_str().unwrap_or("unknown error");
                    let kind = ev["error"]["type"].as_str().unwrap_or("");
                    return Err(AppError::new(
                        "ai",
                        if kind == "overloaded_error" {
                            "Anthropic is busy right now. Try again in a moment.".to_string()
                        } else {
                            format!("Anthropic stopped with an error: {msg}")
                        },
                    ));
                }
                _ => {}
            }
        }
    }
    job.update(|s| s.stop_reason = stop_reason.clone());
    match stop_reason.as_deref() {
        Some("refusal") => Err(AppError::new(
            "ai",
            format!(
                "Claude declined this request{}. Try rephrasing it.",
                refusal_category.map(|c| format!(" ({c})")).unwrap_or_default()
            ),
        )
        .with_code("ai_refused")),
        Some("max_tokens") => Err(AppError::new("ai", "The answer was too long and got cut off. Try a narrower request.")),
        _ => finish(job),
    }
}

/// End of the first complete SSE event in `buf`: (event length, separator length).
fn find_event_end(buf: &[u8]) -> Option<(usize, usize)> {
    let lf = buf.windows(2).position(|w| w == b"\n\n").map(|p| (p, 2));
    let crlf = buf.windows(4).position(|w| w == b"\r\n\r\n").map(|p| (p, 4));
    match (lf, crlf) {
        (Some(a), Some(b)) => Some(if a.0 <= b.0 { a } else { b }),
        (a, b) => a.or(b),
    }
}

/// Checks the key and model without generating anything (no cost).
pub async fn test_anthropic(base: &str, key: &str, model: &str) -> AppResult<String> {
    if key.trim().is_empty() {
        return Err(AppError::invalid("Enter an Anthropic API key"));
    }
    let res = http()
        .get(format!("{base}/v1/models/{model}"))
        .header("x-api-key", key.trim())
        .header("anthropic-version", ANTHROPIC_VERSION)
        .timeout(Duration::from_secs(20))
        .send()
        .await
        .map_err(|e| AppError::new("network", format!("Couldn't reach Anthropic: {e}")))?;
    if !res.status().is_success() {
        let status = res.status();
        let body = res.text().await.unwrap_or_default();
        return Err(anthropic_error(status, &body));
    }
    let v: Value = res.json().await.unwrap_or(Value::Null);
    Ok(v["display_name"].as_str().unwrap_or(model).to_string())
}

// ------------------------------------------------------- other providers

/// Pulls a readable message out of an error body (OpenAI, Gemini, Mistral…).
fn error_message(body: &str) -> String {
    let v: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    let msg = v["error"]["message"]
        .as_str()
        .or_else(|| v["error"].as_str())
        .or_else(|| v["message"].as_str())
        .or_else(|| v["detail"].as_str())
        .map(str::to_string)
        .unwrap_or_else(|| body.chars().take(300).collect());
    msg.lines().next().unwrap_or("").trim().to_string()
}

fn provider_error(name: &str, model: &str, status: reqwest::StatusCode, body: &str) -> AppError {
    let msg = error_message(body);
    let lower = msg.to_lowercase();
    let text = match status.as_u16() {
        400 if lower.contains("api key") || lower.contains("api_key") => format!("{name} didn't accept the API key. Check it in Settings → AI."),
        401 => format!("{name} didn't accept the API key. Check it in Settings → AI."),
        403 => format!("This {name} key isn't allowed to do that. {msg}"),
        404 => format!("{name} doesn't have the model “{model}”. Pick another one in Settings → AI."),
        429 if lower.contains("quota") || lower.contains("insufficient") || lower.contains("billing") || lower.contains("credit") => {
            format!("Your {name} account is out of credit or over its quota. {msg}")
        }
        429 => format!("{name}'s rate limit was reached. Wait a moment and try again."),
        500..=599 => format!("{name} is having trouble right now ({status}). Try again in a moment."),
        _ => format!("{name} returned an error ({status}). {msg}"),
    };
    AppError::new("ai", text.trim().to_string())
}

/// What we ask for when a provider can't enforce a JSON schema itself.
const JSON_INSTRUCTION: &str = "\n\nReply with only a JSON object, no Markdown, in exactly this shape: {\"sql\": \"…\", \"explanation\": \"…\"}";

pub fn openai_body(p: &Provider, model: &str, schema_text: &str, user: &str) -> Value {
    let official = p.id == "openai";
    let system = if official {
        format!("{SYSTEM_PROMPT}\n\n{schema_text}")
    } else {
        format!("{SYSTEM_PROMPT}{JSON_INSTRUCTION}\n\n{schema_text}")
    };
    let mut body = json!({
        "model": model,
        "stream": true,
        "messages": [
            // OpenAI's reasoning models take instructions as "developer" messages.
            { "role": if official { "developer" } else { "system" }, "content": system },
            { "role": "user", "content": user }
        ]
    });
    if official {
        body["response_format"] = json!({
            "type": "json_schema",
            "json_schema": { "name": "rowglow_answer", "schema": answer_schema(), "strict": true }
        });
        body["stream_options"] = json!({ "include_usage": true });
    }
    body
}

/// Chat Completions (OpenAI and the many servers that speak it).
pub async fn run_openai(job: &Job, p: &Provider, base: &str, key: &str, model: &str, schema_text: &str, user: &str) -> AppResult<()> {
    if p.needs_key && key.trim().is_empty() {
        return Err(AppError::invalid(format!("Add your {} API key in Settings → AI first", p.name)).with_code("ai_not_configured"));
    }
    if model.trim().is_empty() {
        return Err(AppError::invalid(format!("Choose a {} model in Settings → AI first", p.name)).with_code("ai_not_configured"));
    }
    if base.is_empty() {
        return Err(AppError::invalid("Enter the server's API address in Settings → AI first").with_code("ai_not_configured"));
    }
    job.update(|s| s.model = Some(model.to_string()));
    let mut req = http().post(format!("{base}/chat/completions")).json(&openai_body(p, model, schema_text, user));
    if !key.trim().is_empty() {
        req = req.bearer_auth(key.trim());
    }
    if p.id == "openrouter" {
        // OpenRouter asks apps to identify themselves.
        req = req.header("X-Title", "Rowglow");
    }
    let mut res = req
        .send()
        .await
        .map_err(|e| AppError::new("network", format!("Couldn't reach {}: {e}", p.name)))?;
    if !res.status().is_success() {
        let status = res.status();
        let body = res.text().await.unwrap_or_default();
        return Err(provider_error(p.name, model, status, &body));
    }
    let mut buf: Vec<u8> = Vec::new();
    let mut finish_reason: Option<String> = None;
    'stream: loop {
        let chunk = res
            .chunk()
            .await
            .map_err(|e| AppError::new("network", format!("The connection to {} dropped: {e}", p.name)))?;
        let Some(chunk) = chunk else { break };
        buf.extend_from_slice(&chunk);
        while let Some(pos) = find_event_end(&buf) {
            let raw: Vec<u8> = buf.drain(..pos.0).collect();
            buf.drain(..pos.1);
            for line in String::from_utf8_lossy(&raw).lines() {
                let Some(data) = line.strip_prefix("data:").map(str::trim) else { continue };
                if data == "[DONE]" {
                    break 'stream;
                }
                let Ok(ev) = serde_json::from_str::<Value>(data) else { continue };
                if ev.get("error").is_some_and(|e| !e.is_null()) {
                    return Err(AppError::new("ai", format!("{} stopped with an error: {}", p.name, error_message(data))));
                }
                if let Some(choice) = ev["choices"].get(0) {
                    if let Some(t) = choice["delta"]["content"].as_str().filter(|t| !t.is_empty()) {
                        job.update(|s| {
                            s.status = "writing".into();
                            s.text.push_str(t);
                        });
                    }
                    if let Some(r) = choice["finish_reason"].as_str() {
                        finish_reason = Some(r.to_string());
                    }
                }
                if let Some(u) = ev.get("usage").filter(|u| !u.is_null()) {
                    let (input, output) = (u["prompt_tokens"].as_u64(), u["completion_tokens"].as_u64());
                    job.update(|s| {
                        s.input_tokens = input;
                        s.output_tokens = output;
                    });
                }
                if let Some(m) = ev["model"].as_str() {
                    let m = m.to_string();
                    job.update(|s| s.model = Some(m));
                }
            }
        }
    }
    job.update(|s| s.stop_reason = finish_reason.clone());
    match finish_reason.as_deref() {
        Some("length") => Err(AppError::new("ai", "The answer was too long and got cut off. Try a narrower request.")),
        Some("content_filter") => Err(AppError::new("ai", format!("{} declined this request. Try rephrasing it.", p.name)).with_code("ai_refused")),
        _ => finish(job),
    }
}

pub fn gemini_body(schema_text: &str, user: &str) -> Value {
    json!({
        "systemInstruction": { "parts": [{ "text": format!("{SYSTEM_PROMPT}\n\n{schema_text}") }] },
        "contents": [{ "role": "user", "parts": [{ "text": user }] }],
        "generationConfig": { "responseMimeType": "application/json", "responseJsonSchema": answer_schema() }
    })
}

fn gemini_model(model: &str) -> &str {
    model.trim().trim_start_matches("models/")
}

/// Applies one GenerateContentResponse (a streamed chunk) to the job.
/// Returns the finish reason, if this chunk has one.
fn gemini_chunk(job: &Job, ev: &Value) -> AppResult<Option<String>> {
    if let Some(r) = ev["promptFeedback"]["blockReason"].as_str() {
        return Err(AppError::new("ai", format!("Gemini declined this request ({r}). Try rephrasing it.")).with_code("ai_refused"));
    }
    if ev.get("error").is_some_and(|e| !e.is_null()) {
        return Err(AppError::new("ai", format!("Gemini stopped with an error: {}", error_message(&ev.to_string()))));
    }
    let cand = &ev["candidates"][0];
    if let Some(parts) = cand["content"]["parts"].as_array() {
        for part in parts {
            if part["thought"].as_bool() == Some(true) {
                continue;
            }
            if let Some(t) = part["text"].as_str().filter(|t| !t.is_empty()) {
                job.update(|s| {
                    s.status = "writing".into();
                    s.text.push_str(t);
                });
            }
        }
    }
    if let Some(u) = ev.get("usageMetadata") {
        let input = u["promptTokenCount"].as_u64();
        let output = match (u["candidatesTokenCount"].as_u64(), u["thoughtsTokenCount"].as_u64()) {
            (Some(a), Some(b)) => Some(a + b),
            (a, b) => a.or(b),
        };
        job.update(|s| {
            if input.is_some() {
                s.input_tokens = input;
            }
            if output.is_some() {
                s.output_tokens = output;
            }
        });
    }
    if let Some(v) = ev["modelVersion"].as_str() {
        let v = v.to_string();
        job.update(|s| s.model = Some(v));
    }
    Ok(cand["finishReason"].as_str().map(str::to_string))
}

/// Gemini generateContent, streamed as server-sent events.
pub async fn run_gemini(job: &Job, base: &str, key: &str, model: &str, schema_text: &str, user: &str) -> AppResult<()> {
    if key.trim().is_empty() {
        return Err(AppError::invalid("Add your Gemini API key in Settings → AI first").with_code("ai_not_configured"));
    }
    let model = gemini_model(model);
    if model.is_empty() {
        return Err(AppError::invalid("Choose a Gemini model in Settings → AI first").with_code("ai_not_configured"));
    }
    job.update(|s| s.model = Some(model.to_string()));
    let mut res = http()
        .post(format!("{base}/models/{model}:streamGenerateContent?alt=sse"))
        .header("x-goog-api-key", key.trim())
        .json(&gemini_body(schema_text, user))
        .send()
        .await
        .map_err(|e| AppError::new("network", format!("Couldn't reach Gemini: {e}")))?;
    if !res.status().is_success() {
        let status = res.status();
        let body = res.text().await.unwrap_or_default();
        return Err(provider_error("Gemini", model, status, &body));
    }
    let mut buf: Vec<u8> = Vec::new();
    let mut finish_reason: Option<String> = None;
    loop {
        let chunk = res
            .chunk()
            .await
            .map_err(|e| AppError::new("network", format!("The connection to Gemini dropped: {e}")))?;
        let Some(chunk) = chunk else { break };
        buf.extend_from_slice(&chunk);
        while let Some(pos) = find_event_end(&buf) {
            let raw: Vec<u8> = buf.drain(..pos.0).collect();
            buf.drain(..pos.1);
            let text = String::from_utf8_lossy(&raw);
            let data: String = text.lines().filter_map(|l| l.strip_prefix("data:")).map(str::trim).collect::<Vec<_>>().join("");
            if data.is_empty() {
                continue;
            }
            if let Ok(ev) = serde_json::from_str::<Value>(&data) {
                if let Some(r) = gemini_chunk(job, &ev)? {
                    finish_reason = Some(r);
                }
            }
        }
    }
    // Without alt=sse support the server sends one JSON array instead.
    let rest = String::from_utf8_lossy(&buf).trim().to_string();
    if rest.starts_with('[') {
        if let Ok(Value::Array(items)) = serde_json::from_str::<Value>(&rest) {
            for ev in &items {
                if let Some(r) = gemini_chunk(job, ev)? {
                    finish_reason = Some(r);
                }
            }
        }
    }
    job.update(|s| s.stop_reason = finish_reason.clone());
    match finish_reason.as_deref() {
        Some("MAX_TOKENS") => Err(AppError::new("ai", "The answer was too long and got cut off. Try a narrower request.")),
        Some("SAFETY" | "PROHIBITED_CONTENT" | "BLOCKLIST" | "SPII" | "RECITATION") => Err(AppError::new(
            "ai",
            format!("Gemini declined this request ({}). Try rephrasing it.", finish_reason.unwrap_or_default()),
        )
        .with_code("ai_refused")),
        _ => finish(job),
    }
}

// ------------------------------------------------------------- model lists

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ModelInfo {
    pub id: String,
    pub name: String,
    pub note: String,
}

/// Models that can't answer a chat request (embeddings, speech, images…).
fn not_a_chat_model(id: &str) -> bool {
    const SKIP: &[&str] = &[
        "embed", "whisper", "tts", "transcribe", "audio", "realtime", "image", "dall-e", "moderation", "search", "rerank",
        "ocr", "guard", "babbage", "davinci", "computer-use", "robotics", "-live", "native-audio", "aqa", "veo", "imagen",
        "sora", "codex", "instruct",
    ];
    let id = id.to_lowercase();
    SKIP.iter().any(|s| id.contains(s))
}

/// Parses GET /models from an OpenAI-compatible server. OpenAI's own list is
/// trimmed to chat models, newest first, with retiring ones labelled.
pub fn parse_openai_models(provider: &str, v: &Value) -> Vec<ModelInfo> {
    let mut rows: Vec<(i64, ModelInfo)> = v["data"]
        .as_array()
        .map(|a| a.as_slice())
        .unwrap_or_default()
        .iter()
        .filter_map(|m| {
            let id = m["id"].as_str()?.to_string();
            if not_a_chat_model(&id) {
                return None;
            }
            if provider == "openai" {
                let chat = id.starts_with("gpt-") || id.starts_with("chat-latest") || (id.starts_with('o') && id[1..].starts_with(|c: char| c.is_ascii_digit()));
                if !chat {
                    return None;
                }
            }
            let note = match m["shutdown_date"].as_str() {
                Some(d) => format!("retiring {d}"),
                None => String::new(),
            };
            let name = m["name"].as_str().unwrap_or(&id).to_string();
            Some((m["created"].as_i64().unwrap_or(0), ModelInfo { id, name, note }))
        })
        .collect();
    if provider == "openai" {
        rows.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.id.cmp(&b.1.id)));
    } else {
        rows.sort_by(|a, b| a.1.id.cmp(&b.1.id));
    }
    rows.into_iter().map(|r| r.1).collect()
}

/// Parses Gemini's models.list: models that can generateContent, newest first.
pub fn parse_gemini_models(v: &Value) -> Vec<ModelInfo> {
    let mut out: Vec<ModelInfo> = v["models"]
        .as_array()
        .map(|a| a.as_slice())
        .unwrap_or_default()
        .iter()
        .filter(|m| {
            m["supportedGenerationMethods"]
                .as_array()
                .is_some_and(|a| a.iter().any(|x| x == "generateContent"))
        })
        .filter_map(|m| {
            let id = m["name"].as_str()?.trim_start_matches("models/").to_string();
            if !id.starts_with("gemini") || not_a_chat_model(&id) {
                return None;
            }
            Some(ModelInfo { name: m["displayName"].as_str().unwrap_or(&id).to_string(), id, note: String::new() })
        })
        .collect();
    out.sort_by(|a, b| b.id.cmp(&a.id));
    out.dedup_by(|a, b| a.id == b.id);
    out
}

/// A sensible default from a provider's list: an everyday model rather than
/// the priciest "pro" tier or a preview.
pub fn recommended(provider: &str, models: &[ModelInfo]) -> Option<String> {
    let pick = |f: &dyn Fn(&str) -> bool| models.iter().find(|m| f(&m.id)).map(|m| m.id.clone());
    let everyday = |id: &str| !id.contains("pro") && !id.contains("preview") && !id.contains("exp") && !id.contains("latest");
    match provider {
        "anthropic" => Some(DEFAULT_MODEL.to_string()),
        "openai" => pick(&|id| everyday(id) && models.iter().any(|m| m.id == id && m.note.is_empty())).or_else(|| pick(&|_| true)),
        "gemini" => pick(&|id| id.contains("flash") && !id.contains("lite") && everyday(id)).or_else(|| pick(&everyday)).or_else(|| pick(&|_| true)),
        "ollama" => pick(&|id| id.contains("coder")).or_else(|| pick(&|_| true)),
        _ => pick(&|_| true),
    }
}

/// The provider's models, fetched live with the user's key (free calls).
pub async fn list_models(settings: &AiSettings, provider_id: &str, key: &str) -> AppResult<Vec<ModelInfo>> {
    let p = provider(provider_id)?;
    let base = settings.base_for(p.id);
    match p.wire {
        Wire::Anthropic => Ok(CLAUDE_MODELS
            .iter()
            .map(|(id, name, note)| ModelInfo { id: id.to_string(), name: name.to_string(), note: note.to_string() })
            .collect()),
        Wire::Ollama => Ok(ollama_models(&base)
            .await?
            .into_iter()
            .map(|id| ModelInfo { name: id.clone(), id, note: String::new() })
            .collect()),
        Wire::Gemini => {
            if key.trim().is_empty() {
                return Err(AppError::invalid("Enter your Gemini API key to see its models"));
            }
            let mut all = Vec::new();
            let mut token = String::new();
            for _ in 0..5 {
                let mut req = http()
                    .get(format!("{base}/models"))
                    .query(&[("pageSize", "1000")])
                    .header("x-goog-api-key", key.trim())
                    .timeout(Duration::from_secs(20));
                if !token.is_empty() {
                    req = req.query(&[("pageToken", token.as_str())]);
                }
                let res = req.send().await.map_err(|e| AppError::new("network", format!("Couldn't reach Gemini: {e}")))?;
                if !res.status().is_success() {
                    let status = res.status();
                    let body = res.text().await.unwrap_or_default();
                    return Err(provider_error("Gemini", "", status, &body));
                }
                let v: Value = res.json().await.map_err(|e| AppError::new("network", e.to_string()))?;
                all.extend(parse_gemini_models(&v));
                match v["nextPageToken"].as_str().filter(|t| !t.is_empty()) {
                    Some(t) => token = t.to_string(),
                    None => break,
                }
            }
            all.sort_by(|a, b| b.id.cmp(&a.id));
            all.dedup_by(|a, b| a.id == b.id);
            Ok(all)
        }
        Wire::Openai => {
            if base.is_empty() {
                return Err(AppError::invalid("Enter the server's API address first"));
            }
            if p.needs_key && key.trim().is_empty() {
                return Err(AppError::invalid(format!("Enter your {} API key to see its models", p.name)));
            }
            let mut req = http().get(format!("{base}/models")).timeout(Duration::from_secs(20));
            if !key.trim().is_empty() {
                req = req.bearer_auth(key.trim());
            }
            let res = req.send().await.map_err(|e| AppError::new("network", format!("Couldn't reach {}: {e}", p.name)))?;
            if !res.status().is_success() {
                let status = res.status();
                let body = res.text().await.unwrap_or_default();
                return Err(provider_error(p.name, "", status, &body));
            }
            let v: Value = res.json().await.map_err(|e| AppError::new("network", e.to_string()))?;
            Ok(parse_openai_models(p.id, &v))
        }
    }
}

/// Checks the key, the connection and the chosen model without generating
/// anything (listing models is free everywhere).
pub async fn test_provider(settings: &AiSettings, key: &str) -> AppResult<String> {
    let p = provider(&settings.provider)?;
    let model = settings.model_for(p.id);
    if p.wire == Wire::Anthropic {
        return test_anthropic(&settings.base_for(p.id), key, &model).await;
    }
    if p.wire == Wire::Ollama {
        return test_ollama(settings).await;
    }
    let models = list_models(settings, p.id, key).await?;
    if model.trim().is_empty() {
        return Err(AppError::invalid("Choose a model"));
    }
    let wanted = gemini_model(&model);
    if !models.is_empty() && !models.iter().any(|m| m.id == wanted) {
        return Err(AppError::invalid(format!("{} doesn't list the model “{wanted}” for this key", p.name)));
    }
    Ok(format!("{} · {wanted}", p.name))
}

// ------------------------------------------------------------------- Ollama

fn ollama_url(s: &AiSettings) -> String {
    let u = s.base_for("ollama");
    if u.is_empty() { OLLAMA_DEFAULT.to_string() } else { u }
}

pub async fn ollama_models(url: &str) -> AppResult<Vec<String>> {
    let base = if url.trim().is_empty() { OLLAMA_DEFAULT } else { url.trim() }.trim_end_matches('/');
    let res = http()
        .get(format!("{base}/api/tags"))
        .timeout(Duration::from_secs(5))
        .send()
        .await
        .map_err(|_| AppError::new("network", format!("Ollama isn't running at {base}. Start the Ollama app (or ollama serve) and try again.")))?;
    if !res.status().is_success() {
        return Err(AppError::new("network", format!("Ollama answered {}", res.status())));
    }
    let v: Value = res.json().await.map_err(|e| AppError::new("network", e.to_string()))?;
    let mut names: Vec<String> = v["models"]
        .as_array()
        .map(|a| a.iter().filter_map(|m| m["name"].as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    names.sort();
    Ok(names)
}

pub async fn run_ollama(job: &Job, settings: &AiSettings, schema_text: &str, user: &str) -> AppResult<()> {
    let model = settings.ollama_model.trim();
    if model.is_empty() {
        return Err(AppError::invalid("Choose an Ollama model in Settings → AI first").with_code("ai_not_configured"));
    }
    let base = ollama_url(settings);
    job.update(|s| s.model = Some(model.to_string()));
    let body = json!({
        "model": model,
        "stream": true,
        "format": answer_schema(),
        "options": { "temperature": 0 },
        "messages": [
            { "role": "system", "content": format!("{SYSTEM_PROMPT}\n\n{schema_text}") },
            { "role": "user", "content": user }
        ]
    });
    let mut res = http()
        .post(format!("{base}/api/chat"))
        .json(&body)
        .send()
        .await
        .map_err(|_| AppError::new("network", format!("Ollama isn't running at {base}. Start it and try again.")))?;
    if !res.status().is_success() {
        let status = res.status();
        let text = res.text().await.unwrap_or_default();
        let msg = serde_json::from_str::<Value>(&text)
            .ok()
            .and_then(|v| v["error"].as_str().map(str::to_string))
            .unwrap_or(text);
        return Err(AppError::new("ai", format!("Ollama returned an error ({status}): {msg}")));
    }
    let mut buf: Vec<u8> = Vec::new();
    loop {
        let chunk = res
            .chunk()
            .await
            .map_err(|e| AppError::new("network", format!("The connection to Ollama dropped: {e}")))?;
        let Some(chunk) = chunk else { break };
        buf.extend_from_slice(&chunk);
        while let Some(nl) = buf.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = buf.drain(..=nl).collect();
            let Ok(v) = serde_json::from_slice::<Value>(&line) else { continue };
            if let Some(e) = v["error"].as_str() {
                return Err(AppError::new("ai", format!("Ollama: {e}")));
            }
            if let Some(t) = v["message"]["content"].as_str().filter(|t| !t.is_empty()) {
                job.update(|s| {
                    s.status = "writing".into();
                    s.text.push_str(t);
                });
            }
            if v["done"].as_bool() == Some(true) {
                let input = v["prompt_eval_count"].as_u64();
                let output = v["eval_count"].as_u64();
                let reason = v["done_reason"].as_str().map(str::to_string);
                job.update(|s| {
                    s.input_tokens = input;
                    s.output_tokens = output;
                    s.stop_reason = reason;
                });
            }
        }
    }
    finish(job)
}

pub async fn test_ollama(settings: &AiSettings) -> AppResult<String> {
    let models = ollama_models(&settings.ollama_url).await?;
    let want = settings.ollama_model.trim();
    if want.is_empty() {
        return Err(AppError::invalid("Choose a model"));
    }
    if !models.iter().any(|m| m == want || m.trim_end_matches(":latest") == want) {
        return Err(AppError::invalid(format!("Ollama doesn't have {want}. Run: ollama pull {want}")));
    }
    Ok(want.to_string())
}

/// A job that is already finished, for tests.
#[cfg(test)]
fn test_job() -> Job {
    Job {
        state: Mutex::new(JobState { status: "thinking".into(), ..Default::default() }),
        abort: Mutex::new(None),
        started: Instant::now(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    #[test]
    fn partial_json_fields() {
        let t = r#"{"sql": "SELECT *\nFROM \"users\" WHERE name = 'aé"#;
        assert_eq!(partial_field(t, "sql").unwrap(), "SELECT *\nFROM \"users\" WHERE name = 'aé");
        assert_eq!(partial_field(t, "explanation"), None);
        let done = r#"{"sql":"SELECT 1","explanation":"One."}"#;
        assert_eq!(partial_field(done, "explanation").unwrap(), "One.");
        assert_eq!(parse_answer(done), Some(("SELECT 1".into(), "One.".into())));
        assert_eq!(
            parse_answer("Here you go:\n```json\n{\"sql\":\"```sql\\nSELECT 2\\n```\",\"explanation\":\"x\"}\n```"),
            Some(("SELECT 2".into(), "x".into()))
        );
        assert_eq!(parse_answer("not json"), None);
    }

    fn table(name: &str, cols: &[(&str, &str)]) -> CompletionTable {
        CompletionTable {
            schema: "public".into(),
            name: name.into(),
            kind: "table".into(),
            columns: cols.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect(),
        }
    }

    #[test]
    fn describes_schema_without_data() {
        let tables = vec![
            table("customers", &[("id", "integer"), ("email", "text")]),
            table("orders", &[("id", "bigint"), ("customer_id", "integer"), ("total", "numeric")]),
        ];
        let rel = SchemaRelations {
            primary_keys: vec![
                TablePrimaryKey { table: "customers".into(), columns: vec!["id".into()] },
                TablePrimaryKey { table: "orders".into(), columns: vec!["id".into()] },
            ],
            foreign_keys: vec![ForeignKeyRelation {
                name: "orders_customer_fk".into(),
                schema: "public".into(),
                table: "orders".into(),
                columns: vec!["customer_id".into()],
                ref_schema: "public".into(),
                ref_table: "customers".into(),
                ref_columns: vec!["id".into()],
            }],
        };
        let (text, n) = describe_schema("PostgreSQL", "shop", "public", &tables, &rel, "");
        assert_eq!(n, 2);
        assert!(text.starts_with("Database: PostgreSQL. Current database: shop. Current schema: public."));
        assert!(text.contains("- customers (table): id integer PK, email text"));
        assert!(text.contains("customer_id integer -> customers.id"));

        // A huge schema keeps the tables the request names.
        let many: Vec<CompletionTable> = (0..4000)
            .map(|i| table(&format!("table_{i:04}"), &[("a_long_column_name_for_padding", "character varying(255)"); 4]))
            .chain(tables.clone())
            .collect();
        let (text, n) = describe_schema("PostgreSQL", "shop", "public", &many, &rel, "total spent per customer from orders");
        assert_eq!(n, 4002);
        assert!(text.len() <= MAX_SCHEMA_CHARS);
        assert!(text.contains("- orders (table):"));
        assert!(text.contains("- customers (table):"), "linked table kept in full");
        assert!(text.contains("Other tables (columns not listed): table_0000"));
    }

    #[test]
    fn request_body() {
        let b = anthropic_body("claude-opus-5-5", "Database: SQLite.", "Write SQL");
        assert_eq!(b["model"], "claude-opus-5-5");
        assert_eq!(DEFAULT_MODEL, "claude-haiku-5-5");
        assert_eq!(CLAUDE_MODELS.len(), 3);
        assert_eq!(CLAUDE_MODELS[0].0, DEFAULT_MODEL, "cheapest first, and the default");
        assert_eq!(b["thinking"]["type"], "adaptive");
        assert_eq!(b["output_config"]["effort"], "medium");
        assert_eq!(b["output_config"]["format"]["type"], "json_schema");
        assert_eq!(b["fallbacks"], "default");
        assert_eq!(b["system"][1]["cache_control"]["type"], "ephemeral");
        assert!(anthropic_body("claude-haiku-5-5", "", "").get("fallbacks").is_none());
    }

    /// Serves one HTTP response on a random port; returns the base URL and
    /// the request it received.
    async fn mock(response: String) -> (String, tokio::task::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut req = Vec::new();
            let mut buf = [0u8; 8192];
            // Read headers, then the body by Content-Length.
            loop {
                let n = sock.read(&mut buf).await.unwrap();
                req.extend_from_slice(&buf[..n]);
                if let Some(h) = req.windows(4).position(|w| w == b"\r\n\r\n") {
                    let head = String::from_utf8_lossy(&req[..h]).to_lowercase();
                    let len = head
                        .lines()
                        .find_map(|l| l.strip_prefix("content-length:"))
                        .and_then(|v| v.trim().parse::<usize>().ok())
                        .unwrap_or(0);
                    while req.len() < h + 4 + len {
                        let n = sock.read(&mut buf).await.unwrap();
                        req.extend_from_slice(&buf[..n]);
                    }
                    break;
                }
            }
            // Dribble the response in small pieces, like a real stream.
            for piece in response.as_bytes().chunks(37) {
                sock.write_all(piece).await.unwrap();
                sock.flush().await.unwrap();
            }
            sock.shutdown().await.ok();
            String::from_utf8_lossy(&req).into_owned()
        });
        (format!("http://127.0.0.1:{port}"), handle)
    }

    fn sse(events: &[Value]) -> String {
        let mut body = String::new();
        for e in events {
            body.push_str(&format!("event: {}\ndata: {}\n\n", e["type"].as_str().unwrap(), e));
        }
        format!("HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n{body}")
    }

    #[tokio::test]
    async fn streams_an_answer_from_claude() {
        crate::init_crypto();
        let answer = r#"{"sql":"SELECT c.email, SUM(o.total) AS spent\nFROM customers c JOIN orders o ON o.customer_id = c.id\nGROUP BY c.email\nORDER BY spent DESC\nLIMIT 100","explanation":"Total spent per customer."}"#;
        let mut events = vec![
            json!({"type":"message_start","message":{"id":"msg_1","model":"claude-haiku-5-5","usage":{"input_tokens":812,"output_tokens":1}}}),
            json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}),
            json!({"type":"content_block_stop","index":0}),
            json!({"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}),
        ];
        for piece in answer.as_bytes().chunks(9) {
            events.push(json!({"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text": String::from_utf8_lossy(piece)}}));
        }
        events.push(json!({"type":"content_block_stop","index":1}));
        events.push(json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":95}}));
        events.push(json!({"type":"message_stop"}));
        let (base, server) = mock(sse(&events)).await;
        let job = test_job();
        run_anthropic(&job, &base, "sk-ant-test", DEFAULT_MODEL, "Database: PostgreSQL.", "total spent per customer")
            .await
            .unwrap();
        let s = job.state.lock().unwrap().clone();
        assert_eq!(s.status, "done");
        assert!(s.sql.as_deref().unwrap().starts_with("SELECT c.email"));
        assert_eq!(s.explanation.as_deref(), Some("Total spent per customer."));
        assert_eq!(s.model.as_deref(), Some("claude-haiku-5-5"));
        assert_eq!(s.output_tokens, Some(95));
        let req = server.await.unwrap();
        let lower = req.to_lowercase();
        assert!(lower.starts_with("post /v1/messages "));
        assert!(lower.contains("x-api-key: sk-ant-test"));
        assert!(lower.contains("anthropic-version: 2026-10-21"));
        // Haiku has no server-side refusal fallback, so no beta header is sent.
        assert!(!lower.contains("anthropic-beta"));
        let body: Value = serde_json::from_str(&req[req.find("\r\n\r\n").unwrap() + 4..]).unwrap();
        assert_eq!(body["stream"], true);
        assert_eq!(body["model"], "claude-haiku-5-5");
        assert!(body.get("fallbacks").is_none());
        assert_eq!(body["messages"][0]["content"], "total spent per customer");
    }

    #[tokio::test]
    async fn reports_refusals_and_bad_keys() {
        crate::init_crypto();
        let events = vec![
            json!({"type":"message_start","message":{"id":"msg_2","model":"claude-opus-5-5","usage":{"input_tokens":10}}}),
            json!({"type":"message_delta","delta":{"stop_reason":"refusal","stop_details":{"type":"refusal","category":"cyber"}},"usage":{"output_tokens":0}}),
            json!({"type":"message_stop"}),
        ];
        let (base, _s) = mock(sse(&events)).await;
        let e = run_anthropic(&test_job(), &base, "k", "claude-opus-5-5", "", "x").await.unwrap_err();
        assert_eq!(e.code.as_deref(), Some("ai_refused"));
        assert!(e.message.contains("declined"));

        let body = r#"{"type":"error","error":{"type":"authentication_error","message":"invalid x-api-key"}}"#;
        let (base, _s) = mock(format!(
            "HTTP/1.1 401 Unauthorized\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        ))
        .await;
        let e = run_anthropic(&test_job(), &base, "bad", "claude-opus-5-5", "", "x").await.unwrap_err();
        assert!(e.message.contains("didn't accept the API key"), "{}", e.message);

        let e = run_anthropic(&test_job(), &base, "  ", "claude-opus-5-5", "", "x").await.unwrap_err();
        assert_eq!(e.code.as_deref(), Some("ai_not_configured"));
    }

    #[tokio::test]
    async fn streams_an_answer_from_openai() {
        crate::init_crypto();
        let answer = r#"{"sql":"SELECT 1 AS one","explanation":"Returns one."}"#;
        let mut body = String::new();
        let chunk = |delta: Value, finish: Value| {
            json!({"id":"chatcmpl-1","object":"chat.completion.chunk","model":"gpt-6-luna","choices":[{"index":0,"delta":delta,"finish_reason":finish}],"usage":null})
        };
        body.push_str(&format!("data: {}\n\n", chunk(json!({"role":"assistant","content":""}), Value::Null)));
        for piece in answer.as_bytes().chunks(7) {
            body.push_str(&format!("data: {}\n\n", chunk(json!({"content": String::from_utf8_lossy(piece)}), Value::Null)));
        }
        body.push_str(&format!("data: {}\n\n", chunk(json!({}), json!("stop"))));
        body.push_str(&format!("data: {}\n\n", json!({"id":"chatcmpl-1","object":"chat.completion.chunk","choices":[],"usage":{"prompt_tokens":700,"completion_tokens":40,"total_tokens":740}})));
        body.push_str("data: [DONE]\n\n");
        let (base, server) = mock(format!("HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n{body}")).await;
        let job = test_job();
        let p = provider("openai").unwrap();
        run_openai(&job, p, &base, "sk-proj-test", "gpt-6-luna", "Database: MySQL.", "one").await.unwrap();
        let st = job.state.lock().unwrap().clone();
        assert_eq!(st.status, "done");
        assert_eq!(st.sql.as_deref(), Some("SELECT 1 AS one"));
        assert_eq!((st.input_tokens, st.output_tokens), (Some(700), Some(40)));
        let req = server.await.unwrap();
        assert!(req.starts_with("POST /chat/completions "), "{req}");
        assert!(req.to_lowercase().contains("authorization: bearer sk-proj-test"));
        let sent: Value = serde_json::from_str(&req[req.find("\r\n\r\n").unwrap() + 4..]).unwrap();
        assert_eq!(sent["messages"][0]["role"], "developer");
        assert_eq!(sent["response_format"]["type"], "json_schema");
        assert_eq!(sent["response_format"]["json_schema"]["strict"], true);
        assert_eq!(sent["stream_options"]["include_usage"], true);
        assert!(sent.get("temperature").is_none() && sent.get("max_tokens").is_none());

        // Other OpenAI-compatible servers: plain system prompt asking for JSON;
        // a fenced answer still parses.
        let fenced = "```json\n{\"sql\": \"SELECT 2\", \"explanation\": \"Two.\"}\n```";
        let body = format!(
            "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
            json!({"choices":[{"index":0,"delta":{"content":fenced},"finish_reason":null}]}),
            json!({"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]})
        );
        let (base, server) = mock(format!("HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n{body}")).await;
        let job = test_job();
        run_openai(&job, provider("mistral").unwrap(), &base, "k", "mistral-large-latest", "", "two").await.unwrap();
        assert_eq!(job.state.lock().unwrap().sql.as_deref(), Some("SELECT 2"));
        let req = server.await.unwrap();
        let sent: Value = serde_json::from_str(&req[req.find("\r\n\r\n").unwrap() + 4..]).unwrap();
        assert_eq!(sent["messages"][0]["role"], "system");
        assert!(sent["messages"][0]["content"].as_str().unwrap().contains("Reply with only a JSON object"));
        assert!(sent.get("response_format").is_none());

        // Errors are readable.
        let err = r#"{"error":{"message":"You exceeded your current quota, please check your plan and billing details.","type":"insufficient_quota","code":"insufficient_quota"}}"#;
        let (base, _s) = mock(format!("HTTP/1.1 429 Too Many Requests\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{err}", err.len())).await;
        let e = run_openai(&test_job(), p, &base, "k", "gpt-6-luna", "", "x").await.unwrap_err();
        assert!(e.message.contains("out of credit"), "{}", e.message);
        let e = run_openai(&test_job(), p, &base, "", "gpt-6-luna", "", "x").await.unwrap_err();
        assert_eq!(e.code.as_deref(), Some("ai_not_configured"));
    }

    #[tokio::test]
    async fn streams_an_answer_from_gemini() {
        crate::init_crypto();
        let answer = r#"{"sql": "SELECT name FROM artists LIMIT 100", "explanation": "Artist names."}"#;
        let (a, b) = answer.split_at(30);
        let ev = |parts: Value, finish: Value, usage: Value| json!({"candidates":[{"content":{"role":"model","parts":parts},"finishReason":finish,"index":0}],"usageMetadata":usage,"modelVersion":"gemini-3.8-flash"});
        let body = format!(
            "data: {}\r\n\r\ndata: {}\r\n\r\ndata: {}\r\n\r\n",
            ev(json!([{"text":"thinking about it","thought":true}]), Value::Null, json!({"promptTokenCount":512})),
            ev(json!([{"text":a}]), Value::Null, json!({"promptTokenCount":512})),
            ev(json!([{"text":b}]), json!("STOP"), json!({"promptTokenCount":512,"candidatesTokenCount":30,"thoughtsTokenCount":20}))
        );
        let (base, server) = mock(format!("HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n{body}")).await;
        let job = test_job();
        run_gemini(&job, &base, "AIza-test", "models/gemini-3.8-flash", "Database: SQLite.", "artists").await.unwrap();
        let st = job.state.lock().unwrap().clone();
        assert_eq!(st.status, "done");
        assert_eq!(st.sql.as_deref(), Some("SELECT name FROM artists LIMIT 100"));
        assert_eq!((st.input_tokens, st.output_tokens), (Some(512), Some(50)));
        let req = server.await.unwrap();
        assert!(req.starts_with("POST /models/gemini-3.8-flash:streamGenerateContent?alt=sse "), "{req}");
        assert!(req.to_lowercase().contains("x-goog-api-key: aiza-test"));
        let sent: Value = serde_json::from_str(&req[req.find("\r\n\r\n").unwrap() + 4..]).unwrap();
        assert_eq!(sent["generationConfig"]["responseMimeType"], "application/json");
        assert_eq!(sent["generationConfig"]["responseJsonSchema"]["required"][0], "sql");
        assert!(sent["systemInstruction"]["parts"][0]["text"].as_str().unwrap().contains("Database: SQLite."));

        // Blocked prompt, and a bad key (Gemini answers 400 for those).
        let blocked = format!("data: {}\n\n", json!({"promptFeedback":{"blockReason":"SAFETY"}}));
        let (base, _s) = mock(format!("HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n{blocked}")).await;
        let e = run_gemini(&test_job(), &base, "k", "gemini-3.8-flash", "", "x").await.unwrap_err();
        assert_eq!(e.code.as_deref(), Some("ai_refused"));
        let bad = r#"{"error":{"code":400,"message":"API key not valid. Please pass a valid API key.","status":"INVALID_ARGUMENT"}}"#;
        let (base, _s) = mock(format!("HTTP/1.1 400 Bad Request\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{bad}", bad.len())).await;
        let e = run_gemini(&test_job(), &base, "bad", "gemini-3.8-flash", "", "x").await.unwrap_err();
        assert!(e.message.contains("didn't accept the API key"), "{}", e.message);
    }

    #[test]
    fn model_lists() {
        let openai = json!({"object":"list","data":[
            {"id":"gpt-5","created":1754000000,"object":"model","owned_by":"system","shutdown_date":"2026-12-11"},
            {"id":"gpt-6.2-astra","created":1790000000,"object":"model","owned_by":"system","shutdown_date":null},
            {"id":"gpt-6.2-astra-pro","created":1790000001,"object":"model","owned_by":"system"},
            {"id":"gpt-6-luna","created":1780000000,"object":"model","owned_by":"system"},
            {"id":"text-embedding-3-large","created":1700000000,"object":"model","owned_by":"system"},
            {"id":"gpt-realtime","created":1790000002,"object":"model","owned_by":"system"},
            {"id":"whisper-1","created":1600000000,"object":"model","owned_by":"system"},
            {"id":"dall-e-3","created":1600000000,"object":"model","owned_by":"system"}
        ]});
        let m = parse_openai_models("openai", &openai);
        let ids: Vec<&str> = m.iter().map(|x| x.id.as_str()).collect();
        assert_eq!(ids, ["gpt-6.2-astra-pro", "gpt-6.2-astra", "gpt-6-luna", "gpt-5"]);
        assert_eq!(m[3].note, "retiring 2026-12-11");
        assert_eq!(recommended("openai", &m).as_deref(), Some("gpt-6.2-astra"));

        let gemini = json!({"models":[
            {"name":"models/gemini-3.5-flash-lite","displayName":"Gemini 3.5 Flash-Lite","supportedGenerationMethods":["generateContent","countTokens"]},
            {"name":"models/gemini-3.8-flash","displayName":"Gemini 3.8 Flash","supportedGenerationMethods":["generateContent"]},
            {"name":"models/gemini-4-carbon","displayName":"Gemini 4 Carbon","supportedGenerationMethods":["generateContent"]},
            {"name":"models/gemini-embedding-001","supportedGenerationMethods":["embedContent"]},
            {"name":"models/gemini-3.8-flash-tts","supportedGenerationMethods":["generateContent"]},
            {"name":"models/imagen-4.0","supportedGenerationMethods":["predict"]}
        ]});
        let g = parse_gemini_models(&gemini);
        let ids: Vec<&str> = g.iter().map(|x| x.id.as_str()).collect();
        assert_eq!(ids, ["gemini-4-carbon", "gemini-3.8-flash", "gemini-3.5-flash-lite"]);
        assert_eq!(g[1].name, "Gemini 3.8 Flash");
        assert_eq!(recommended("gemini", &g).as_deref(), Some("gemini-3.8-flash"));

        let mistral = json!({"data":[{"id":"mistral-large-latest"},{"id":"mistral-embed"},{"id":"codestral-latest"}]});
        let ids: Vec<String> = parse_openai_models("mistral", &mistral).into_iter().map(|x| x.id).collect();
        assert_eq!(ids, ["codestral-latest", "mistral-large-latest"]);

        let s = AiSettings::default();
        assert_eq!(s.model_for("anthropic"), DEFAULT_MODEL);
        assert_eq!(s.base_for("openai"), "https://api.openai.com/v1");
        assert_eq!(key_secret("anthropic"), KEY_SECRET);
        assert_eq!(key_secret("gemini"), "ai:gemini");
        // Older settings (only Claude/Ollama fields) still load.
        let old: AiSettings = serde_json::from_str(r#"{"provider":"ollama","model":"claude-opus-5-5","ollamaUrl":"http://127.0.0.1:11434","ollamaModel":"qwen2.5-coder"}"#).unwrap();
        assert_eq!(old.model_for("ollama"), "qwen2.5-coder");
        assert!(old.models.is_empty());
    }

    #[tokio::test]
    async fn streams_an_answer_from_ollama() {
        crate::init_crypto();
        let answer = r#"{"sql":"SELECT COUNT(*) FROM orders","explanation":"Counts orders."}"#;
        let mut body = String::new();
        for piece in answer.as_bytes().chunks(6) {
            body.push_str(&json!({"model":"qwen2.5-coder","message":{"role":"assistant","content": String::from_utf8_lossy(piece)},"done":false}).to_string());
            body.push('\n');
        }
        body.push_str(&json!({"model":"qwen2.5-coder","message":{"role":"assistant","content":""},"done":true,"done_reason":"stop","prompt_eval_count":300,"eval_count":20}).to_string());
        body.push('\n');
        let (base, server) = mock(format!("HTTP/1.1 200 OK\r\ncontent-type: application/x-ndjson\r\nconnection: close\r\n\r\n{body}")).await;
        let job = test_job();
        let settings = AiSettings { provider: "ollama".into(), ollama_url: base, ollama_model: "qwen2.5-coder".into(), ..Default::default() };
        run_ollama(&job, &settings, "Database: SQLite.", "how many orders").await.unwrap();
        let s = job.state.lock().unwrap().clone();
        assert_eq!(s.status, "done");
        assert_eq!(s.sql.as_deref(), Some("SELECT COUNT(*) FROM orders"));
        assert_eq!(s.output_tokens, Some(20));
        let req = server.await.unwrap();
        assert!(req.starts_with("POST /api/chat "));
        let sent: Value = serde_json::from_str(&req[req.find("\r\n\r\n").unwrap() + 4..]).unwrap();
        assert_eq!(sent["format"]["required"][0], "sql");
        assert!(sent["messages"][0]["content"].as_str().unwrap().contains("Database: SQLite."));
    }
}
