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
