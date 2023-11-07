#![cfg(target_arch = "wasm32")]

extern crate wasm_bindgen_test;
use std::{assert, println};

use logseq_sqlite::console_log;
use wasm_bindgen_test::*;

// wasm_bindgen_test_configure!(run_in_browser);
wasm_bindgen_test_configure!(run_in_worker);

#[wasm_bindgen_test]
fn sqlite_version() {
    //    logseq_sqlite::preinit();
    //  assert_eq!(1 + 1, 2);

    let x = logseq_sqlite::get_version();
    assert_eq!(x, "3.42.0".to_string());
    logseq_sqlite::log(&format!("sqlite version: {}", x));

    //    logseq_sqlite::dummy();
}

#[wasm_bindgen_test]
async fn opfs_ok() {
    let has_opfs_support = logseq_sqlite::has_opfs_support();
    assert_eq!(has_opfs_support, true);
    logseq_sqlite::log("good =>");
}

#[wasm_bindgen_test]
async fn a_demo() {
    let x = logseq_sqlite::init().await.unwrap();

    //    assert_eq!(x, "".to_string());
}

/* [wasm_bindgen_test]
async fn dummy() {
    let x = logseq_sqlite::open_file_handle_pool().await;
    logseq_sqlite::log(&format!("open_file_handle_pool: {:?}", x));
}
*/
