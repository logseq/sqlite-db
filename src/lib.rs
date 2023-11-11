use std::{cell::RefCell, collections::HashMap, sync::Mutex};

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

#[wasm_bindgen]
pub fn fetch_all_pages(db: &str) -> Result<JsValue, JsValue> {
    let conns = CONNS.lock().unwrap();
    let conn = conns.get(db).unwrap().borrow();

    let mut stmt = conn.prepare("SELECT * FROM blocks WHERE type = 2").unwrap();
    let pages_iter = stmt
        .query_map(params![], |row| {
            Ok(Block {
                uuid: row.get(0)?,
                block_type: row.get(1)?,
                page_uuid: row.get(2)?,
                page_journal_day: row.get(3)?,
                name: row.get(4)?,
                content: row.get(5)?,
                datoms: row.get(6)?,
                created_at: row.get(7)?,
                updated_at: row.get(8)?,
            })
        })
        .unwrap();

    let mut pages = Vec::new();
    for page in pages_iter {
        pages.push(page.unwrap());
    }

    Ok(serde_wasm_bindgen::to_value(&pages).unwrap())
}

#[derive(Serialize, Deserialize, Debug)]
pub struct BlockInfo {
    uuid: String,
    page_uuid: String,
}

/// Fetch all blocks and its page uuid
/// => [{uuid: string, page_uuid: string}]
#[wasm_bindgen]
pub fn fetch_all_blocks(db: &str) -> Result<JsValue, JsValue> {
    let conns = CONNS.lock().unwrap();
    let conn = conns.get(db).unwrap().borrow();

    let mut stmt = conn
        .prepare("SELECT uuid, page_uuid FROM blocks WHERE type = 1")
        .unwrap();
    let blocks_iter = stmt
        .query_map(params![], |row| {
            Ok(BlockInfo {
                uuid: row.get(0)?,
                page_uuid: row.get(1)?,
            })
        })
        .unwrap();

    let mut blocks = Vec::new();
    for block in blocks_iter {
        blocks.push(block.unwrap());
    }

    Ok(serde_wasm_bindgen::to_value(&blocks).unwrap())
}

pub fn fetch_recent_journals(db: &str) -> Result<JsValue, JsValue> {
    let conns = CONNS.lock().unwrap();
    let conn = conns.get(db).unwrap().borrow();

    let mut stmt = conn
        .prepare("SELECT uuid FROM blocks WHERE type = 2 ORDER BY page_journal_day DESC LIMIT 3")
        .unwrap();
    let mut uuids: Vec<String> = vec![];
    let rows = stmt.query_map(params![], |row| row.get(0)).unwrap();

    for row in rows {
        uuids.push(row.unwrap());
    }

    let mut sql = "SELECT * FROM blocks WHERE type = 1 AND page_uuid IN (".to_string();
    for (i, uuid) in uuids.iter().enumerate() {
        if i > 0 {
            sql.push_str(", ");
        }
        sql.push_str("'");
        sql.push_str(uuid);
        sql.push_str("'");
    }
    sql.push_str(")");

    let mut stmt = conn.prepare(&sql).unwrap();
    let pages_iter = stmt
        .query_map(params![], |row| {
            Ok(Block {
                uuid: row.get(0)?,
                block_type: row.get(1)?,
                page_uuid: row.get(2)?,
                page_journal_day: row.get(3)?,
                name: row.get(4)?,
                content: row.get(5)?,
                datoms: row.get(6)?,
                created_at: row.get(7)?,
                updated_at: row.get(8)?,
            })
        })
        .unwrap();

    let mut pages = Vec::new();
    for page in pages_iter {
        pages.push(page.unwrap());
    }

    Ok(serde_wasm_bindgen::to_value(&pages).unwrap())
}

