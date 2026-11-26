//! MongoDB browser: databases, collections, find / aggregate, document
//! editing by _id, indexes and raw commands.
//!
//! Documents travel as relaxed Extended JSON, so ObjectIds, dates, decimals
//! and binary round-trip exactly.
//!
//! Safety:
//! * Read-only connections can't insert/replace/delete, can't run $out/$merge
//!   pipelines, and only run commands on a read allow-list.
//! * Replacing or deleting a document targets its _id and must match exactly
//!   the expected number of documents, else nothing is reported as saved.
//! * Dropping databases / collections and other destructive commands need an
//!   explicit confirmation from the UI.

use crate::error::{AppError, AppResult};
use crate::model::*;
use mongodb::bson::{doc, Bson, Document};
use futures_util::TryStreamExt;
use mongodb::error::ErrorKind;
use mongodb::options::{ClientOptions, Credential, ServerAddress, Tls, TlsOptions};
use mongodb::Client;
use serde_json::{json, Value as Json};
use std::time::{Duration, Instant};

#[derive(Clone)]
pub struct MongoSession {
    client: Client,
    pub default_db: String,
    read_only: bool,
}

const MAX_DOCS: i64 = 1000;
const QUERY_TIME: Duration = Duration::from_secs(60);

pub fn mongo_error(e: mongodb::error::Error) -> AppError {
    match e.kind.as_ref() {
        ErrorKind::Authentication { message, .. } => AppError::new("connection", format!("Authentication failed: {message}")),
        ErrorKind::ServerSelection { message, .. } => AppError::new(
            "connection",
            format!("Couldn't reach the MongoDB server: {}", message.lines().next().unwrap_or(message)),
        ),
        ErrorKind::Io(_) | ErrorKind::ConnectionPoolCleared { .. } | ErrorKind::DnsResolve { .. } => {
            AppError::new("connection", e.to_string())
        }
        ErrorKind::Command(c) => {
            let kind = if c.code == 50 { "cancelled" } else { "db" };
            AppError::new(kind, c.message.clone()).with_code(format!("{} {}", c.code, c.code_name))
        }
        _ => AppError::new("db", e.to_string()),
    }
}

/// Parses Extended JSON text (`{"_id": {"$oid": "…"}}`) into BSON.
pub fn parse_ejson(text: &str) -> AppResult<Bson> {
    let t = text.trim();
    if t.is_empty() {
        return Ok(Bson::Document(Document::new()));
    }
    let v: Json = serde_json::from_str(t).map_err(|e| AppError::invalid(format!("Invalid JSON: {e}")))?;
    Bson::try_from(v).map_err(|e| AppError::invalid(format!("Invalid Extended JSON: {e}")))
}

pub fn parse_doc(text: &str, what: &str) -> AppResult<Document> {
    match parse_ejson(text)? {
        Bson::Document(d) => Ok(d),
        _ => Err(AppError::invalid(format!("The {what} must be a JSON object, like {{ \"field\": 1 }}"))),
    }
}

fn parse_pipeline(text: &str) -> AppResult<Vec<Document>> {
    match parse_ejson(text)? {
        Bson::Array(a) => a
            .into_iter()
            .map(|s| match s {
                Bson::Document(d) => Ok(d),
                _ => Err(AppError::invalid("Every pipeline stage must be an object")),
            })
            .collect(),
        Bson::Document(d) if d.is_empty() => Ok(Vec::new()),
        Bson::Document(d) => Ok(vec![d]),
        _ => Err(AppError::invalid("The pipeline must be a JSON array of stages")),
    }
}

fn to_json(b: Bson) -> Json {
    b.into_relaxed_extjson()
}

fn writes_in_pipeline(p: &[Document]) -> bool {
    p.iter().any(|s| s.contains_key("$out") || s.contains_key("$merge"))
}

