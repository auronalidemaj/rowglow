//! Integration tests against real local servers seeded by `scripts/seed-demo.sh`.
//! Run with: ROWGLOW_IT=1 cargo test --test drivers -- --test-threads=1

use rowglow_lib::drivers::{Channel, Session};
use rowglow_lib::model::{ConnectionConfig, Engine, SslMode};
use std::time::Duration;

fn enabled() -> bool {
    std::env::var("ROWGLOW_IT").is_ok()
}

fn cfg(engine: Engine) -> ConnectionConfig {
    let demo = concat!(env!("CARGO_MANIFEST_DIR"), "/../../../scripts/demo/music.sqlite");
    ConnectionConfig {
        id: "it".into(),
        name: "it".into(),
        engine,
        host: if engine == Engine::Sqlite { String::new() } else { "127.0.0.1".into() },
        port: match engine {
            Engine::Postgres => 5432,
            Engine::Mysql => 3306,
            other => other.default_port(),
        },
        username: match engine {
            Engine::Postgres => std::env::var("USER").unwrap(),
            Engine::Mysql => "root".into(),
            _ => String::new(),
        },
        database: if engine == Engine::Sqlite { String::new() } else { "rowglow_demo".into() },
        ssl_mode: SslMode::Prefer,
        file_path: if engine == Engine::Sqlite { demo.into() } else { String::new() },
        color: String::new(),
        env: "local".into(),
        read_only: false,
        group_name: String::new(),
        ssh_enabled: false,
        ssh_host: String::new(),
        ssh_port: 0,
        ssh_user: String::new(),
        ssh_auth: String::new(),
        ssh_key_path: String::new(),
        created_at: 0,
        updated_at: 0,
        last_used_at: None,
        has_password: false,
        has_ssh_secret: false,
        options: Default::default(),
        sentinel_password: None,
    }
}

async fn open(engine: Engine) -> Session {
    rowglow_lib::init_crypto();
    Session::open("s".into(), &cfg(engine), None, None, None).await.expect("open session")
}

#[tokio::test]
async fn postgres_end_to_end() {
    if !enabled() {
        return;
    }
    let s = open(Engine::Postgres).await;
    assert_eq!(s.info.flavor, "PostgreSQL");
    assert!(s.schemas().await.unwrap().contains(&"shop".to_string()));
    let objs = s.objects("shop").await.unwrap();
    let kinds: Vec<(&str, &str)> = objs.iter().map(|o| (o.name.as_str(), o.kind.as_str())).collect();
    assert!(kinds.contains(&("orders", "table")));
    assert!(kinds.contains(&("customer_revenue", "view")));
    assert!(kinds.contains(&("daily_sales", "matview")));
    assert!(kinds.contains(&("order_count", "function")));
    assert!(kinds.contains(&("order_status", "type")));

    let r = s
        .execute(Channel::Editor, "SELECT id, status, total, placed_at, tags FROM shop.orders o JOIN LATERAL (SELECT ARRAY['a','b'] AS tags) t ON true ORDER BY id LIMIT 5", 3)
        .await
        .unwrap();
    assert!(r.has_rows);
    assert!(r.truncated);
    assert_eq!(r.rows.len(), 3);
    assert_eq!(r.columns[0].type_name, "int8");
    assert_eq!(r.columns[1].type_name, "order_status");
    assert_eq!(r.rows[0][4].as_deref(), Some("{a,b}"));

    let st = s.structure("shop", "order_items").await.unwrap();
    assert_eq!(st.primary_key, vec!["order_id", "product_id"]);
    assert_eq!(st.foreign_keys.len(), 2);
    let cst = s.structure("shop", "customers").await.unwrap();
    assert!(cst.referenced_by.iter().any(|f| f.ref_table == "orders" && f.ref_columns == vec!["customer_id"]));
    assert!(cst.columns.iter().any(|c| c.extra.as_deref() == Some("identity by default")));

    let ddl = s.ddl("shop", "orders", "table", None).await.unwrap();
    assert!(ddl.contains("CREATE TABLE \"shop\".\"orders\""), "{ddl}");
    assert!(ddl.contains("orders_status_placed_idx"));
    let fdef = s.ddl("shop", "order_count", "function", Some("cust bigint")).await.unwrap();
    assert!(fdef.contains("CREATE OR REPLACE FUNCTION"));
    let tdef = s.ddl("shop", "order_status", "type", None).await.unwrap();
    assert!(tdef.contains("ENUM ('pending'"), "{tdef}");

    // Errors carry SQLSTATE + position
    let e = s.execute(Channel::Editor, "SELECT nope FROM shop.orders", 10).await.unwrap_err();
    assert_eq!(e.code.as_deref(), Some("42703"));
    assert_eq!(e.position, Some(8));

    // Transactions roll back on failure
    let before = s.execute(Channel::Aux, "SELECT note FROM shop.orders WHERE id = 1", 1).await.unwrap();
    let res = s
        .transaction(&[
            "UPDATE shop.orders SET note = 'changed' WHERE id = 1".into(),
            "UPDATE shop.orders SET total = 'not a number' WHERE id = 1".into(),
        ], false)
        .await;
    assert_eq!(res.unwrap_err().0, 1);
    let after = s.execute(Channel::Aux, "SELECT note FROM shop.orders WHERE id = 1", 1).await.unwrap();
    assert_eq!(before.rows, after.rows);

    // Notices are collected
    let n = s.execute(Channel::Editor, "DO $$ BEGIN RAISE NOTICE 'hello from plpgsql'; END $$", 1).await.unwrap();
    assert!(n.notices.iter().any(|m| m.contains("hello from plpgsql")), "{:?}", n.notices);

    // Cancel a long query from another task
    let s = std::sync::Arc::new(s);
    let s2 = s.clone();
    let h = tokio::spawn(async move { s2.execute(Channel::Editor, "SELECT pg_sleep(10)", 1).await });
    tokio::time::sleep(Duration::from_millis(400)).await;
    s.cancel().await.unwrap();
    let err = h.await.unwrap().unwrap_err();
    assert_eq!(err.kind, "cancelled");

    let comps = s.completions(None).await.unwrap();
    assert!(comps.iter().any(|t| t.schema == "shop" && t.name == "orders" && t.columns.iter().any(|c| c.0 == "status")));
}

#[tokio::test]
async fn mysql_end_to_end() {
    if !enabled() {
        return;
    }
    let s = open(Engine::Mysql).await;
    assert!(s.info.flavor == "MySQL" || s.info.flavor == "MariaDB");
    assert!(s.schemas().await.unwrap().contains(&"rowglow_demo".to_string()));
    let objs = s.objects("rowglow_demo").await.unwrap();
    assert!(objs.iter().any(|o| o.name == "orders" && o.kind == "table"));
    assert!(objs.iter().any(|o| o.name == "customer_revenue" && o.kind == "view"));

    let r = s
        .execute(Channel::Editor, "SELECT id, status, total, placed_at, metadata, marketing FROM customers c JOIN orders o ON o.customer_id = c.id ORDER BY o.id LIMIT 3", 10)
        .await;
    // ambiguous id → server error with code
    assert!(r.is_err());
    let r = s
        .execute(Channel::Editor, "SELECT o.id, o.status, o.total, o.placed_at, c.metadata FROM customers c JOIN orders o ON o.customer_id = c.id WHERE c.metadata IS NOT NULL ORDER BY o.id LIMIT 3", 10)
        .await
        .unwrap();
    assert_eq!(r.rows.len(), 3);
    assert_eq!(r.columns[0].type_name, "bigint unsigned");
    assert_eq!(r.columns[1].type_name, "enum");
    assert_eq!(r.columns[4].type_name, "json");
    assert!(r.rows[0][3].as_deref().unwrap().contains('.'), "datetime(3) keeps millis");

    let st = s.structure("rowglow_demo", "order_items").await.unwrap();
    assert_eq!(st.primary_key, vec!["order_id", "product_id"]);
    assert_eq!(st.foreign_keys.len(), 2);
    let cst = s.structure("rowglow_demo", "customers").await.unwrap();
    assert!(cst.referenced_by.iter().any(|f| f.ref_table == "orders"));
    let pst = s.structure("rowglow_demo", "products").await.unwrap();
    assert!(pst.constraints.iter().any(|c| c.kind == "check" && c.definition.contains("price")));

    let ddl = s.ddl("rowglow_demo", "orders", "table", None).await.unwrap();
    assert!(ddl.starts_with("CREATE TABLE `orders`"));

    let u = s.execute(Channel::Editor, "UPDATE orders SET note = note WHERE id <= 5", 1).await.unwrap();
    assert!(!u.has_rows);

    let res = s
        .transaction(&[
            "UPDATE orders SET note = 'x' WHERE id = 1".into(),
            "UPDATE orders SET nope = 1 WHERE id = 1".into(),
        ], false)
        .await;
    assert_eq!(res.unwrap_err().0, 1);
    let after = s.execute(Channel::Aux, "SELECT note FROM orders WHERE id = 1", 1).await.unwrap();
    assert_ne!(after.rows[0][0].as_deref(), Some("x"));

    let s = std::sync::Arc::new(s);
    let s2 = s.clone();
    let h = tokio::spawn(async move { s2.execute(Channel::Editor, "SELECT SLEEP(10)", 1).await });
    tokio::time::sleep(Duration::from_millis(500)).await;
    s.cancel().await.unwrap();
    // SLEEP() returns 1 when interrupted rather than erroring
    let r = h.await.unwrap();
    match r {
        Ok(res) => assert_eq!(res.rows[0][0].as_deref(), Some("1")),
        Err(e) => assert_eq!(e.kind, "cancelled"),
    }

    let comps = s.completions(Some("rowglow_demo")).await.unwrap();
    assert!(comps.iter().any(|t| t.name == "orders"));
}