#[wasm_bindgen]
pub fn fetch_init_data(db: &str) -> Result<JsValue, JsValue> {
    let conns = CONNS.lock().unwrap();
    let conn = conns.get(db).unwrap().borrow();

    let mut stmt = conn
        .prepare("SELECT * FROM blocks WHERE type IN (3, 4, 5, 6)")
        .unwrap();
    let blocks_iter = stmt
        .query_map(params![], |row| {
            Ok(Block {
                uuid: row.get(0)?,
                block_type: row.get(1)?,
                page_uuid: row.get(2)?,
                page_journal_day: row.get(3)?,
                name: row.get(4)?,
                content: row.get(5)?,
                datoms: row.get(6)?,
                created_at: row.get(7)?,
                updated_at: row.get(8)?,
            })
        })
        .unwrap();

    let mut blocks = Vec::new();
    for block in blocks_iter {
        blocks.push(block.unwrap());
    }

    Ok(serde_wasm_bindgen::to_value(&blocks).unwrap())
}

#[wasm_bindgen]
pub fn fetch_blocks_excluding(db: &str, excluded_uuids: JsValue) -> Result<JsValue, JsValue> {
    let conns = CONNS.lock().unwrap();
    let conn = conns.get(db).unwrap().borrow();

    let excluded_uuids: Vec<String> = serde_wasm_bindgen::from_value(excluded_uuids).unwrap();

    let mut sql = "SELECT * FROM blocks WHERE type = 1 AND uuid NOT IN (".to_string();
    for (i, uuid) in excluded_uuids.iter().enumerate() {
        if i > 0 {
            sql.push_str(", ");
        }
        sql.push_str("'");
        sql.push_str(uuid);
        sql.push_str("'");
    }
    sql.push_str(")");

    let mut stmt = conn.prepare(&sql).unwrap();
    let blocks_iter = stmt
        .query_map(params![], |row| {
            Ok(Block {
                uuid: row.get(0)?,
                block_type: row.get(1)?,
                page_uuid: row.get(2)?,
                page_journal_day: row.get(3)?,
                name: row.get(4)?,
                content: row.get(5)?,
                datoms: row.get(6)?,
                created_at: row.get(7)?,
                updated_at: row.get(8)?,
            })
        })
        .unwrap();

    let mut blocks = Vec::new();
    for block in blocks_iter {
        blocks.push(block.unwrap());
    }

    Ok(serde_wasm_bindgen::to_value(&blocks).unwrap())
}

// unit test

pub fn block_db_test() -> Result<(), JsValue> {
    let _ = new_db("my-graph").unwrap();

    let dummy_blocks = vec![
        Block {
            uuid: "1".to_string(),
            block_type: 1,
            page_uuid: "1".to_string(),
            page_journal_day: 1,
            name: "1".to_string(),
            content: "1".to_string(),
            datoms: "1".to_string(),
            created_at: 1,
            updated_at: 1,
        },
        Block {
            uuid: "2".to_string(),
            block_type: 1,
            page_uuid: "1".to_string(),
            page_journal_day: 1,
            name: "2".to_string(),
            content: "2".to_string(),
            datoms: "2".to_string(),
            created_at: 2,
            updated_at: 2,
        },
        Block {
            uuid: "3".to_string(),
            block_type: 1,
            page_uuid: "1".to_string(),
            page_journal_day: 1,
            name: "3".to_string(),
            content: "3".to_string(),
            datoms: "3".to_string(),
            created_at: 3,
            updated_at: 3,
        },
    ];
    let val = serde_wasm_bindgen::to_value(&dummy_blocks).unwrap();

    let _ = upsert_blocks("my-graph", val).unwrap();

    let val = fetch_all_blocks("my-graph").unwrap();
    let blocks: Vec<BlockInfo> = serde_wasm_bindgen::from_value(val).unwrap();
    assert_eq!(blocks.len(), 3);

    console_log!("blocks: {:#?}", blocks);

    Ok(())
}

pub fn rusqlite_test() -> Result<(), JsValue> {
    use rusqlite::Connection;

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
