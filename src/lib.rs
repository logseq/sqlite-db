use std::{
    cell::RefCell,
    collections::HashMap,
    sync::{Mutex, RwLock},
};

use once_cell::sync::Lazy;
use rusqlite::{named_params, params};
use serde::{Deserialize, Serialize};
use wasm_bindgen::prelude::*;

pub use self::sqlite_opfs::{get_version, has_opfs_support};

mod sqlite_opfs;

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = console)]
    pub fn log(s: &str);
}

#[macro_export]
macro_rules! console_log {
    ($($arg:tt)*) => {{
        self::log(&std::format!("{}:{} {}", file!(), line!(), std::format!($($arg)*)));
    }};
}

/// Init sqlite binding, preload file sync access handles
/// This should be the only async fn
#[wasm_bindgen]
pub async fn init() -> Result<(), JsValue> {
    console_log!(
        "[logseq-db] init {} {}",
        env!("CARGO_PKG_NAME"),
        env!("CARGO_PKG_VERSION")
    );

    set_panic_hook();

    sqlite_opfs::init_sqlite().await.unwrap();

    Ok(())
}

fn set_panic_hook() {
    // When the `console_error_panic_hook` feature is enabled, we can call the
    // `set_panic_hook` function at least once during initialization, and then
    // we will get better error messages if our code ever panics.
    //
    // For more details see
    // https://github.com/rustwasm/console_error_panic_hook#readme
    #[cfg(feature = "console_error_panic_hook")]
    console_error_panic_hook::set_once();
}

static CONNS: Lazy<Mutex<HashMap<String, RefCell<rusqlite::Connection>>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

#[derive(Serialize, Deserialize, Debug)]
pub struct Block {
    uuid: String,
    #[serde(rename = "type")]
    block_type: i32,
    page_uuid: String,
    page_journal_day: i32,
    name: String,
    content: String,
    datoms: String,
    created_at: i64,
    updated_at: i64,
}

#[wasm_bindgen]
pub fn new_db(db: &str) -> Result<(), JsValue> {
    let conn = rusqlite::Connection::open(db).unwrap();

    let sql = r#"
    CREATE TABLE IF NOT EXISTS blocks (
        uuid TEXT PRIMARY KEY NOT NULL,
        type INTEGER NOT NULL,
        page_uuid TEXT,
        page_journal_day INTEGER,
        name TEXT UNIQUE,
        content TEXT,
        datoms TEXT,
        created_at INTEGER NOT NULL,
        updated_at INTEGER NOT NULL
        )"#;
    conn.execute(sql, params![]).unwrap();

    let sql = "CREATE INDEX IF NOT EXISTS block_type ON blocks(type)";
    conn.execute(sql, params![]).unwrap();

    CONNS
        .lock()
        .unwrap()
        .insert(db.to_string(), RefCell::new(conn));

    Ok(())
}

#[wasm_bindgen]
pub fn delete_blocks(db: &str, uuids: Vec<String>) -> Result<(), JsValue> {
    let conns = CONNS.lock().unwrap();
    let conn = conns.get(db).unwrap().borrow();

    let sql = "DELETE FROM blocks WHERE uuid = ?";
    for uuid in uuids {
        conn.execute(sql, params![uuid]).unwrap();
    }

    Ok(())
}

#[wasm_bindgen]
pub fn upsert_blocks(db: &str, blocks: JsValue) -> Result<(), JsValue> {
    console_log!("upsert_blocks: {:?}", blocks);
    let conns = CONNS.lock().unwrap();
    let mut conn = conns.get(db).unwrap().borrow_mut();

    let tx = conn.transaction().unwrap();

    let sql = r#"
    INSERT INTO blocks (uuid, type, page_uuid, page_journal_day, name, content,datoms, created_at, updated_at)
            VALUES (@uuid, @type, @page_uuid, @page_journal_day, @name, @content, @datoms, @created_at, @updated_at)
            ON CONFLICT (uuid)
            DO UPDATE
                SET (type, page_uuid, page_journal_day, name, content, datoms, created_at, updated_at)
                = (@type, @page_uuid, @page_journal_day, @name, @content, @datoms, @created_at, @updated_at)
    "#;

    let blocks: Vec<Block> = serde_wasm_bindgen::from_value(blocks).unwrap();
    for block in blocks {
        tx.execute(
            sql,
            named_params! {
                "@uuid": block.uuid,
                "@type": block.block_type,
                "@page_uuid": block.page_uuid,
                "@page_journal_day": block.page_journal_day,
                "@name": block.name,
                "@content": block.content,
                "@datoms": block.datoms,
                "@created_at": block.created_at,
                "@updated_at": block.updated_at,
            },
        )
        .unwrap();
    }

    tx.commit().unwrap();
    Ok(())
}

pub fn rusqlite_test() -> Result<(), JsValue> {
    use rusqlite::{params, Connection};

    #[derive(Debug)]
    struct Person {
        id: i32,
        name: String,
        data: Option<Vec<u8>>,
    }

    let conn = Connection::open("demo.db").unwrap();
    conn.execute(
        "CREATE TABLE IF NOT EXISTS person (
                  id              INTEGER PRIMARY KEY,
                  name            TEXT NOT NULL,
                  data            BLOB
                  )",
        params![],
    )
    .unwrap();

    let me = Person {
        id: 1,
        name: "Steven".to_string(),
        data: None,
    };

    let start = js_sys::Date::now();
    for _ in 0..500 {
        conn.execute(
            "INSERT INTO person (name, data) VALUES (?1, ?2)",
            params![me.name, me.data],
        )
        .unwrap();
    }
    let elapsed = js_sys::Date::now() - start;
    console_log!("insert 500 rows: {:?}ms", elapsed);

    let mut stmt = conn
        .prepare("SELECT id, name, data FROM person limit 10")
        .unwrap();
    let person_iter = stmt
        .query_map(params![], |row| {
            Ok(Person {
                id: row.get(0)?,
                name: row.get(1)?,
                data: row.get(2)?,
            })
        })
        .unwrap();

    for person in person_iter {
        console_log!("Found person {:?}", person.unwrap());
    }

    Ok(())
}