#[tokio::test]
async fn sqlite_end_to_end() {
    if !enabled() {
        return;
    }
    let s = open(Engine::Sqlite).await;
    assert_eq!(s.schemas().await.unwrap(), vec!["main"]);
    let objs = s.objects("main").await.unwrap();
    assert!(objs.iter().any(|o| o.name == "tracks" && o.kind == "table"));
    assert!(objs.iter().any(|o| o.name == "album_lengths" && o.kind == "view"));
    let st = s.structure("main", "albums").await.unwrap();
    assert_eq!(st.primary_key, vec!["id"]);
    assert_eq!(st.foreign_keys.len(), 1);
    assert_eq!(st.foreign_keys[0].ref_table, "artists");
    assert_eq!(st.foreign_keys[0].ref_columns, vec!["id"]);
    assert!(st.referenced_by.iter().any(|f| f.ref_table == "tracks"));
    assert_eq!(st.triggers.len(), 1);
    let r = s.execute(Channel::Editor, "SELECT title, cover FROM albums ORDER BY id", 100).await.unwrap();
    assert_eq!(r.rows[0][1].as_deref(), Some("x'89504E47'"));
    let ddl = s.ddl("main", "albums", "table", None).await.unwrap();
    assert!(ddl.contains("CREATE TABLE albums"));
    assert!(ddl.contains("CREATE INDEX albums_artist"));
    let res = s.transaction(&["UPDATE tracks SET seconds = 1 WHERE id = 1".into(), "UPDATE nope SET x = 1".into()], false).await;
    assert_eq!(res.unwrap_err().0, 1);
}

/// Reads a value from scripts/.test-servers.env (written by scripts/test-servers.sh).
fn test_env(key: &str) -> Option<String> {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../../scripts/.test-servers.env");
    let text = std::fs::read_to_string(path).ok()?;
    text.lines()
        .filter_map(|l| l.split_once('='))
        .find(|(k, _)| k.trim() == key)
        .map(|(_, v)| v.trim().trim_matches('"').to_string())
}

async fn open_mssql(database: &str) -> Option<Session> {
    let pw = test_env("MSSQL_SA_PASSWORD")?;
    rowglow_lib::init_crypto();
    let mut c = cfg(Engine::Mssql);
    c.username = "sa".into();
    if database != "master" {
        // Fresh test containers don't have it yet (tests run in any order).
        c.database = "master".into();
        let m = Session::open("ms0".into(), &c, Some(&pw), None, None).await.expect("open SQL Server session");
        let q = format!("IF DB_ID('{database}') IS NULL CREATE DATABASE {database}");
        m.execute(Channel::Editor, &q, 1).await.expect("create test database");
    }
    c.database = database.into();
    Some(Session::open("ms".into(), &c, Some(&pw), None, None).await.expect("open SQL Server session"))
}

