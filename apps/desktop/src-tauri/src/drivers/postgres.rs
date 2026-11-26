//! PostgreSQL driver.
//!
//! Values are fetched with the *simple query* protocol, so the server renders
//! every value as text exactly like psql does (numeric precision, intervals,
//! arrays, ranges, PostGIS, enums, custom types…). Column type names come from
//! a `prepare` of row-returning statements.

use super::tls::make_pg_tls;
use crate::error::{AppError, AppResult};
use crate::model::*;
use crate::util::{first_keyword, os_user, quote_ident};
use futures_util::{pin_mut, StreamExt};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio_postgres::config::SslMode as PgSsl;
use tokio_postgres::types::ToSql;
use tokio_postgres::{AsyncMessage, CancelToken, Client, SimpleQueryMessage};

pub struct PgConn {
    pub client: Client,
    notices: Arc<Mutex<Vec<String>>>,
    cancel: CancelToken,
}

pub struct PgSession {
    pub editor: PgConn,
    pub aux: PgConn,
}

type Rows = Vec<Vec<Option<String>>>;

async fn connect_conn(
    cfg: &ConnectionConfig,
    password: Option<&str>,
    database: &str,
) -> AppResult<PgConn> {
    let mut c = tokio_postgres::Config::new();
    let host = if cfg.host.trim().is_empty() {
        "localhost".to_string()
    } else {
        cfg.host.trim().to_string()
    };
    c.host(&host);
    c.port(if cfg.port == 0 { 5432 } else { cfg.port });
    let user = if cfg.username.trim().is_empty() {
        os_user()
    } else {
        cfg.username.trim().to_string()
    };
    c.user(&user);
    if let Some(p) = password {
        if !p.is_empty() {
            c.password(p);
        }
    }
    c.dbname(if database.is_empty() { "postgres" } else { database });
    c.application_name("Rowglow");
    c.connect_timeout(Duration::from_secs(10));
    c.ssl_mode(match cfg.ssl_mode {
        SslMode::Disable => PgSsl::Disable,
        SslMode::Prefer => PgSsl::Prefer,
        SslMode::Require => PgSsl::Require,
    });
    let (client, mut connection) =
        tokio::time::timeout(Duration::from_secs(15), c.connect(make_pg_tls()))
            .await
            .map_err(|_| AppError::new("connection", "Timed out while connecting to the server"))??;

    let notices = Arc::new(Mutex::new(Vec::new()));
    let sink = notices.clone();
    tokio::spawn(async move {
        let stream = futures_util::stream::poll_fn(move |cx| connection.poll_message(cx));
        pin_mut!(stream);
        while let Some(msg) = stream.next().await {
            match msg {
                Ok(AsyncMessage::Notice(n)) => {
                    if let Ok(mut v) = sink.lock() {
                        if v.len() < 500 {
                            v.push(format!("{}: {}", n.severity(), n.message()));
                        }
                    }
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }
    });
    let cancel = client.cancel_token();
    Ok(PgConn {
        client,
        notices,
        cancel,
    })
}

fn wants_types(sql: &str) -> bool {
    matches!(
        first_keyword(sql).as_str(),
        "SELECT" | "WITH" | "TABLE" | "VALUES" | "SHOW" | "EXPLAIN"
    ) || sql.to_ascii_lowercase().contains("returning")
}

impl PgConn {
    pub async fn run(&self, sql: &str, limit: usize) -> AppResult<QueryResult> {
        let start = Instant::now();
        if let Ok(mut n) = self.notices.lock() {
            n.clear();
        }
        let types: Option<Vec<String>> = if wants_types(sql) {
            match self.client.prepare(sql).await {
                Ok(st) => Some(
                    st.columns()
                        .iter()
                        .map(|c| c.type_().name().to_string())
                        .collect(),
                ),
                Err(_) => None,
            }
        } else {
            None
        };

        let stream = self.client.simple_query_raw(sql).await?;
        pin_mut!(stream);
        let mut out = QueryResult::default();
        let mut current_set = 0usize;
        let mut target_set: Option<usize> = None;
        while let Some(msg) = stream.next().await {
            match msg? {
                SimpleQueryMessage::RowDescription(desc) => {
                    if target_set.is_none() {
                        target_set = Some(current_set);
                        out.has_rows = true;
                        out.columns = desc
                            .iter()
                            .enumerate()
                            .map(|(i, c)| Column {
                                name: c.name().to_string(),
                                type_name: types
                                    .as_ref()
                                    .and_then(|t| t.get(i).cloned())
                                    .unwrap_or_default(),
                            })
                            .collect();
                    }
                }
                SimpleQueryMessage::Row(row) => {
                    if target_set == Some(current_set) {
                        if out.rows.len() < limit {
                            out.rows
                                .push((0..row.len()).map(|i| row.get(i).map(str::to_string)).collect());
                        } else {
                            out.truncated = true;
                        }
                    }
                }
                SimpleQueryMessage::CommandComplete(n) => {
                    if out.rows_affected.is_none() {
                        out.rows_affected = Some(n);
                    }
                    current_set += 1;
                }
                _ => {}
            }
        }
        out.duration_ms = start.elapsed().as_secs_f64() * 1000.0;
        if let Ok(mut n) = self.notices.lock() {
            out.notices = std::mem::take(&mut *n);
        }
        Ok(out)
    }

    async fn rows(&self, sql: &str, params: &[&(dyn ToSql + Sync)]) -> AppResult<Rows> {
        let rows = self.client.query(sql, params).await?;
        Ok(rows
            .iter()
            .map(|r| (0..r.len()).map(|i| r.get::<_, Option<String>>(i)).collect())
            .collect())
    }

    async fn simple(&self, sql: &str) -> AppResult<()> {
        self.client.batch_execute(sql).await?;
        Ok(())
    }
}

fn s(v: &[Option<String>], i: usize) -> String {
    v.get(i).cloned().flatten().unwrap_or_default()
}
fn o(v: &[Option<String>], i: usize) -> Option<String> {
    v.get(i).cloned().flatten()
}
fn json_list(v: &[Option<String>], i: usize) -> Vec<String> {
    o(v, i)
        .and_then(|j| serde_json::from_str::<Vec<String>>(&j).ok())
        .unwrap_or_default()
}
fn fk_action(code: &str) -> String {
    match code {
        "r" => "RESTRICT",
        "c" => "CASCADE",
        "n" => "SET NULL",
        "d" => "SET DEFAULT",
        _ => "NO ACTION",
    }
    .to_string()
}

const REGCLASS: &str = "format('%I.%I', $1::text, $2::text)::regclass";

impl PgSession {
    pub async fn open(
        cfg: &ConnectionConfig,
        password: Option<&str>,
        database: &str,
    ) -> AppResult<(PgSession, ServerInfo)> {
        let editor = connect_conn(cfg, password, database).await?;
        let aux = connect_conn(cfg, password, database).await?;
        if cfg.read_only {
            let sql = "SET SESSION CHARACTERISTICS AS TRANSACTION READ ONLY";
            editor.simple(sql).await?;
            aux.simple(sql).await?;
        }
        // The aux connection runs SQL that Rowglow generates (staged edits, filters,
        // exports). Pin the literal syntax our quoting relies on.
        let _ = aux.simple("SET standard_conforming_strings = on").await;
        let r = aux
            .rows(
                "SELECT current_setting('server_version'), current_user::text, current_schema()::text, current_database()::text, version()",
                &[],
            )
            .await?;
        let row = r.first().cloned().unwrap_or_default();
        let full = s(&row, 4);
        let flavor = if full.contains("CockroachDB") {
            "CockroachDB"
        } else if full.contains("Redshift") {
            "Redshift"
        } else {
            "PostgreSQL"
        };
        let info = ServerInfo {
            session_id: String::new(),
            connection_id: cfg.id.clone(),
            engine: Engine::Postgres,
            flavor: flavor.into(),
            version: s(&row, 0).split_whitespace().next().unwrap_or("").to_string(),
            user: s(&row, 1),
            default_schema: o(&row, 2).unwrap_or_else(|| "public".into()),
            database: s(&row, 3),
            read_only: cfg.read_only,
        };
        Ok((PgSession { editor, aux }, info))
    }

    pub async fn cancel(&self) -> AppResult<()> {
        self.editor.cancel.cancel_query(make_pg_tls()).await?;
        Ok(())
    }

    pub fn is_closed(&self) -> bool {
        self.editor.client.is_closed() || self.aux.client.is_closed()
    }

    pub async fn transaction(&self, stmts: &[String], expect_one: bool) -> Result<Vec<QueryResult>, (usize, AppError)> {
        let conn = &self.aux;
        conn.simple("BEGIN").await.map_err(|e| (0, e))?;
        let _ = conn.simple("SET LOCAL lock_timeout = '10s'").await;
        let mut results = Vec::new();
        for (i, sql) in stmts.iter().enumerate() {
            let res = conn.run(sql, 1000).await.and_then(|r| super::check_one(expect_one, r));
            match res {
                Ok(r) => results.push(r),
                Err(e) => {
                    let _ = conn.simple("ROLLBACK").await;
                    return Err((i, e));
                }
            }
        }
        conn.simple("COMMIT").await.map_err(|e| (stmts.len(), e))?;
        Ok(results)
    }

    pub async fn use_schema(&self, schema: &str) -> AppResult<()> {
        let sql = format!(
            "SET search_path TO {}, public",
            quote_ident(Engine::Postgres, schema)
        );
        self.editor.simple(&sql).await?;
        Ok(())
    }

    pub async fn databases(&self) -> AppResult<Vec<String>> {
        let rows = self
            .aux
            .rows(
                "SELECT datname::text FROM pg_database WHERE datallowconn AND NOT datistemplate ORDER BY datname",
                &[],
            )
            .await?;
        Ok(rows.iter().map(|r| s(r, 0)).collect())
    }

    pub async fn schemas(&self) -> AppResult<Vec<String>> {
        let rows = self
            .aux
            .rows(
                "SELECT nspname::text FROM pg_namespace
                 WHERE nspname NOT LIKE 'pg_toast%' AND nspname NOT LIKE 'pg_temp_%'
                 ORDER BY (nspname IN ('pg_catalog', 'information_schema')), nspname",
                &[],
            )
            .await?;
        Ok(rows.iter().map(|r| s(r, 0)).collect())
    }

    pub async fn objects(&self, schema: &str) -> AppResult<Vec<DbObject>> {
        let mut out = Vec::new();
        let rels = self
            .aux
            .rows(
                "SELECT c.relname::text,
                        CASE c.relkind WHEN 'r' THEN 'table' WHEN 'p' THEN 'table' WHEN 'v' THEN 'view'
                             WHEN 'm' THEN 'matview' WHEN 'f' THEN 'foreign' WHEN 'S' THEN 'sequence' END,
                        c.reltuples::bigint::text,
                        obj_description(c.oid, 'pg_class')
                 FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
                 WHERE n.nspname = $1::text AND c.relkind IN ('r','p','v','m','f','S')
                 ORDER BY c.relname",
                &[&schema],
            )
            .await?;
        for r in rels {
            let est = o(&r, 2).and_then(|v| v.parse::<i64>().ok()).filter(|v| *v >= 0);
            out.push(DbObject {
                name: s(&r, 0),
                kind: s(&r, 1),
                rows_estimate: est,
                comment: o(&r, 3),
                signature: None,
            });
        }
        let funcs = match self
            .aux
            .rows(
                "SELECT p.proname::text, pg_get_function_identity_arguments(p.oid),
                        CASE WHEN p.prokind = 'p' THEN 'procedure' ELSE 'function' END
                 FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace
                 WHERE n.nspname = $1::text AND p.prokind IN ('f', 'p')
                   AND NOT EXISTS (SELECT 1 FROM pg_depend d WHERE d.objid = p.oid AND d.deptype = 'e')
                 ORDER BY p.proname",
                &[&schema],
            )
            .await
        {
            Ok(r) => r,
            Err(_) => self
                .aux
                .rows(
                    "SELECT p.proname::text, pg_get_function_identity_arguments(p.oid), 'function'
                     FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace
                     WHERE n.nspname = $1::text AND NOT p.proisagg ORDER BY p.proname",
                    &[&schema],
                )
                .await
                .unwrap_or_default(),
        };
        for r in funcs {
            out.push(DbObject {
                name: s(&r, 0),
                kind: s(&r, 2),
                rows_estimate: None,
                comment: None,
                signature: Some(s(&r, 1)),
            });
        }
        let types = self
            .aux
            .rows(
                "SELECT t.typname::text, CASE t.typtype WHEN 'e' THEN 'enum' ELSE 'domain' END
                 FROM pg_type t JOIN pg_namespace n ON n.oid = t.typnamespace
                 WHERE n.nspname = $1::text AND t.typtype IN ('e', 'd')
                   AND NOT EXISTS (SELECT 1 FROM pg_depend d WHERE d.objid = t.oid AND d.deptype = 'e')
                 ORDER BY t.typname",
                &[&schema],
            )
            .await
            .unwrap_or_default();
        for r in types {
            out.push(DbObject {
                name: s(&r, 0),
                kind: "type".into(),
                rows_estimate: None,
                comment: Some(s(&r, 1)),
                signature: None,
            });
        }
        Ok(out)
    }

    pub async fn structure(&self, schema: &str, table: &str) -> AppResult<TableStructure> {
        let p: [&(dyn ToSql + Sync); 2] = [&schema, &table];
        let cols = match self
            .aux
            .rows(
                &format!(
                    "SELECT a.attname::text, format_type(a.atttypid, a.atttypmod), (NOT a.attnotnull)::text,
                            pg_get_expr(d.adbin, d.adrelid), col_description(a.attrelid, a.attnum),
                            CASE WHEN a.attidentity = 'a' THEN 'identity always'
                                 WHEN a.attidentity = 'd' THEN 'identity by default'
                                 WHEN a.attgenerated = 's' THEN 'generated' END
                     FROM pg_attribute a
                     LEFT JOIN pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum
                     WHERE a.attrelid = {REGCLASS} AND a.attnum > 0 AND NOT a.attisdropped
                     ORDER BY a.attnum"
                ),
                &p,
            )
            .await
        {
            Ok(r) => r,
            Err(_) => {
                self.aux
                    .rows(
                        &format!(
                            "SELECT a.attname::text, format_type(a.atttypid, a.atttypmod), (NOT a.attnotnull)::text,
                                    pg_get_expr(d.adbin, d.adrelid), col_description(a.attrelid, a.attnum), NULL::text
                             FROM pg_attribute a
                             LEFT JOIN pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum
                             WHERE a.attrelid = {REGCLASS} AND a.attnum > 0 AND NOT a.attisdropped
                             ORDER BY a.attnum"
                        ),
                        &p,
                    )
                    .await?
            }
        };
        let pk: Vec<String> = self
            .aux
            .rows(
                &format!(
                    "SELECT a.attname::text FROM pg_index i
                     CROSS JOIN LATERAL unnest(i.indkey::int2[]) WITH ORDINALITY AS k(attnum, ord)
                     JOIN pg_attribute a ON a.attrelid = i.indrelid AND a.attnum = k.attnum
                     WHERE i.indrelid = {REGCLASS} AND i.indisprimary ORDER BY k.ord"
                ),
                &p,
            )
            .await?
            .iter()
            .map(|r| s(r, 0))
            .collect();
        let columns = cols
            .iter()
            .map(|r| ColumnDef {
                name: s(r, 0),
                data_type: s(r, 1),
                nullable: s(r, 2) == "true",
                default_value: o(r, 3),
                is_primary_key: pk.contains(&s(r, 0)),
                comment: o(r, 4),
                extra: o(r, 5),
            })
            .collect();
        let indexes = self
            .aux
            .rows(
                &format!(
                    "SELECT ic.relname::text, pg_get_indexdef(i.indexrelid), i.indisunique::text, i.indisprimary::text,
                            (SELECT array_to_json(array_agg(a.attname::text ORDER BY k.ord))::text
                             FROM unnest(i.indkey::int2[]) WITH ORDINALITY k(attnum, ord)
                             JOIN pg_attribute a ON a.attrelid = i.indrelid AND a.attnum = k.attnum)
                     FROM pg_index i JOIN pg_class ic ON ic.oid = i.indexrelid
                     WHERE i.indrelid = {REGCLASS}
                     ORDER BY i.indisprimary DESC, ic.relname"
                ),
                &p,
            )
            .await?
            .iter()
            .map(|r| IndexDef {
                name: s(r, 0),
                definition: o(r, 1),
                unique: s(r, 2) == "true",
                primary: s(r, 3) == "true",
                columns: json_list(r, 4),
            })
            .collect();
        let constraints = self
            .aux
            .rows(
                &format!(
                    "SELECT conname::text, contype::text, pg_get_constraintdef(oid)
                     FROM pg_constraint WHERE conrelid = {REGCLASS} AND contype <> 'n' ORDER BY contype, conname"
                ),
                &p,
            )
            .await?
            .iter()
            .map(|r| ConstraintDef {
                name: s(r, 0),
                kind: match s(r, 1).as_str() {
                    "p" => "primary",
                    "u" => "unique",
                    "c" => "check",
                    "f" => "foreign",
                    "x" => "exclusion",
                    _ => "other",
                }
                .into(),
                definition: s(r, 2),
            })
            .collect();
        let cols_of = |rel: &str, key: &str| {
            format!(
                "(SELECT array_to_json(array_agg(a.attname::text ORDER BY k.ord))::text
                  FROM unnest(con.{key}) WITH ORDINALITY k(attnum, ord)
                  JOIN pg_attribute a ON a.attrelid = con.{rel} AND a.attnum = k.attnum)"
            )
        };
        let fks = self
            .aux
            .rows(
                &format!(
                    "SELECT con.conname::text, {}, fn.nspname::text, fc.relname::text, {},
                            con.confupdtype::text, con.confdeltype::text
                     FROM pg_constraint con
                     JOIN pg_class fc ON fc.oid = con.confrelid
                     JOIN pg_namespace fn ON fn.oid = fc.relnamespace
                     WHERE con.contype = 'f' AND con.conrelid = {REGCLASS}
                     ORDER BY con.conname",
                    cols_of("conrelid", "conkey"),
                    cols_of("confrelid", "confkey")
                ),
                &p,
            )
            .await?
            .iter()
            .map(|r| ForeignKeyDef {
                name: s(r, 0),
                columns: json_list(r, 1),
                ref_schema: s(r, 2),
                ref_table: s(r, 3),
                ref_columns: json_list(r, 4),
                on_update: fk_action(&s(r, 5)),
                on_delete: fk_action(&s(r, 6)),
            })
            .collect();
        let referenced_by = self
            .aux
            .rows(
                &format!(
                    "SELECT con.conname::text, {}, sn.nspname::text, sc.relname::text, {},
                            con.confupdtype::text, con.confdeltype::text
                     FROM pg_constraint con
                     JOIN pg_class sc ON sc.oid = con.conrelid
                     JOIN pg_namespace sn ON sn.oid = sc.relnamespace
                     WHERE con.contype = 'f' AND con.confrelid = {REGCLASS}
                     ORDER BY sc.relname, con.conname",
                    cols_of("confrelid", "confkey"),
                    cols_of("conrelid", "conkey")
                ),
                &p,
            )
            .await?
            .iter()
            .map(|r| ForeignKeyDef {
                name: s(r, 0),
                columns: json_list(r, 1),
                ref_schema: s(r, 2),
                ref_table: s(r, 3),
                ref_columns: json_list(r, 4),
                on_update: fk_action(&s(r, 5)),
                on_delete: fk_action(&s(r, 6)),
            })
            .collect();
        let triggers = self
            .aux
            .rows(
                &format!(
                    "SELECT tgname::text, pg_get_triggerdef(oid) FROM pg_trigger
                     WHERE tgrelid = {REGCLASS} AND NOT tgisinternal ORDER BY tgname"
                ),
                &p,
            )
            .await?
            .iter()
            .map(|r| TriggerDef {
                name: s(r, 0),
                definition: s(r, 1),
            })
            .collect();
        Ok(TableStructure {
            columns,
            primary_key: pk,
            indexes,
            foreign_keys: fks,
            constraints,
            triggers,
            referenced_by,
        })
    }

    pub async fn ddl(&self, schema: &str, name: &str, kind: &str, signature: Option<&str>) -> AppResult<String> {
        let qn = format!(
            "{}.{}",
            quote_ident(Engine::Postgres, schema),
            quote_ident(Engine::Postgres, name)
        );
        match kind {
            "view" | "matview" => {
                let r = self
                    .aux
                    .rows(&format!("SELECT pg_get_viewdef({REGCLASS}, true)"), &[&schema, &name])
                    .await?;
                let body = r.first().map(|r| s(r, 0)).unwrap_or_default();
                let head = if kind == "view" {
                    "CREATE OR REPLACE VIEW"
                } else {
                    "CREATE MATERIALIZED VIEW"
                };
                Ok(format!("{head} {qn} AS\n{}", body.trim_end()))
            }
            "function" | "procedure" => {
                let sig = signature.unwrap_or("");
                let r = self
                    .aux
                    .rows(
                        "SELECT pg_get_functiondef(p.oid) FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace
                         WHERE n.nspname = $1::text AND p.proname = $2::text
                           AND pg_get_function_identity_arguments(p.oid) = $3::text",
                        &[&schema, &name, &sig],
                    )
                    .await?;
                Ok(r.first().map(|r| s(r, 0)).unwrap_or_default())
            }
            "sequence" => {
                let r = self
                    .aux
                    .rows(
                        "SELECT format('CREATE SEQUENCE %I.%I\n    START %s\n    INCREMENT %s\n    MINVALUE %s\n    MAXVALUE %s\n    CACHE %s%s;',
                                schemaname, sequencename, start_value, increment_by, min_value, max_value, cache_size,
                                CASE WHEN cycle THEN '\n    CYCLE' ELSE '' END)
                         FROM pg_sequences WHERE schemaname = $1::text AND sequencename = $2::text",
                        &[&schema, &name],
                    )
                    .await?;
                Ok(r.first().map(|r| s(r, 0)).unwrap_or_default())
            }
            "type" => {
                let r = self
                    .aux
                    .rows(
                        "SELECT format('CREATE TYPE %I.%I AS ENUM (%s);', n.nspname, t.typname,
                                string_agg(quote_literal(e.enumlabel), ', ' ORDER BY e.enumsortorder))
                         FROM pg_type t JOIN pg_namespace n ON n.oid = t.typnamespace
                         JOIN pg_enum e ON e.enumtypid = t.oid
                         WHERE n.nspname = $1::text AND t.typname = $2::text
                         GROUP BY n.nspname, t.typname
                         UNION ALL
                         SELECT format('CREATE DOMAIN %I.%I AS %s%s;', n.nspname, t.typname,
                                format_type(t.typbasetype, t.typtypmod),
                                CASE WHEN t.typnotnull THEN ' NOT NULL' ELSE '' END)
                         FROM pg_type t JOIN pg_namespace n ON n.oid = t.typnamespace
                         WHERE n.nspname = $1::text AND t.typname = $2::text AND t.typtype = 'd'",
                        &[&schema, &name],
                    )
                    .await?;
                Ok(r.first().map(|r| s(r, 0)).unwrap_or_default())
            }
            _ => self.table_ddl(schema, name, &qn).await,
        }
    }

    async fn table_ddl(&self, schema: &str, name: &str, qn: &str) -> AppResult<String> {
        let st = self.structure(schema, name).await?;
        let mut lines: Vec<String> = Vec::new();
        for c in &st.columns {
            let mut l = format!("    {} {}", quote_ident(Engine::Postgres, &c.name), c.data_type);
            match c.extra.as_deref() {
                Some("identity always") => l.push_str(" GENERATED ALWAYS AS IDENTITY"),
                Some("identity by default") => l.push_str(" GENERATED BY DEFAULT AS IDENTITY"),
                Some("generated") => {
                    if let Some(d) = &c.default_value {
                        l.push_str(&format!(" GENERATED ALWAYS AS ({d}) STORED"));
                    }
                }
                _ => {
                    if let Some(d) = &c.default_value {
                        l.push_str(&format!(" DEFAULT {d}"));
                    }
                }
            }
            if !c.nullable {
                l.push_str(" NOT NULL");
            }
            lines.push(l);
        }
        for con in &st.constraints {
            lines.push(format!(
                "    CONSTRAINT {} {}",
                quote_ident(Engine::Postgres, &con.name),
                con.definition
            ));
        }
        let mut out = format!("CREATE TABLE {qn} (\n{}\n);\n", lines.join(",\n"));
        let constraint_names: Vec<&str> = st.constraints.iter().map(|c| c.name.as_str()).collect();
        for idx in &st.indexes {
            if !constraint_names.contains(&idx.name.as_str()) {
                if let Some(d) = &idx.definition {
                    out.push_str(&format!("\n{d};"));
                }
            }
        }
        for t in &st.triggers {
            out.push_str(&format!("\n{};", t.definition));
        }
        for c in &st.columns {
            if let Some(cm) = &c.comment {
                out.push_str(&format!(
                    "\nCOMMENT ON COLUMN {qn}.{} IS {};",
                    quote_ident(Engine::Postgres, &c.name),
                    crate::util::quote_literal(Engine::Postgres, cm)
                ));
            }
        }
        Ok(out.trim_end().to_string() + "\n")
    }

    pub async fn completions(&self) -> AppResult<Vec<CompletionTable>> {
        let rows = self
            .aux
            .rows(
                "SELECT n.nspname::text, c.relname::text,
                        CASE c.relkind WHEN 'v' THEN 'view' WHEN 'm' THEN 'matview' ELSE 'table' END,
                        array_to_json(array_agg(a.attname::text ORDER BY a.attnum))::text,
                        array_to_json(array_agg(format_type(a.atttypid, a.atttypmod) ORDER BY a.attnum))::text
                 FROM pg_class c
                 JOIN pg_namespace n ON n.oid = c.relnamespace
                 JOIN pg_attribute a ON a.attrelid = c.oid AND a.attnum > 0 AND NOT a.attisdropped
                 WHERE c.relkind IN ('r','p','v','m','f')
                   AND n.nspname NOT IN ('pg_catalog', 'information_schema')
                   AND n.nspname NOT LIKE 'pg_toast%' AND n.nspname NOT LIKE 'pg_temp%'
                 GROUP BY n.nspname, c.relname, c.relkind
                 ORDER BY n.nspname, c.relname
                 LIMIT 5000",
                &[],
            )
            .await?;
        Ok(rows
            .iter()
            .map(|r| {
                let names = json_list(r, 3);
                let types = json_list(r, 4);
                CompletionTable {
                    schema: s(r, 0),
                    name: s(r, 1),
                    kind: s(r, 2),
                    columns: names
                        .into_iter()
                        .zip(types.into_iter().chain(std::iter::repeat(String::new())))
                        .collect(),
                }
            })
            .collect())
    }

    pub async fn relations(&self, schema: &str) -> AppResult<SchemaRelations> {
        let rows = self
            .aux
            .rows(
                "SELECT con.contype::text, con.conname::text, c.relname::text,
                        array_to_json(ARRAY(SELECT a.attname::text FROM unnest(con.conkey) WITH ORDINALITY k(n, i)
                              JOIN pg_attribute a ON a.attrelid = con.conrelid AND a.attnum = k.n ORDER BY k.i))::text,
                        fn.nspname::text, fc.relname::text,
                        array_to_json(ARRAY(SELECT a.attname::text FROM unnest(con.confkey) WITH ORDINALITY k(n, i)
                              JOIN pg_attribute a ON a.attrelid = con.confrelid AND a.attnum = k.n ORDER BY k.i))::text
                 FROM pg_constraint con
                 JOIN pg_class c ON c.oid = con.conrelid
                 JOIN pg_namespace n ON n.oid = c.relnamespace
                 LEFT JOIN pg_class fc ON fc.oid = con.confrelid
                 LEFT JOIN pg_namespace fn ON fn.oid = fc.relnamespace
                 WHERE n.nspname = $1 AND con.contype IN ('p', 'f')
                 ORDER BY c.relname, con.conname",
                &[&schema],
            )
            .await?;
        let mut out = SchemaRelations::default();
        for r in rows {
            if s(&r, 0) == "p" {
                out.primary_keys.push(TablePrimaryKey { table: s(&r, 2), columns: json_list(&r, 3) });
            } else {
                out.foreign_keys.push(ForeignKeyRelation {
                    name: s(&r, 1),
                    schema: schema.to_string(),
                    table: s(&r, 2),
                    columns: json_list(&r, 3),
                    ref_schema: s(&r, 4),
                    ref_table: s(&r, 5),
                    ref_columns: json_list(&r, 6),
                });
            }
        }
        Ok(out)
    }
}