/// Commands allowed on read-only connections (by their first key).
const READ_COMMANDS: &[&str] = &[
    "find", "aggregate", "count", "distinct", "listCollections", "listIndexes", "listDatabases", "dbStats", "collStats",
    "serverStatus", "buildInfo", "buildinfo", "ping", "hello", "isMaster", "ismaster", "hostInfo", "connectionStatus", "explain",
    "getParameter", "getCmdLineOpts", "currentOp", "top", "validate", "dataSize", "getLog", "replSetGetStatus", "usersInfo",
    "rolesInfo", "listCommands", "features", "whatsmyuri", "getMore", "killCursors",
];
const DANGEROUS_COMMANDS: &[&str] = &[
    "dropDatabase", "drop", "dropIndexes", "deleteIndexes", "shutdown", "renameCollection", "dropUser", "dropAllUsersFromDatabase",
    "dropRole", "dropAllRolesFromDatabase", "compact", "repairDatabase", "replSetReconfig", "replSetStepDown", "fsync",
    "setFeatureCompatibilityVersion", "killOp", "killAllSessions", "delete", "emptycapped",
];

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let hex = |c: u8| (c as char).to_digit(16);
            if let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2])) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Splits `user:password@` out of a mongodb:// or mongodb+srv:// address:
/// returns (address without credentials, user, password). None when the
/// address has no password in it. Works for multi-host addresses too.
pub fn split_uri_credentials(host: &str) -> Option<(String, String, String)> {
    let host = host.trim();
    let (scheme, rest) = host.split_once("://")?;
    let auth_end = rest.find(['/', '?']).unwrap_or(rest.len());
    let authority = &rest[..auth_end];
    let at = authority.rfind('@')?;
    let (user, pass) = authority[..at].split_once(':')?;
    let stripped = format!("{scheme}://{}{}", &authority[at + 1..], &rest[auth_end..]);
    Some((stripped, percent_decode(user), percent_decode(pass)))
}

impl MongoSession {
    pub async fn open(cfg: &ConnectionConfig, password: Option<&str>, database: &str) -> AppResult<(MongoSession, ServerInfo)> {
        let host = cfg.host.trim();
        let mut opts = if host.contains("://") {
            // A full mongodb:// or mongodb+srv:// address (e.g. Atlas).
            ClientOptions::parse(host).await.map_err(mongo_error)?
        } else {
            let mut o = ClientOptions::default();
            o.hosts = vec![ServerAddress::Tcp {
                host: if host.is_empty() { "127.0.0.1".into() } else { host.to_string() },
                port: Some(if cfg.port == 0 { 27017 } else { cfg.port }),
            }];
            // One server, no replica-set discovery: also what an SSH tunnel needs.
            o.direct_connection = Some(true);
            o
        };
        if !cfg.username.trim().is_empty() {
            let source = opts
                .credential
                .as_ref()
                .and_then(|c| c.source.clone())
                .unwrap_or_else(|| "admin".into());
            opts.credential = Some(
                Credential::builder()
                    .username(cfg.username.trim().to_string())
                    .password(password.map(str::to_string))
                    .source(source)
                    .build(),
            );
        }
        match cfg.ssl_mode {
            SslMode::Require => {
                let insecure = !host.starts_with("mongodb+srv://");
                opts.tls = Some(Tls::Enabled(TlsOptions::builder().allow_invalid_certificates(insecure).build()));
            }
            SslMode::Disable => opts.tls = Some(Tls::Disabled),
            SslMode::Prefer => {}
        }
        opts.app_name = Some("Rowglow".into());
        opts.connect_timeout = Some(Duration::from_secs(10));
        opts.server_selection_timeout = Some(Duration::from_secs(12));
        let client = Client::with_options(opts).map_err(mongo_error)?;
        let default_db = if database.trim().is_empty() {
            client.default_database().map(|d| d.name().to_string()).unwrap_or_else(|| "test".into())
        } else {
            database.trim().to_string()
        };
        let build = client.database("admin").run_command(doc! { "buildInfo": 1 }).await.map_err(mongo_error)?;
        let version = build.get_str("version").unwrap_or_default().to_string();
        let hello = client.database("admin").run_command(doc! { "hello": 1 }).await.ok();
        let flavor = match &hello {
            Some(h) if h.get_str("msg").ok() == Some("isdbgrid") => "MongoDB (sharded)",
            Some(h) if h.get_str("setName").is_ok() => "MongoDB (replica set)",
            _ => "MongoDB",
        };
        let session = MongoSession { client, default_db: default_db.clone(), read_only: cfg.read_only };
        let info = ServerInfo {
            session_id: String::new(),
            connection_id: cfg.id.clone(),
            engine: Engine::Mongodb,
            flavor: flavor.into(),
            version,
            database: default_db,
            default_schema: String::new(),
            user: cfg.username.trim().to_string(),
            read_only: cfg.read_only,
        };
        Ok((session, info))
    }