#[tokio::test]
async fn mssql_end_to_end() {
    if !enabled() {
        return;
    }
    let Some(master) = open_mssql("master").await else {
        eprintln!("SQL Server test server not configured; skipping");
        return;
    };
    assert_eq!(master.info.flavor, "SQL Server");
    master
        .execute(Channel::Editor, "IF DB_ID('rowglow_demo') IS NULL CREATE DATABASE rowglow_demo", 1)
        .await
        .unwrap();
    drop(master);
    let s = open_mssql("rowglow_demo").await.unwrap();
    assert_eq!(s.info.database, "rowglow_demo");
    assert_eq!(s.info.default_schema, "dbo");
    for sql in [
        "IF OBJECT_ID('shop.order_audit') IS NOT NULL DROP TABLE shop.order_audit",
        "IF OBJECT_ID('shop.big_orders') IS NOT NULL DROP VIEW shop.big_orders",
        "IF OBJECT_ID('shop.orders') IS NOT NULL DROP TABLE shop.orders",
        "IF OBJECT_ID('shop.customers') IS NOT NULL DROP TABLE shop.customers",
        "IF SCHEMA_ID('shop') IS NULL EXEC('CREATE SCHEMA shop')",
        "CREATE TABLE shop.customers (
            id INT IDENTITY(1,1) CONSTRAINT pk_customers PRIMARY KEY,
            name NVARCHAR(100) NOT NULL,
            email VARCHAR(200) CONSTRAINT uq_customers_email UNIQUE,
            balance DECIMAL(10,2) NOT NULL CONSTRAINT df_customers_balance DEFAULT 0,
            joined DATETIME2(3),
            active BIT,
            avatar VARBINARY(MAX),
            ref UNIQUEIDENTIFIER NOT NULL DEFAULT NEWID(),
            name_len AS LEN(name)
        )",
        "CREATE TABLE shop.orders (
            id BIGINT IDENTITY(1,1) PRIMARY KEY,
            customer_id INT NOT NULL CONSTRAINT fk_orders_customer REFERENCES shop.customers(id) ON DELETE CASCADE,
            total MONEY NOT NULL CONSTRAINT ck_orders_total CHECK (total >= 0),
            placed_at DATETIMEOFFSET,
            note NVARCHAR(MAX)
        )",
        "CREATE INDEX ix_orders_customer ON shop.orders(customer_id)",
        "CREATE TABLE shop.order_audit (id INT IDENTITY PRIMARY KEY, order_id BIGINT, at DATETIME2 DEFAULT SYSDATETIME())",
        "CREATE TRIGGER shop.trg_orders_audit ON shop.orders AFTER UPDATE AS
            INSERT INTO shop.order_audit (order_id) SELECT id FROM inserted UNION ALL SELECT id FROM inserted",
        "CREATE VIEW shop.big_orders AS SELECT id, total FROM shop.orders WHERE total > 100",
        "INSERT INTO shop.customers (name, email, balance, joined, active, avatar) VALUES
            (N'Zoë Ångström', 'zoe@example.com', 12.5, '2024-05-01 10:20:30.125', 1, 0x0102FF),
            (N'Bob', 'bob@example.com', 0, NULL, 0, NULL),
            (N'Chen 陈', NULL, 99.99, '2023-01-02', NULL, NULL)",
        "INSERT INTO shop.orders (customer_id, total, placed_at, note) VALUES
            (1, 150.25, '2024-05-02 08:00:00 +02:00', N'first'), (1, 20, NULL, NULL), (2, 300, NULL, N'big'),
            (3, 5, NULL, NULL), (3, 7.5, NULL, NULL)",
    ] {
        s.execute(Channel::Editor, sql, 10).await.unwrap_or_else(|e| panic!("{sql}: {}", e.message));
    }

    assert!(s.databases().await.unwrap().contains(&"rowglow_demo".to_string()));
    assert!(s.schemas().await.unwrap().contains(&"shop".to_string()));
    let objs = s.objects("shop").await.unwrap();
    assert!(objs.iter().any(|o| o.name == "customers" && o.kind == "table" && o.rows_estimate == Some(3)));
    assert!(objs.iter().any(|o| o.name == "big_orders" && o.kind == "view"));

    // Value rendering
    let r = s
        .execute(Channel::Editor, "SELECT id, name, email, balance, joined, active, avatar, ref, name_len FROM shop.customers ORDER BY id", 10)
        .await
        .unwrap();
    assert!(r.has_rows);
    assert_eq!(r.rows.len(), 3);
    assert_eq!(r.rows[0][1].as_deref(), Some("Zoë Ångström"));
    assert_eq!(r.rows[2][1].as_deref(), Some("Chen 陈"));
    assert_eq!(r.rows[0][3].as_deref(), Some("12.50"));
    assert_eq!(r.rows[0][4].as_deref(), Some("2024-05-01 10:20:30.125"));
    assert_eq!(r.rows[0][5].as_deref(), Some("1"));
    assert_eq!(r.rows[0][6].as_deref(), Some("0x0102FF"));
    assert_eq!(r.rows[1][4], None);
    assert_eq!(r.columns[0].type_name, "int");
    assert_eq!(r.columns[5].type_name, "bit");
    let o = s.execute(Channel::Editor, "SELECT total, placed_at FROM shop.orders WHERE id = 1", 10).await.unwrap();
    eprintln!("DEBUG {:?}", o);
    assert_eq!(o.rows[0][0].as_deref(), Some("150.25"));
    assert_eq!(o.rows[0][1].as_deref(), Some("2024-05-02 08:00:00 +02:00"));

    // Truncation stops reading early and leaves the connection usable.
    let big = s
        .execute(Channel::Editor, "SELECT TOP 200000 a.object_id FROM sys.all_objects a CROSS JOIN sys.all_objects b", 100)
        .await
        .unwrap();
    assert!(big.truncated);
    assert_eq!(big.rows.len(), 100);
    let after = s.execute(Channel::Editor, "SELECT 42 AS answer", 10).await.unwrap();
    assert_eq!(after.rows[0][0].as_deref(), Some("42"));

    // Row counts come from @@ROWCOUNT, so the trigger's 2 audit rows don't count.
    let u = s.execute(Channel::Editor, "UPDATE shop.orders SET note = N'x' WHERE id = 2", 10).await.unwrap();
    assert!(!u.has_rows);
    assert_eq!(u.rows_affected, Some(1));
    let ins = s
        .execute(Channel::Editor, "INSERT INTO shop.orders (customer_id, total) OUTPUT inserted.id VALUES (2, 1), (2, 2)", 10)
        .await
        .unwrap();
    assert!(ins.has_rows);
    assert_eq!(ins.rows.len(), 2);
    assert_eq!(ins.rows_affected, Some(2));

    // Staged edits: exactly one row each, all-or-nothing.
    let ok = s
        .transaction(
            &[
                "UPDATE [shop].[customers] SET [name] = N'Zoë' WHERE [id] = 1".into(),
                "UPDATE [shop].[orders] SET [note] = N'y' WHERE [id] = 2".into(),
            ],
            true,
        )
        .await
        .unwrap();
    assert_eq!(ok.len(), 2);
    let res = s
        .transaction(
            &[
                "UPDATE [shop].[customers] SET [name] = N'changed' WHERE [id] = 1".into(),
                "UPDATE [shop].[customers] SET [name] = N'nobody' WHERE [id] = 999".into(),
            ],
            true,
        )
        .await;
    let (idx, err) = res.unwrap_err();
    assert_eq!(idx, 1);
    assert_eq!(err.code.as_deref(), Some("row_not_found"));
    let name = s.execute(Channel::Aux, "SELECT name FROM shop.customers WHERE id = 1", 1).await.unwrap();
    assert_eq!(name.rows[0][0].as_deref(), Some("Zoë"));
    let bad = s
        .transaction(&["UPDATE shop.orders SET total = -1 WHERE id = 1".into()], false)
        .await
        .unwrap_err();
    assert_eq!(bad.1.code.as_deref(), Some("547"));

    // Errors carry the server error number
    let e = s.execute(Channel::Editor, "SELECT nope FROM shop.orders", 10).await.unwrap_err();
    assert_eq!(e.code.as_deref(), Some("207"));

    // Structure
    let st = s.structure("shop", "customers").await.unwrap();
    assert_eq!(st.primary_key, vec!["id"]);
    let id = st.columns.iter().find(|c| c.name == "id").unwrap();
    assert_eq!(id.extra.as_deref(), Some("identity(1,1)"));
    let nl = st.columns.iter().find(|c| c.name == "name_len").unwrap();
    assert!(nl.extra.as_deref().unwrap().starts_with("generated"));
    assert_eq!(st.columns.iter().find(|c| c.name == "name").unwrap().data_type, "nvarchar(100)");
    assert_eq!(st.columns.iter().find(|c| c.name == "balance").unwrap().data_type, "decimal(10,2)");
    assert!(st.constraints.iter().any(|c| c.name == "uq_customers_email" && c.kind == "unique"));
    assert!(st.referenced_by.iter().any(|f| f.ref_table == "orders" && f.ref_columns == vec!["customer_id"]));
    let ost = s.structure("shop", "orders").await.unwrap();
    assert_eq!(ost.foreign_keys.len(), 1);
    assert_eq!(ost.foreign_keys[0].on_delete, "CASCADE");
    assert!(ost.constraints.iter().any(|c| c.kind == "check" && c.definition.contains("total")));
    assert!(ost.indexes.iter().any(|i| i.name == "ix_orders_customer"));
    assert!(ost.triggers.iter().any(|t| t.name == "trg_orders_audit"));

    let ddl = s.ddl("shop", "orders", "table", None).await.unwrap();
    assert!(ddl.contains("CREATE TABLE [shop].[orders]"), "{ddl}");
    assert!(ddl.contains("IDENTITY(1,1)"), "{ddl}");
    assert!(ddl.contains("ix_orders_customer"), "{ddl}");
    let vdef = s.ddl("shop", "big_orders", "view", None).await.unwrap();
    assert!(vdef.contains("CREATE VIEW"), "{vdef}");

    let rel = s.relations("shop").await.unwrap();
    assert!(rel.foreign_keys.iter().any(|f| f.table == "orders" && f.ref_table == "customers"));
    assert!(rel.primary_keys.iter().any(|p| p.table == "customers" && p.columns == vec!["id"]));
    let comps = s.completions(None).await.unwrap();
    assert!(comps.iter().any(|t| t.schema == "shop" && t.name == "orders" && t.columns.iter().any(|c| c.0 == "note")));

    // Cancel a long query from another task; the editor connection keeps working.
    let s = std::sync::Arc::new(s);
    let s2 = s.clone();
    let h = tokio::spawn(async move { s2.execute(Channel::Editor, "WAITFOR DELAY '00:00:10'", 1).await });
    tokio::time::sleep(Duration::from_millis(500)).await;
    let started = std::time::Instant::now();
    s.cancel().await.unwrap();
    let err = h.await.unwrap().unwrap_err();
    assert_eq!(err.kind, "cancelled");
    assert!(started.elapsed() < Duration::from_secs(5));
    let again = s.execute(Channel::Editor, "SELECT 1", 1).await.unwrap();
    assert_eq!(again.rows[0][0].as_deref(), Some("1"));
}

