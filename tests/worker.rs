#![cfg(target_arch = "wasm32")]

extern crate wasm_bindgen_test;

use logseq_sqlite::console_log;
use wasm_bindgen_test::*;

// wasm_bindgen_test_configure!(run_in_browser);
wasm_bindgen_test_configure!(run_in_worker);

#[wasm_bindgen_test]
fn sqlite_version() {
    let x = logseq_sqlite::get_version();
    assert_eq!(x, "3.42.0".to_string());
    logseq_sqlite::log(&format!("sqlite version: {}", x));
}

#[wasm_bindgen_test]
async fn opfs_ok() {
    let has_opfs_support = logseq_sqlite::has_opfs_support();
    assert_eq!(has_opfs_support, true);
}

#[wasm_bindgen_test]
async fn library_init() {
    logseq_sqlite::ensure_init().await.unwrap();

    logseq_sqlite::init_db("my-graph").await.unwrap();

    // logseq_sqlite::rusqlite_test().unwrap();

    logseq_sqlite::block_db_test().unwrap();

    logseq_sqlite::log("all done");
}