    fn guard_write(&self) -> AppResult<()> {
        if self.read_only {
            return Err(AppError::invalid("This connection is read-only, so Rowglow blocked the change.").with_code("read_only"));
        }
        Ok(())
    }

    pub async fn databases(&self) -> AppResult<Vec<String>> {
        match self.client.list_database_names().authorized_databases(true).await {
            Ok(mut v) => {
                if !v.contains(&self.default_db) {
                    v.push(self.default_db.clone());
                }
                v.sort();
                Ok(v)
            }
            // Users without listDatabases still get their own database.
            Err(e) if matches!(e.kind.as_ref(), ErrorKind::Command(c) if c.code == 13) => Ok(vec![self.default_db.clone()]),
            Err(e) => Err(mongo_error(e)),
        }
    }

    pub async fn collections(&self, db: &str) -> AppResult<Json> {
        let database = self.client.database(db);
        let specs: Vec<mongodb::results::CollectionSpecification> = database
            .list_collections()
            .await
            .map_err(mongo_error)?
            .try_collect()
            .await
            .map_err(mongo_error)?;
        let mut out = Vec::new();
        for s in specs.into_iter().filter(|s| !s.name.starts_with("system.")) {
            let kind = format!("{:?}", s.collection_type).to_lowercase();
            let count = if kind == "collection" && out.len() < 300 {
                tokio::time::timeout(
                    Duration::from_secs(3),
                    database.collection::<Document>(&s.name).estimated_document_count(),
                )
                .await
                .ok()
                .and_then(Result::ok)
            } else {
                None
            };
            out.push(json!({ "name": s.name, "kind": kind, "count": count }));
        }
        out.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
        Ok(json!(out))
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn find(
        &self,
        db: &str,
        coll: &str,
        filter: &str,
        sort: &str,
        projection: &str,
        skip: u64,
        limit: i64,
    ) -> AppResult<Json> {
        let start = Instant::now();
        let c = self.client.database(db).collection::<Document>(coll);
        let filter = parse_doc(filter, "filter")?;
        let sort = parse_doc(sort, "sort")?;
        let projection = parse_doc(projection, "projection")?;
        let limit = limit.clamp(1, MAX_DOCS);
        let mut find = c.find(filter.clone()).skip(skip).limit(limit).max_time(QUERY_TIME);
        if !sort.is_empty() {
            find = find.sort(sort);
        }
        if !projection.is_empty() {
            find = find.projection(projection);
        }
        let docs: Vec<Document> = find.await.map_err(mongo_error)?.try_collect().await.map_err(mongo_error)?;
        let ms = start.elapsed().as_secs_f64() * 1000.0;
        // Exact count, but don't let it hold the page up on huge collections.
        let total = if filter.is_empty() {
            c.estimated_document_count().await.ok()
        } else {
            c.count_documents(filter).max_time(Duration::from_secs(5)).await.ok()
        };
        Ok(json!({
            "docs": docs.into_iter().map(|d| to_json(Bson::Document(d))).collect::<Vec<_>>(),
            "total": total,
            "durationMs": ms,
        }))
    }

    pub async fn aggregate(&self, db: &str, coll: &str, pipeline: &str) -> AppResult<Json> {
        let start = Instant::now();
        let mut stages = parse_pipeline(pipeline)?;
        if writes_in_pipeline(&stages) {
            self.guard_write()?;
        } else if !stages.iter().any(|s| s.contains_key("$limit")) {
            stages.push(doc! { "$limit": MAX_DOCS });
        }
        let c = self.client.database(db).collection::<Document>(coll);
        let docs: Vec<Document> = c
            .aggregate(stages)
            .max_time(QUERY_TIME)
            .await
            .map_err(mongo_error)?
            .try_collect()
            .await
            .map_err(mongo_error)?;
        let truncated = docs.len() as i64 >= MAX_DOCS;
        Ok(json!({
            "docs": docs.into_iter().take(MAX_DOCS as usize).map(|d| to_json(Bson::Document(d))).collect::<Vec<_>>(),
            "truncated": truncated,
            "durationMs": start.elapsed().as_secs_f64() * 1000.0,
        }))
    }

    pub async fn insert(&self, db: &str, coll: &str, docs: &[String]) -> AppResult<Json> {
        self.guard_write()?;
        let parsed: Vec<Document> = docs.iter().map(|d| parse_doc(d, "document")).collect::<AppResult<_>>()?;
        if parsed.is_empty() {
            return Err(AppError::invalid("Nothing to insert"));
        }
        let r = self
            .client
            .database(db)
            .collection::<Document>(coll)
            .insert_many(parsed)
            .await
            .map_err(mongo_error)?;
        let mut ids: Vec<(usize, Bson)> = r.inserted_ids.into_iter().collect();
        ids.sort_by_key(|(i, _)| *i);
        Ok(json!({ "inserted": ids.len(), "ids": ids.into_iter().map(|(_, id)| to_json(id)).collect::<Vec<_>>() }))
    }

    /// Replaces the document with this _id. The new document keeps the same _id.
    pub async fn replace(&self, db: &str, coll: &str, id: &str, doc_text: &str) -> AppResult<Json> {
        self.guard_write()?;
        let id = parse_ejson(id)?;
        let mut doc = parse_doc(doc_text, "document")?;
        match doc.get("_id") {
            Some(new_id) if *new_id != id => {
                return Err(AppError::invalid("A document's _id can't be changed. Insert a copy and delete the original instead."))
            }
            _ => {
                doc.insert("_id", id.clone());
            }
        }
        let r = self
            .client
            .database(db)
            .collection::<Document>(coll)
            .replace_one(doc! { "_id": id }, doc)
            .await
            .map_err(mongo_error)?;
        if r.matched_count != 1 {
            return Err(AppError::new("db", "This document no longer exists (it was deleted elsewhere). Nothing was saved.").with_code("row_not_found"));
        }
        Ok(json!({ "matched": r.matched_count, "modified": r.modified_count }))
    }

    pub async fn delete(&self, db: &str, coll: &str, ids: &[String]) -> AppResult<Json> {
        self.guard_write()?;
        let ids: Vec<Bson> = ids.iter().map(|i| parse_ejson(i)).collect::<AppResult<_>>()?;
        if ids.is_empty() {
            return Ok(json!({ "deleted": 0 }));
        }
        let n = ids.len() as u64;
        let r = self
            .client
            .database(db)
            .collection::<Document>(coll)
            .delete_many(doc! { "_id": { "$in": ids } })
            .await
            .map_err(mongo_error)?;
        Ok(json!({ "deleted": r.deleted_count, "missing": n - r.deleted_count.min(n) }))
    }

    pub async fn indexes(&self, db: &str, coll: &str) -> AppResult<Json> {
        let idx: Vec<mongodb::IndexModel> = self
            .client
            .database(db)
            .collection::<Document>(coll)
            .list_indexes()
            .await
            .map_err(mongo_error)?
            .try_collect()
            .await
            .map_err(mongo_error)?;
        Ok(json!(idx
            .into_iter()
            .map(|i| {
                let o = i.options.unwrap_or_default();
                json!({
                    "name": o.name,
                    "keys": to_json(Bson::Document(i.keys)),
                    "unique": o.unique.unwrap_or(false),
                    "sparse": o.sparse.unwrap_or(false),
                    "ttlSeconds": o.expire_after.map(|d| d.as_secs()),
                    "partial": o.partial_filter_expression.map(|p| to_json(Bson::Document(p))),
                })
            })
            .collect::<Vec<_>>()))
    }

    pub async fn create_collection(&self, db: &str, name: &str) -> AppResult<()> {
        self.guard_write()?;
        if name.trim().is_empty() {
            return Err(AppError::invalid("Enter a collection name"));
        }
        self.client.database(db).create_collection(name.trim()).await.map_err(mongo_error)
    }

    pub async fn drop_collection(&self, db: &str, name: &str) -> AppResult<()> {
        self.guard_write()?;
        self.client.database(db).collection::<Document>(name).drop().await.map_err(mongo_error)
    }

    /// Runs a database command, e.g. `{ "dbStats": 1 }`.
    pub async fn command(&self, db: &str, text: &str, confirmed: bool) -> AppResult<Json> {
        let cmd = parse_doc(text, "command")?;
        let Some(name) = cmd.keys().next().cloned() else {
            return Err(AppError::invalid("Write a command, e.g. { \"dbStats\": 1 }"));
        };
        let pipeline_writes = cmd.get_array("pipeline").map(|p| {
            p.iter().any(|s| s.as_document().map(|d| d.contains_key("$out") || d.contains_key("$merge")).unwrap_or(false))
        });
        let is_read = READ_COMMANDS.contains(&name.as_str()) && !pipeline_writes.unwrap_or(false);
        if self.read_only && !is_read {
            return Err(AppError::invalid(format!("This connection is read-only, so Rowglow blocked {name}.")).with_code("read_only"));
        }
        if !confirmed && DANGEROUS_COMMANDS.contains(&name.as_str()) {
            let mut e = AppError::invalid(format!("{name} can permanently remove data or disrupt the server.")).with_code("confirm_required");
            e.detail = Some(text.to_string());
            return Err(e);
        }
        let start = Instant::now();
        let r = self.client.database(db).run_command(cmd).await.map_err(mongo_error)?;
        Ok(json!({ "result": to_json(Bson::Document(r)), "durationMs": start.elapsed().as_secs_f64() * 1000.0, "write": !is_read }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_credentials_out_of_addresses() {
        assert_eq!(
            split_uri_credentials("mongodb+srv://app:p%40ss%3Aw0rd@cluster0.abc.mongodb.net/?retryWrites=true"),
            Some(("mongodb+srv://cluster0.abc.mongodb.net/?retryWrites=true".into(), "app".into(), "p@ss:w0rd".into()))
        );
        assert_eq!(
            split_uri_credentials("mongodb://u:p@h1:27017,h2:27017/shop?authSource=admin"),
            Some(("mongodb://h1:27017,h2:27017/shop?authSource=admin".into(), "u".into(), "p".into()))
        );
        // No password in the address: left alone.
        assert_eq!(split_uri_credentials("mongodb://u@localhost:27017"), None);
        assert_eq!(split_uri_credentials("mongodb://localhost:27017"), None);
        assert_eq!(split_uri_credentials("localhost"), None);
        assert_eq!(percent_decode("100%"), "100%");
    }

    #[test]
    fn extended_json_roundtrip() {
        let b = parse_ejson(r#"{"_id": {"$oid": "65f0c0ffee0000000000abcd"}, "at": {"$date": "2024-05-01T10:00:00Z"}, "n": {"$numberLong": "9007199254740993"}, "price": {"$numberDecimal": "12.50"}}"#).unwrap();
        let d = b.as_document().unwrap();
        assert!(matches!(d.get("_id"), Some(Bson::ObjectId(_))));
        assert!(matches!(d.get("at"), Some(Bson::DateTime(_))));
        assert!(matches!(d.get("n"), Some(Bson::Int64(9007199254740993))));
        assert!(matches!(d.get("price"), Some(Bson::Decimal128(_))));
        let back = to_json(b);
        assert_eq!(back["_id"]["$oid"], "65f0c0ffee0000000000abcd");
        assert_eq!(back["price"]["$numberDecimal"], "12.50");
        assert!(parse_doc("[1]", "filter").is_err());
        assert_eq!(parse_pipeline(r#"[{"$match": {}}, {"$out": "x"}]"#).map(|p| writes_in_pipeline(&p)).unwrap(), true);
    }
}