#[tokio::test]
async fn duckdb_end_to_end() {
    if !enabled() {
        return;
    }
    let dir = std::env::temp_dir().join(format!("rowglow-duck-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("it.duckdb");
    let _ = std::fs::remove_file(&path);
    rowglow_lib::drivers::duckdb::create_file(path.to_str().unwrap()).unwrap();
    let mut c = cfg(Engine::Duckdb);
    c.host = String::new();
    c.file_path = path.to_string_lossy().into_owned();
    let s = Session::open("d".into(), &c, None, None, None).await.unwrap();
    assert_eq!(s.info.flavor, "DuckDB");
    for sql in [
        "CREATE TABLE artists (id INTEGER PRIMARY KEY, name VARCHAR NOT NULL UNIQUE)",
        "CREATE TABLE albums (id INTEGER PRIMARY KEY, artist_id INTEGER REFERENCES artists(id), title VARCHAR, price DECIMAL(6,2) DEFAULT 9.99, CHECK (price >= 0))",
        "CREATE INDEX albums_title ON albums(title)",
        "CREATE VIEW cheap AS SELECT * FROM albums WHERE price < 5",
        "INSERT INTO artists VALUES (1, 'Nina'), (2, 'Miles')",
        "INSERT INTO albums VALUES (1, 1, 'Pastel Blues', 4.5), (2, 2, 'Kind of Blue', 12)",
    ] {
        s.execute(Channel::Editor, sql, 10).await.unwrap_or_else(|e| panic!("{sql}: {}", e.message));
    }
    let objs = s.objects("main").await.unwrap();
    assert!(objs.iter().any(|o| o.name == "albums" && o.kind == "table"));
    assert!(objs.iter().any(|o| o.name == "cheap" && o.kind == "view"));
    let st = s.structure("main", "albums").await.unwrap();
    assert_eq!(st.primary_key, vec!["id"]);
    assert_eq!(st.foreign_keys.len(), 1);
    assert_eq!(st.foreign_keys[0].ref_table, "artists");
    assert_eq!(st.foreign_keys[0].ref_columns, vec!["id"]);
    assert!(st.constraints.iter().any(|c| c.kind == "check"));
    assert!(st.indexes.iter().any(|i| i.name == "albums_title"));
    let ast = s.structure("main", "artists").await.unwrap();
    assert!(ast.referenced_by.iter().any(|f| f.ref_table == "albums"));
    let ddl = s.ddl("main", "albums", "table", None).await.unwrap();
    assert!(ddl.contains("CREATE TABLE albums"), "{ddl}");
    let rel = s.relations("main").await.unwrap();
    assert!(rel.foreign_keys.iter().any(|f| f.table == "albums" && f.ref_table == "artists"));
    let comps = s.completions(None).await.unwrap();
    assert!(comps.iter().any(|t| t.name == "albums" && t.columns.iter().any(|c| c.0 == "price")));

    let ok = s
        .transaction(&["UPDATE \"main\".\"albums\" SET \"title\" = 'Kind Of Blue' WHERE \"id\" = 2".into()], true)
        .await
        .unwrap();
    assert_eq!(ok[0].rows_affected, Some(1));
    let (idx, err) = s
        .transaction(
            &[
                "UPDATE albums SET title = 'x' WHERE id = 1".into(),
                "UPDATE albums SET title = 'y' WHERE id = 99".into(),
            ],
            true,
        )
        .await
        .unwrap_err();
    assert_eq!(idx, 1);
    assert_eq!(err.code.as_deref(), Some("row_not_found"));
    let t = s.execute(Channel::Aux, "SELECT title FROM albums WHERE id = 1", 1).await.unwrap();
    assert_eq!(t.rows[0][0].as_deref(), Some("Pastel Blues"));

    // Cancel
    let s = std::sync::Arc::new(s);
    let s2 = s.clone();
    let h = tokio::spawn(async move {
        s2.execute(Channel::Editor, "SELECT count(*) FROM range(100000000000) a WHERE a.range % 7 = 3", 1).await
    });
    tokio::time::sleep(Duration::from_millis(300)).await;
    s.cancel().await.unwrap();
    let err = h.await.unwrap().unwrap_err();
    assert_eq!(err.kind, "cancelled", "{}", err.message);
    drop(s);

    // Read-only connections can't write, enforced by DuckDB itself.
    c.read_only = true;
    let ro = Session::open("d2".into(), &c, None, None, None).await.unwrap();
    let e = ro.execute(Channel::Editor, "DELETE FROM albums", 1).await.unwrap_err();
    assert!(e.message.to_lowercase().contains("read-only") || e.message.to_lowercase().contains("read only"), "{}", e.message);
    drop(ro);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn ssh_tunnel_to_sql_server() {
    if !enabled() {
        return;
    }
    let (Some(pw), Some(ssh_user), Some(ssh_pw)) = (test_env("MSSQL_SA_PASSWORD"), test_env("SSH_USER"), test_env("SSH_PASSWORD")) else {
        eprintln!("SSH test server not configured; skipping");
        return;
    };
    rowglow_lib::init_crypto();
    let mut c = cfg(Engine::Mssql);
    // Seen from the SSH server, SQL Server is the container "rg-mssql" on the Docker network.
    c.host = "rg-mssql".into();
    c.username = "sa".into();
    c.database = "master".into();
    c.ssh_enabled = true;
    c.ssh_host = "127.0.0.1".into();
    c.ssh_port = 2222;
    c.ssh_user = ssh_user.clone();
    c.ssh_auth = "password".into();
    let params = |known: Option<String>, secret: &str| rowglow_lib::ssh::SshParams {
        host: "127.0.0.1".into(),
        port: 2222,
        user: ssh_user.clone(),
        auth: rowglow_lib::ssh::SshAuth::Password(secret.into()),
        known_fingerprint: known,
    };

    // 1. Unknown host: refused, with the fingerprint to show the user.
    let err = match Session::open("t".into(), &c, Some(&pw), None, Some(params(None, &ssh_pw))).await {
        Ok(_) => panic!("unknown host must be refused"),
        Err(e) => e,
    };
    assert_eq!(err.code.as_deref(), Some("ssh_unknown_host"), "{}", err.message);
    let fp = err.detail.clone().unwrap();
    assert!(fp.starts_with("SHA256:"), "{fp}");

    // 2. A different trusted key: refused as a possible MITM.
    let err = match Session::open("t".into(), &c, Some(&pw), None, Some(params(Some("SHA256:not-the-key".into()), &ssh_pw))).await {
        Ok(_) => panic!("changed host key must be refused"),
        Err(e) => e,
    };
    assert_eq!(err.code.as_deref(), Some("ssh_host_key_changed"));

    // 3. Wrong password.
    let err = match Session::open("t".into(), &c, Some(&pw), None, Some(params(Some(fp.clone()), "wrong"))).await {
        Ok(_) => panic!("bad password must fail"),
        Err(e) => e,
    };
    assert_eq!(err.code.as_deref(), Some("ssh_auth_failed"), "{}", err.message);

    // 4. Trusted: queries run through the tunnel.
    let s = Session::open("t".into(), &c, Some(&pw), None, Some(params(Some(fp), &ssh_pw))).await.expect("tunnelled session");
    let r = s.execute(Channel::Editor, "SELECT @@SERVERNAME AS name, DB_NAME() AS db", 10).await.unwrap();
    assert_eq!(r.rows[0][1].as_deref(), Some("master"));
    let dbs = s.databases().await.unwrap();
    assert!(dbs.contains(&"master".to_string()));
    // Several queries in parallel on both connections share the one SSH session.
    let (a, b) = tokio::join!(
        s.execute(Channel::Editor, "SELECT COUNT(*) FROM sys.objects", 10),
        s.execute(Channel::Aux, "SELECT COUNT(*) FROM sys.columns", 10)
    );
    assert!(a.is_ok() && b.is_ok());
}

/// Key-based SSH login. Needs RG_SSH_KEY_DIR with rg_ed25519 (passphrase
/// "rg-pass-123") and rg_rsa (no passphrase) authorised on the test server.
#[tokio::test]
async fn ssh_tunnel_with_keys() {
    if !enabled() {
        return;
    }
    let (Some(pw), Some(ssh_user), Ok(dir)) = (test_env("MSSQL_SA_PASSWORD"), test_env("SSH_USER"), std::env::var("RG_SSH_KEY_DIR")) else {
        eprintln!("SSH key test not configured; skipping");
        return;
    };
    rowglow_lib::init_crypto();
    let mut c = cfg(Engine::Mssql);
    c.host = "rg-mssql".into();
    c.username = "sa".into();
    c.ssh_enabled = true;
    let probe = rowglow_lib::ssh::SshParams {
        host: "127.0.0.1".into(),
        port: 2222,
        user: ssh_user.clone(),
        auth: rowglow_lib::ssh::SshAuth::Password(String::new()),
        known_fingerprint: None,
    };
    let fp = match rowglow_lib::ssh::open_tunnel(&probe, "rg-mssql", 1433).await {
        Err(e) => e.detail.unwrap(),
        Ok(_) => panic!("expected unknown host"),
    };
    let key = |file: &str, passphrase: Option<&str>| rowglow_lib::ssh::SshParams {
        host: "127.0.0.1".into(),
        port: 2222,
        user: ssh_user.clone(),
        auth: rowglow_lib::ssh::SshAuth::Key {
            path: format!("{dir}/{file}"),
            passphrase: passphrase.map(str::to_string),
        },
        known_fingerprint: Some(fp.clone()),
    };
    for p in [key("rg_ed25519", Some("rg-pass-123")), key("rg_rsa", None)] {
        let s = Session::open("k".into(), &c, Some(&pw), None, Some(p)).await.expect("key login");
        let r = s.execute(Channel::Editor, "SELECT 1", 1).await.unwrap();
        assert_eq!(r.rows[0][0].as_deref(), Some("1"));
    }
    let err = match Session::open("k".into(), &c, Some(&pw), None, Some(key("rg_ed25519", Some("nope")))).await {
        Ok(_) => panic!("wrong passphrase must fail"),
        Err(e) => e,
    };
    assert!(err.message.contains("Couldn't read the SSH key"), "{}", err.message);
}

async fn import_roundtrip(s: &Session, schema: &str, label: &str) {
    use rowglow_lib::import::{preview, run, sql_type, ImportColumn, ImportRequest};
    let engine = s.engine();
    let dir = std::env::temp_dir().join(format!("rg-import-{}-{label}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let good = dir.join("people.csv");
    let mut csv = String::from("id,name,score,active,born\n");
    for i in 1..=1203 {
        csv.push_str(&format!("{i},\"Person, {i} Zoë\",{}.5,{},2001-02-03\n", i % 7, if i % 2 == 0 { "true" } else { "false" }));
    }
    csv.push_str("1204,,,,\n");
    std::fs::write(&good, &csv).unwrap();
    let p = preview(good.to_str().unwrap(), None, None).unwrap();
    assert_eq!(p.total_rows, 1204);
    assert!(p.has_header);
    let table = "rg_import_test";
    let q = rowglow_lib::util::qualified(engine, schema, table);
    let _ = s.execute(Channel::Editor, &format!("DROP TABLE {q}"), 1).await;
    let columns: Vec<ImportColumn> = p
        .columns
        .iter()
        .enumerate()
        .map(|(i, c)| ImportColumn { source: i, target: c.name.clone(), sql_type: sql_type(engine, &c.kind) })
        .collect();
    let req = |job: &str, path: &std::path::Path, create: bool, cols: Vec<ImportColumn>| ImportRequest {
        session_id: s.id.clone(),
        job_id: job.into(),
        path: path.to_string_lossy().into_owned(),
        delimiter: ",".into(),
        has_header: true,
        encoding: "utf-8".into(),
        schema: schema.into(),
        table: table.into(),
        create,
        columns: cols,
        empty_as_null: true,
    };
    // Free plan limit is checked before anything happens.
    let e = run(s, req("j0", &good, true, columns.clone()), Some(1000)).await.unwrap_err();
    assert_eq!(e.code.as_deref(), Some("limit_import"), "{label}");
    let n = run(s, req("j1", &good, true, columns.clone()), None).await.unwrap_or_else(|e| panic!("{label}: {}", e.message));
    assert_eq!(n, 1204, "{label}");
    let r = s.execute(Channel::Aux, &format!("SELECT count(*) FROM {q}"), 1).await.unwrap();
    assert_eq!(r.rows[0][0].as_deref(), Some("1204"), "{label}");
    let r = s
        .execute(Channel::Aux, &format!("SELECT name FROM {q} WHERE id = 2"), 1)
        .await
        .unwrap();
    assert_eq!(r.rows[0][0].as_deref(), Some("Person, 2 Zoë"), "{label}");
    let r = s.execute(Channel::Aux, &format!("SELECT name FROM {q} WHERE id = 1204"), 1).await.unwrap();
    assert_eq!(r.rows[0][0], None, "{label}: empty cell becomes NULL");

    // A bad value in the middle rolls back the whole import.
    let bad = dir.join("bad.csv");
    std::fs::write(&bad, "id,name\n5000,ok\n5001,ok\nnot-a-number,boom\n").unwrap();
    let cols2 = vec![
        ImportColumn { source: 0, target: "id".into(), sql_type: String::new() },
        ImportColumn { source: 1, target: "name".into(), sql_type: String::new() },
    ];
    // SQLite stores 'not-a-number' happily in an INTEGER column; force a failure there with a missing column instead.
    let cols_bad = if engine == Engine::Sqlite {
        vec![ImportColumn { source: 0, target: "nope".into(), sql_type: String::new() }]
    } else {
        cols2.clone()
    };
    let e = run(s, req("j2", &bad, false, cols_bad), None).await.unwrap_err();
    assert!(e.message.contains("Nothing was imported"), "{label}: {}", e.message);
    let r = s.execute(Channel::Aux, &format!("SELECT count(*) FROM {q}"), 1).await.unwrap();
    assert_eq!(r.rows[0][0].as_deref(), Some("1204"), "{label}: rolled back");

    // Creating a table that then fails leaves nothing behind (MySQL commits DDL implicitly).
    let e = run(
        s,
        ImportRequest { table: "rg_import_fail".into(), ..req("j3", &bad, true, vec![
            ImportColumn { source: 0, target: "id".into(), sql_type: sql_type(engine, "integer") },
            ImportColumn { source: 1, target: "name".into(), sql_type: sql_type(engine, "text") },
        ]) },
        None,
    )
    .await;
    if engine != Engine::Sqlite {
        assert!(e.is_err(), "{label}");
        let objs = s.objects(schema).await.unwrap();
        assert!(!objs.iter().any(|o| o.name == "rg_import_fail"), "{label}: failed create must not leave a table");
    } else {
        let _ = s.execute(Channel::Editor, &format!("DROP TABLE {}", rowglow_lib::util::qualified(engine, schema, "rg_import_fail")), 1).await;
    }
    s.execute(Channel::Editor, &format!("DROP TABLE {q}"), 1).await.unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn csv_import_all_engines() {
    if !enabled() {
        return;
    }
    let pg = open(Engine::Postgres).await;
    import_roundtrip(&pg, "shop", "postgres").await;
    let my = open(Engine::Mysql).await;
    import_roundtrip(&my, "rowglow_demo", "mysql").await;
    if let Some(ms) = open_mssql("rowglow_demo").await {
        import_roundtrip(&ms, "dbo", "mssql").await;
    }
    // File engines on throwaway copies
    let dir = std::env::temp_dir().join(format!("rg-import-files-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let lite = dir.join("t.sqlite");
    rowglow_lib::drivers::sqlite::create_file(lite.to_str().unwrap()).unwrap();
    let mut c = cfg(Engine::Sqlite);
    c.file_path = lite.to_string_lossy().into_owned();
    let s = Session::open("l".into(), &c, None, None, None).await.unwrap();
    import_roundtrip(&s, "main", "sqlite").await;
    let duck = dir.join("t.duckdb");
    rowglow_lib::drivers::duckdb::create_file(duck.to_str().unwrap()).unwrap();
    let mut c = cfg(Engine::Duckdb);
    c.host = String::new();
    c.file_path = duck.to_string_lossy().into_owned();
    let s = Session::open("d".into(), &c, None, None, None).await.unwrap();
    import_roundtrip(&s, "main", "duckdb").await;
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn redis_end_to_end() {
    if !enabled() {
        return;
    }
    let Some(pw) = test_env("REDIS_PASSWORD") else {
        eprintln!("Redis test server not configured; skipping");
        return;
    };
    use rowglow_lib::drivers::redis::RedisOp;
    use serde_json::json;
    rowglow_lib::init_crypto();
    let mut c = cfg(Engine::Redis);
    c.database = "3".into();
    let s = Session::open("r".into(), &c, Some(&pw), None, None).await.expect("redis session");
    assert!(s.info.flavor == "Redis" || s.info.flavor == "Valkey", "{}", s.info.flavor);
    assert_eq!(s.info.database, "3");
    let r = s.redis().await.unwrap();
    // clean slate in db 3 (our test server only)
    r.command("FLUSHDB", true).await.unwrap();
    let flush = r.command("FLUSHDB", false).await.unwrap_err();
    assert_eq!(flush.code.as_deref(), Some("confirm_required"));
    assert_eq!(r.command("SELECT 1", true).await.unwrap_err().code.as_deref(), Some("refused"));

    let op = |v: serde_json::Value| serde_json::from_value::<RedisOp>(v).unwrap();
    r.op(op(json!({"op": "create", "key": "user:1", "kind": "hash", "field": "name", "value": "Zoë"}))).await.unwrap();
    r.op(op(json!({"op": "create", "key": "greeting", "kind": "string", "value": "hello", "ttl": 3600}))).await.unwrap();
    r.op(op(json!({"op": "create", "key": "queue", "kind": "list", "value": "a"}))).await.unwrap();
    r.op(op(json!({"op": "create", "key": "tags", "kind": "set", "value": "x"}))).await.unwrap();
    r.op(op(json!({"op": "create", "key": "board", "kind": "zset", "value": "ann", "score": 10.5}))).await.unwrap();
    assert!(r.op(op(json!({"op": "create", "key": "greeting", "kind": "string", "value": "again"}))).await.is_err(), "create never overwrites");
    r.command("XADD events * kind signup user 1", false).await.unwrap();
    r.command("SET bin \"\\xff\\xfe\"", false).await.unwrap();

    let page = r.scan("0", "*", 500, None).await.unwrap();
    let keys = page["keys"].as_array().unwrap();
    assert_eq!(keys.len(), 7, "{page}");
    let greeting = keys.iter().find(|k| k["text"] == "greeting").unwrap();
    assert_eq!(greeting["type"], "string");
    assert!(greeting["ttl"].as_i64().unwrap() > 3_000_000);
    let only_hashes = r.scan("0", "user:*", 500, Some("hash")).await.unwrap();
    assert_eq!(only_hashes["keys"].as_array().unwrap().len(), 1);

    // string edit keeps TTL
    r.op(op(json!({"op": "setString", "key": "greeting", "value": "héllo"}))).await.unwrap();
    let g = r.get("greeting", &None).await.unwrap();
    assert_eq!(g["value"]["text"], "héllo");
    assert!(g["ttl"].as_i64().unwrap() > 0, "KEEPTTL");
    assert!(r.op(op(json!({"op": "setString", "key": "nope", "value": "x"}))).await.is_err(), "XX never creates");

    // hash: rename a field atomically
    r.op(op(json!({"op": "hashSet", "key": "user:1", "field": "email", "value": "z@example.com"}))).await.unwrap();
    r.op(op(json!({"op": "hashSet", "key": "user:1", "field": "full_name", "value": "Zoë A.", "oldField": "name"}))).await.unwrap();
    let h = r.get("user:1", &None).await.unwrap();
    let fields: Vec<&str> = h["items"].as_array().unwrap().iter().map(|i| i["field"]["text"].as_str().unwrap()).collect();
    assert!(fields.contains(&"full_name") && !fields.contains(&"name") && fields.contains(&"email"), "{fields:?}");

    // list: set, push, guarded remove
    r.op(op(json!({"op": "listPush", "key": "queue", "value": "b", "head": false}))).await.unwrap();
    r.op(op(json!({"op": "listPush", "key": "queue", "value": "a", "head": false}))).await.unwrap();
    r.op(op(json!({"op": "listSet", "key": "queue", "index": 1, "value": "B", "expected": "b"}))).await.unwrap();
    assert!(r.op(op(json!({"op": "listRemove", "key": "queue", "index": 0, "expected": "zzz"}))).await.is_err(), "stale list edit refused");
    r.op(op(json!({"op": "listRemove", "key": "queue", "index": 2, "expected": "a"}))).await.unwrap();
    let l = r.get("queue", &None).await.unwrap();
    let vals: Vec<&str> = l["items"].as_array().unwrap().iter().map(|i| i["value"]["text"].as_str().unwrap()).collect();
    assert_eq!(vals, ["a", "B"], "removes exactly the chosen duplicate");

    r.op(op(json!({"op": "setAdd", "key": "tags", "members": ["y", "z"]}))).await.unwrap();
    r.op(op(json!({"op": "setRemove", "key": "tags", "members": ["x"]}))).await.unwrap();
    assert_eq!(r.get("tags", &None).await.unwrap()["length"], 2);
    r.op(op(json!({"op": "zsetAdd", "key": "board", "member": "bob", "score": 3}))).await.unwrap();
    let z = r.get("board", &None).await.unwrap();
    assert_eq!(z["items"][0]["member"]["text"], "bob");
    let st = r.get("events", &None).await.unwrap();
    assert_eq!(st["type"], "stream");
    assert_eq!(st["items"][0]["fields"][0][0]["text"], "kind");
    let b = r.get("bin", &None).await.unwrap();
    assert!(b["value"]["b64"].is_string(), "binary values come back as base64");

    // expire / persist / rename / delete
    r.op(op(json!({"op": "expire", "key": "tags", "ttl": 100}))).await.unwrap();
    assert!(r.get("tags", &None).await.unwrap()["ttl"].as_i64().unwrap() > 0);
    r.op(op(json!({"op": "expire", "key": "tags", "ttl": null}))).await.unwrap();
    assert_eq!(r.get("tags", &None).await.unwrap()["ttl"], -1);
    assert!(r.op(op(json!({"op": "rename", "key": "tags", "newKey": "board"}))).await.is_err(), "rename never overwrites");
    r.op(op(json!({"op": "rename", "key": "tags", "newKey": "labels"}))).await.unwrap();
    r.op(op(json!({"op": "delete", "keys": ["labels", "greeting"]}))).await.unwrap();
    assert_eq!(r.get("greeting", &None).await.unwrap_err().code.as_deref(), Some("key_missing"));

    // console replies
    let out = r.command("HGETALL user:1", false).await.unwrap();
    assert!(matches!(out["reply"]["t"].as_str(), Some("array") | Some("map")), "{out}");
    let err = r.command("INCR user:1", false).await.unwrap();
    assert_eq!(err["reply"]["t"], "error");
    assert!(s.databases().await.unwrap().len() >= 16);

    // read-only connection
    let mut ro = c.clone();
    ro.read_only = true;
    let rs = Session::open("ro".into(), &ro, Some(&pw), None, None).await.unwrap();
    let rr = rs.redis().await.unwrap();
    assert!(rr.command("GET user:1", false).await.is_ok());
    assert_eq!(rr.command("DEL user:1", false).await.unwrap_err().code.as_deref(), Some("read_only"));
    assert_eq!(rr.command("CONFIG SET maxmemory 1", true).await.unwrap_err().code.as_deref(), Some("read_only"));
    assert!(rr.op(op(json!({"op": "delete", "keys": ["user:1"]}))).await.is_err());

    // wrong password is a connection error
    let e = match Session::open("bad".into(), &c, Some("wrong"), None, None).await {
        Ok(_) => panic!("bad password must fail"),
        Err(e) => e,
    };
    assert_eq!(e.kind, "connection", "{}", e.message);

    // through the SSH tunnel to the container
    if let (Some(ssh_user), Some(ssh_pw)) = (test_env("SSH_USER"), test_env("SSH_PASSWORD")) {
        let probe = rowglow_lib::ssh::SshParams {
            host: "127.0.0.1".into(),
            port: 2222,
            user: ssh_user.clone(),
            auth: rowglow_lib::ssh::SshAuth::Password(ssh_pw.clone()),
            known_fingerprint: None,
        };
        let fp = match rowglow_lib::ssh::open_tunnel(&probe, "rg-redis", 6379).await {
            Err(e) => e.detail.unwrap(),
            Ok(_) => panic!("expected unknown host"),
        };
        let mut t = c.clone();
        t.host = "rg-redis".into();
        t.ssh_enabled = true;
        let ts = Session::open("t".into(), &t, Some(&pw), None, Some(rowglow_lib::ssh::SshParams { known_fingerprint: Some(fp), ..probe }))
            .await
            .expect("redis via ssh");
        let v = ts.redis().await.unwrap().get("user:1", &None).await.unwrap();
        assert_eq!(v["type"], "hash");
    }
    r.command("FLUSHDB", true).await.unwrap();
}

#[tokio::test]
async fn mongodb_end_to_end() {
    if !enabled() {
        return;
    }
    let (Some(user), Some(pw)) = (test_env("MONGO_USER"), test_env("MONGO_PASSWORD")) else {
        eprintln!("MongoDB test server not configured; skipping");
        return;
    };
    rowglow_lib::init_crypto();
    let mut c = cfg(Engine::Mongodb);
    c.username = user.clone();
    c.database = "rowglow_it".into();
    let s = Session::open("m".into(), &c, Some(&pw), None, None).await.expect("mongo session");
    assert!(s.info.flavor.starts_with("MongoDB"), "{}", s.info.flavor);
    assert_eq!(s.info.database, "rowglow_it");
    let m = s.mongo().await.unwrap();
    let db = "rowglow_it";
    let _ = m.command(db, r#"{"dropDatabase": 1}"#, true).await;
    assert_eq!(m.command(db, r#"{"dropDatabase": 1}"#, false).await.unwrap_err().code.as_deref(), Some("confirm_required"));
    m.create_collection(db, "people").await.unwrap();
    let docs: Vec<String> = (1..=30)
        .map(|i| {
            format!(
                r#"{{"name": "P{i:02}", "age": {}, "tags": ["t{}"], "joined": {{"$date": "2024-0{}-01T00:00:00Z"}}, "balance": {{"$numberDecimal": "{i}.50"}}}}"#,
                20 + i,
                i % 3,
                1 + i % 9
            )
        })
        .collect();
    let ins = m.insert(db, "people", &docs).await.unwrap();
    assert_eq!(ins["inserted"], 30);
    let first_id = ins["ids"][0].to_string();
    assert!(first_id.contains("$oid"), "{first_id}");

    let cols = m.collections(db).await.unwrap();
    assert_eq!(cols[0]["name"], "people");
    assert_eq!(cols[0]["count"], 30);

    let page = m.find(db, "people", r#"{"age": {"$gte": 40}}"#, r#"{"age": -1}"#, r#"{"name": 1, "age": 1}"#, 2, 5).await.unwrap();
    let got = page["docs"].as_array().unwrap();
    assert_eq!(got.len(), 5);
    assert_eq!(page["total"], 11);
    assert_eq!(got[0]["age"], 48, "sorted desc, skipped 2");
    assert!(got[0].get("tags").is_none(), "projection applied");
    assert!(m.find(db, "people", "[1]", "", "", 0, 5).await.is_err());

    // replace keeps _id; changing _id is refused; deleted docs are reported
    let full = m.find(db, "people", &format!(r#"{{"_id": {first_id}}}"#), "", "", 0, 1).await.unwrap();
    let mut d = full["docs"][0].clone();
    d["name"] = serde_json::json!("Zoë");
    m.replace(db, "people", &first_id, &d.to_string()).await.unwrap();
    let again = m.find(db, "people", &format!(r#"{{"_id": {first_id}}}"#), "", "", 0, 1).await.unwrap();
    assert_eq!(again["docs"][0]["name"], "Zoë");
    assert_eq!(again["docs"][0]["balance"]["$numberDecimal"], "1.50", "decimals round-trip");
    assert!(again["docs"][0]["joined"]["$date"].is_string(), "dates round-trip");
    let mut moved = d.clone();
    moved["_id"] = serde_json::json!({"$oid": "000000000000000000000000"});
    assert!(m.replace(db, "people", &first_id, &moved.to_string()).await.is_err());
    let del = m.delete(db, "people", &[first_id.clone()]).await.unwrap();
    assert_eq!(del["deleted"], 1);
    assert_eq!(m.replace(db, "people", &first_id, &d.to_string()).await.unwrap_err().code.as_deref(), Some("row_not_found"));

    let agg = m
        .aggregate(db, "people", r#"[{"$group": {"_id": {"$mod": ["$age", 3]}, "n": {"$sum": 1}}}, {"$sort": {"_id": 1}}]"#)
        .await
        .unwrap();
    assert_eq!(agg["docs"].as_array().unwrap().len(), 3);
    m.command(db, r#"{"createIndexes": "people", "indexes": [{"key": {"age": 1}, "name": "age_1"}]}"#, false).await.unwrap();
    let idx = m.indexes(db, "people").await.unwrap();
    assert!(idx.as_array().unwrap().iter().any(|i| i["name"] == "age_1"));
    let stats = m.command(db, r#"{"dbStats": 1}"#, false).await.unwrap();
    assert_eq!(stats["result"]["db"], db);
    assert!(s.databases().await.unwrap().contains(&db.to_string()));

    // read-only
    let mut ro = c.clone();
    ro.read_only = true;
    let rs = Session::open("mr".into(), &ro, Some(&pw), None, None).await.unwrap();
    let rm = rs.mongo().await.unwrap();
    assert!(rm.find(db, "people", "{}", "", "", 0, 1).await.is_ok());
    assert_eq!(rm.insert(db, "people", &["{}".into()]).await.unwrap_err().code.as_deref(), Some("read_only"));
    assert_eq!(rm.aggregate(db, "people", r#"[{"$out": "copy"}]"#).await.unwrap_err().code.as_deref(), Some("read_only"));
    assert_eq!(rm.command(db, r#"{"drop": "people"}"#, true).await.unwrap_err().code.as_deref(), Some("read_only"));
    assert!(rm.command(db, r#"{"dbStats": 1}"#, false).await.is_ok());

    // wrong password
    let e = match Session::open("bad".into(), &c, Some("wrong"), None, None).await {
        Ok(_) => panic!("bad password must fail"),
        Err(e) => e,
    };
    assert_eq!(e.kind, "connection", "{}", e.message);

    // through SSH
    if let (Some(ssh_user), Some(ssh_pw)) = (test_env("SSH_USER"), test_env("SSH_PASSWORD")) {
        let probe = rowglow_lib::ssh::SshParams {
            host: "127.0.0.1".into(),
            port: 2222,
            user: ssh_user,
            auth: rowglow_lib::ssh::SshAuth::Password(ssh_pw),
            known_fingerprint: None,
        };
        let fp = match rowglow_lib::ssh::open_tunnel(&probe, "rg-mongo", 27017).await {
            Err(e) => e.detail.unwrap(),
            Ok(_) => panic!("expected unknown host"),
        };
        let mut t = c.clone();
        t.host = "rg-mongo".into();
        t.ssh_enabled = true;
        let ts = Session::open("t".into(), &t, Some(&pw), None, Some(rowglow_lib::ssh::SshParams { known_fingerprint: Some(fp), ..probe }))
            .await
            .expect("mongo via ssh");
        let n = ts.mongo().await.unwrap().find(db, "people", "{}", "", "", 0, 1).await.unwrap();
        assert_eq!(n["total"], 29);
    }
    m.command(db, r#"{"dropDatabase": 1}"#, true).await.unwrap();
}

// ------------------------------------------------------------------- 1.2

#[tokio::test]
async fn redis_cluster_end_to_end() {
    if !enabled() {
        return;
    }
    let Some(pw) = test_env("REDIS_PASSWORD") else {
        eprintln!("Redis test servers not configured; skipping");
        return;
    };
    use rowglow_lib::drivers::redis::RedisOp;
    use serde_json::json;
    use std::collections::HashSet;
    rowglow_lib::init_crypto();
    let mut c = cfg(Engine::Redis);
    c.port = 7001;
    c.database = String::new();
    c.options.redis_mode = "cluster".into();
    c.options.redis_nodes = "127.0.0.1:7002, 127.0.0.1:7003".into();
    let s = Session::open("rc".into(), &c, Some(&pw), None, None).await.expect("cluster session");
    let r = s.redis().await.unwrap();
    assert!(r.is_cluster());
    assert_eq!(r.databases().await.unwrap(), vec!["0".to_string()]);
    // Our throwaway cluster only.
    r.command("FLUSHALL", true).await.unwrap();

    for i in 0..300 {
        r.command(&format!("SET it:key:{i} v{i}"), false).await.unwrap();
    }
    let op = |v: serde_json::Value| serde_json::from_value::<RedisOp>(v).unwrap();
    r.op(op(json!({"op": "create", "key": "it:{user}:1", "kind": "hash", "field": "name", "value": "Zoë"}))).await.unwrap();

    // Every key, across the three primaries, page by page.
    let mut seen = HashSet::new();
    let mut cursor = "0".to_string();
    let mut pages = 0;
    loop {
        let page = r.scan(&cursor, "it:*", 40, None).await.unwrap();
        for k in page["keys"].as_array().unwrap() {
            assert!(k["type"] == "string" || k["type"] == "hash", "{k}");
            seen.insert(k["text"].as_str().unwrap().to_string());
        }
        cursor = page["cursor"].as_str().unwrap().to_string();
        pages += 1;
        if cursor == "0" {
            break;
        }
        assert!(pages < 200, "scan never finished");
    }
    assert_eq!(seen.len(), 301, "pages: {pages}");
    let hashes = r.scan("0", "it:*", 5000, Some("hash")).await.unwrap();
    // The hash lives on one node; walk until it shows up.
    let mut found = hashes["keys"].as_array().unwrap().len();
    let mut cur = hashes["cursor"].as_str().unwrap().to_string();
    while cur != "0" {
        let p = r.scan(&cur, "it:*", 5000, Some("hash")).await.unwrap();
        found += p["keys"].as_array().unwrap().len();
        cur = p["cursor"].as_str().unwrap().to_string();
    }
    assert_eq!(found, 1);

    let o = r.overview().await.unwrap();
    assert_eq!(o["cluster"]["state"], "ok", "{o}");
    let nodes = o["cluster"]["nodes"].as_array().unwrap();
    assert_eq!(nodes.len(), 6, "{o}");
    assert_eq!(nodes.iter().filter(|n| n["role"] == "primary").count(), 3);
    assert_eq!(o["dbsize"], 301);

    // Console: routed by slot, multi-node commands aggregated.
    let dbsize = r.command("DBSIZE", false).await.unwrap();
    assert_eq!(dbsize["reply"]["v"], 301, "{dbsize}");
    let get = r.command("GET it:key:42", false).await.unwrap();
    assert_eq!(get["reply"]["text"], "v42");
    let mget = r.command("MGET it:key:1 it:key:2 it:key:3", false).await.unwrap();
    assert_eq!(mget["reply"]["v"].as_array().unwrap().len(), 3, "{mget}");
    assert_eq!(r.command("KEYS *", false).await.unwrap_err().code.as_deref(), Some("confirm_required"));

    // Typed edits and the same-slot rule for renames.
    let k = |s: &str| json!(s);
    r.op(op(json!({"op": "hashSet", "key": "it:{user}:1", "field": "city", "value": "Oslo"}))).await.unwrap();
    let h = r.get("it:{user}:1", &None).await.unwrap();
    assert_eq!(h["type"], "hash");
    r.op(op(json!({"op": "rename", "key": "it:{user}:1", "newKey": "it:{user}:2"}))).await.unwrap();
    let cross = r.op(op(json!({"op": "rename", "key": "it:key:1", "newKey": "it:other:1"}))).await;
    assert!(cross.is_err(), "renaming across slots must fail");
    let gone = r.op(op(json!({"op": "delete", "keys": [k("it:key:1"), k("it:key:2"), k("it:key:3"), k("it:key:200")]}))).await.unwrap();
    assert_eq!(gone["changed"], 4);

    // A cluster only has database 0.
    let mut bad = c.clone();
    bad.database = "2".into();
    assert!(Session::open("rc2".into(), &bad, Some(&pw), None, None).await.is_err());
    r.command("FLUSHALL", true).await.unwrap();
}

#[tokio::test]
async fn redis_sentinel_end_to_end() {
    if !enabled() {
        return;
    }
    let (Some(pw), Some(spw)) = (test_env("REDIS_PASSWORD"), test_env("SENTINEL_PASSWORD")) else {
        eprintln!("Redis test servers not configured; skipping");
        return;
    };
    rowglow_lib::init_crypto();
    let mut c = cfg(Engine::Redis);
    c.port = 26379;
    c.database = "2".into();
    c.options.redis_mode = "sentinel".into();
    c.options.sentinel_master = "mymaster".into();
    c.sentinel_password = Some(spw.clone());
    let s = Session::open("rs".into(), &c, Some(&pw), None, None).await.expect("sentinel session");
    assert!(!s.info.read_only);
    let r = s.redis().await.unwrap();
    r.command("SET it:sentinel yes", false).await.unwrap();
    let role = r.command("ROLE", false).await.unwrap();
    assert_eq!(role["reply"]["v"][0]["text"], "master", "{role}");

    // A replica connection is read-only (and really is a replica).
    let mut rc = c.clone();
    rc.options.redis_replica = true;
    let rs = Session::open("rs2".into(), &rc, Some(&pw), None, None).await.expect("replica session");
    assert!(rs.info.read_only);
    let rr = rs.redis().await.unwrap();
    let role = rr.command("ROLE", false).await.unwrap();
    assert_eq!(role["reply"]["v"][0]["text"], "slave", "{role}");
    assert_eq!(rr.command("SET x 1", false).await.unwrap_err().code.as_deref(), Some("read_only"));
    // Replication is asynchronous; give it a moment.
    let mut seen = false;
    for _ in 0..20 {
        if rr.command("GET it:sentinel", false).await.unwrap()["reply"]["text"] == "yes" {
            seen = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(seen, "replica never got the key");

    // Wrong master name / missing Sentinel password give clear errors.
    let mut wrong = c.clone();
    wrong.options.sentinel_master = "nope".into();
    let e = Session::open("rs3".into(), &wrong, Some(&pw), None, None).await.err().expect("unknown master");
    assert!(e.message.contains("nope"), "{}", e.message);
    let mut nopw = c.clone();
    nopw.sentinel_password = None;
    assert!(Session::open("rs4".into(), &nopw, Some(&pw), None, None).await.is_err());
    r.command("DEL it:sentinel", false).await.unwrap();
}

/// Serves one canned HTTP response; returns its URL and the raw request.
async fn one_shot_http(response: String) -> (String, tokio::task::JoinHandle<String>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let h = tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.unwrap();
        let mut req = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            let n = sock.read(&mut buf).await.unwrap();
            req.extend_from_slice(&buf[..n]);
            if let Some(h) = req.windows(4).position(|w| w == b"\r\n\r\n") {
                let head = String::from_utf8_lossy(&req[..h]).to_lowercase();
                let len: usize = head
                    .lines()
                    .find_map(|l| l.strip_prefix("content-length:"))
                    .and_then(|v| v.trim().parse().ok())
                    .unwrap_or(0);
                while req.len() < h + 4 + len {
                    let n = sock.read(&mut buf).await.unwrap();
                    req.extend_from_slice(&buf[..n]);
                }
                break;
            }
        }
        sock.write_all(response.as_bytes()).await.unwrap();
        sock.shutdown().await.ok();
        String::from_utf8_lossy(&req).into_owned()
    });
    (format!("http://127.0.0.1:{port}"), h)
}

#[tokio::test]
async fn entra_sign_in_paths() {
    if !enabled() {
        return;
    }
    rowglow_lib::init_crypto();
    // 1. Service principal: the token request goes to Entra as a form POST.
    let body = r#"{"token_type":"Bearer","expires_in":3599,"access_token":"eyJ.fake.token"}"#;
    let (url, req) = one_shot_http(format!(
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    ))
    .await;
    let t = rowglow_lib::entra::service_principal(&url, "contoso.onmicrosoft.com", "app-id", "s3cr3t").await.unwrap();
    assert_eq!(t.access_token, "eyJ.fake.token");
    assert!(t.fresh());
    let req = req.await.unwrap();
    assert!(req.starts_with("POST /contoso.onmicrosoft.com/oauth2/v2.0/token "), "{req}");
    assert!(req.contains("grant_type=client_credentials"));
    assert!(req.contains("scope=https%3A%2F%2Fdatabase.windows.net%2F.default"));
    assert!(req.contains("client_id=app-id"));

    // Refusals come back readable.
    let err = r#"{"error":"invalid_client","error_description":"AADSTS7000215: Invalid client secret provided.\r\nTrace ID: x"}"#;
    let (url, _) = one_shot_http(format!(
        "HTTP/1.1 401 Unauthorized\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{err}",
        err.len()
    ))
    .await;
    let e = rowglow_lib::entra::service_principal(&url, "contoso.onmicrosoft.com", "app-id", "bad").await.unwrap_err();
    assert!(e.message.contains("AADSTS7000215"), "{}", e.message);
    assert!(!e.message.contains("Trace ID"));

    // 2. Azure CLI: a stand-in `az` that prints what the real one does.
    let dir = std::env::temp_dir().join(format!("rg-az-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let ok_az = dir.join("az-ok");
    std::fs::write(
        &ok_az,
        "#!/bin/sh\n[ \"$1 $2 $3 $4\" = \"account get-access-token --resource https://database.windows.net/\" ] || exit 9\necho '{\"accessToken\":\"cli-token\",\"expiresOn\":\"2099-01-01 00:00:00.000000\",\"expires_on\":4070908800,\"tenant\":\"t\",\"tokenType\":\"Bearer\"}'\n",
    )
    .unwrap();
    let bad_az = dir.join("az-bad");
    std::fs::write(&bad_az, "#!/bin/sh\necho \"ERROR: Please run 'az login' to setup account.\" >&2\nexit 1\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for p in [&ok_az, &bad_az] {
            std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }
    let t = rowglow_lib::entra::azure_cli(Some(ok_az.clone()), Some("contoso.onmicrosoft.com")).await.unwrap();
    assert_eq!(t.access_token, "cli-token");
    let e = rowglow_lib::entra::azure_cli(Some(bad_az), None).await.unwrap_err();
    assert!(e.message.contains("az login"), "{}", e.message);
    assert!(rowglow_lib::entra::azure_cli(Some(ok_az), Some("--output")).await.is_err(), "flag-like tenant refused");
    std::fs::remove_dir_all(&dir).ok();

    // 3. The token reaches SQL Server's login (FEDAUTH). Our Docker server has
    //    no Entra ID, so it must refuse the login — cleanly, not hang or crash.
    let Some(_) = test_env("MSSQL_SA_PASSWORD") else { return };
    let body = r#"{"token_type":"Bearer","expires_in":3599,"access_token":"eyJ.not.real"}"#;
    let (url, req) = one_shot_http(format!(
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    ))
    .await;
    std::env::set_var("ROWGLOW_ENTRA_AUTHORITY", &url);
    let mut c = cfg(Engine::Mssql);
    c.username = "app-id".into();
    c.database = "master".into();
    c.options.auth = "entra-sp".into();
    c.options.tenant = "contoso.onmicrosoft.com".into();
    let started = std::time::Instant::now();
    let res = tokio::time::timeout(Duration::from_secs(30), Session::open("ms-entra".into(), &c, Some("secret"), None, None)).await;
    std::env::remove_var("ROWGLOW_ENTRA_AUTHORITY");
    let err = res.expect("login must not hang").err().expect("SQL Server without Entra must refuse the token");
    eprintln!("SQL Server answered the Entra token with: {} ({:?})", err.message, started.elapsed());
    assert!(req.await.unwrap().contains("client_secret=secret"), "the token was requested");
    assert!(!err.message.is_empty());

    // Entra needs encryption; a plain connection is refused up front.
    c.ssl_mode = SslMode::Disable;
    let e = Session::open("ms-entra2".into(), &c, Some("secret"), None, None).await.err().unwrap();
    assert!(e.message.contains("encrypted"), "{}", e.message);
    // Windows sign-in is for the Windows build.
    #[cfg(not(windows))]
    {
        c.ssl_mode = SslMode::Prefer;
        c.options.auth = "windows".into();
        let e = Session::open("ms-win".into(), &c, Some("x"), None, None).await.err().unwrap();
        assert!(e.message.contains("Rowglow for Windows"), "{}", e.message);
    }
}

#[tokio::test]
async fn radar_finds_new_engines() {
    if !enabled() {
        return;
    }
    let (Some(ms_pw), Some(redis_pw), Some(mongo_pw)) =
        (test_env("MSSQL_SA_PASSWORD"), test_env("REDIS_PASSWORD"), test_env("MONGO_PASSWORD"))
    else {
        return;
    };
    let found = rowglow_lib::discovery::scan().await;
    for d in &found {
        eprintln!("radar: {:?} {} {:?} :{} {} {:?} {:?}", d.engine, d.flavor, d.version, d.port, d.source, d.container, d.note);
    }
    let ms = found.iter().find(|d| d.container.as_deref() == Some("rg-mssql")).expect("SQL Server container");
    assert_eq!(ms.engine, Engine::Mssql);
    assert_eq!(ms.port, 1433);
    assert_eq!(ms.username.as_deref(), Some("sa"));
    assert_eq!(ms.password.as_deref(), Some(ms_pw.as_str()));
    assert!(ms.version.as_deref().unwrap_or("").starts_with("16."), "PRELOGIN version: {:?}", ms.version);
    assert_eq!(ms.note.as_deref(), Some("SQL Server 2022"));

    let redis = found.iter().find(|d| d.container.as_deref() == Some("rg-redis")).expect("Redis container");
    assert_eq!(redis.engine, Engine::Redis);
    assert_eq!(redis.password.as_deref(), Some(redis_pw.as_str()), "from --requirepass");

    let mongo = found.iter().find(|d| d.container.as_deref() == Some("rg-mongo")).expect("MongoDB container");
    assert_eq!(mongo.engine, Engine::Mongodb);
    assert_eq!(mongo.username.as_deref(), Some("rowglow"));
    assert_eq!(mongo.password.as_deref(), Some(mongo_pw.as_str()));
    assert!(mongo.version.as_deref().unwrap_or("").starts_with("7."), "buildInfo version: {:?}", mongo.version);

    // The Sentinel's master isn't published by its container's default port,
    // so it is found by the PING probe — which reports it needs a password.
    let probed = found.iter().find(|d| d.port == 6390).expect("Redis on :6390");
    assert_eq!(probed.engine, Engine::Redis);
    assert_eq!(probed.note.as_deref(), Some("Password required"));
    assert!(probed.password.is_none());
}
