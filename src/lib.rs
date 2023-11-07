use core::slice;
use std::{
    collections::HashMap,
    ffi::{c_char, c_int, c_void, CStr, CString},
    ptr,
    sync::{atomic::AtomicBool, Arc, RwLock},
    thread,
    time::SystemTime,
};

use js_sys::{
    Array, ArrayBuffer, Atomics, DataView, Function, Int32Array, Object, Promise, Reflect,
    SharedArrayBuffer, JSON,
};
use libsqlite3_sys::*;
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::{spawn_local, JsFuture};
use web_sys::{
    FileSystemDirectoryHandle, FileSystemFileHandle, FileSystemGetFileOptions, FileSystemHandle,
    FileSystemReadWriteOptions, FileSystemSyncAccessHandle, TextDecoder, TextEncoder,
};

pub mod vfs;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct GlobalMetadata {
    // init at library load, load all sync access handles
    // file_handle_pool: HashMap<String, FileSystemSyncAccessHandle>,
    // in fs metadata, path to uuid file name
    version: i32,
    #[serde(default)]
    empty_files: Vec<String>,
    // display name to uuid file name
    #[serde(default)]
    files: HashMap<String, String>,
}

static mut FILE_POOL: Lazy<RwLock<HashMap<String, FileSystemSyncAccessHandle>>> =
    Lazy::new(|| RwLock::new(HashMap::new()));

static mut METADATA: Option<GlobalMetadata> = None;

#[repr(C)]
pub struct FileHandle {
    _super: sqlite3_file,
    sah: FileSystemSyncAccessHandle,
}

/// The pool
struct Pool {
    metadata: RwLock<GlobalMetadata>,
    handle_pool: RwLock<HashMap<String, FileSystemSyncAccessHandle>>,

    sqlite_files: RwLock<HashMap<String, FileHandle>>,
}

static mut POOL: Lazy<Pool> = Lazy::new(|| Pool {
    metadata: RwLock::new(GlobalMetadata {
        version: 1,
        empty_files: Vec::new(),
        files: HashMap::new(),
    }),
    handle_pool: RwLock::new(HashMap::new()),
    sqlite_files: RwLock::new(HashMap::new()),
});

impl Pool {
    pub fn read(&self, handle: &FileHandle, buf: &mut [u8], offset: u64) -> Result<(), JsValue> {
        let mut opts = FileSystemReadWriteOptions::default();
        Reflect::set(&mut opts, &"at".into(), &0.into())?;

        handle.sah.read_with_u8_array_and_options(buf, &opts)?;

        Ok(())
    }

    pub fn write(&self, handle: &FileHandle, buf: &[u8], offset: u64) -> Result<(), JsValue> {
        let mut opts = FileSystemReadWriteOptions::default();
        Reflect::set(&mut opts, &"at".into(), &0.into())?;

        handle.sah.write_with_u8_array_and_options(buf, &opts)?;

        Ok(())
    }
}

impl GlobalMetadata {
    pub fn persist(&self) -> Result<(), JsValue> {
        let pool = unsafe { FILE_POOL.read().unwrap() };
        let handle = pool.get("metadata.json").unwrap();

        let jsmeta = serde_wasm_bindgen::to_value(self).unwrap();
        let s = JSON::stringify(&jsmeta).unwrap();
        let raw = s.as_string().unwrap().as_bytes().to_vec();

        let mut opts = FileSystemReadWriteOptions::default();
        Reflect::set(&mut opts, &"at".into(), &0.into())?;
        let new_size = raw.len();

        handle.write_with_u8_array_and_options(&raw, &opts)?;
        handle.truncate_with_u32(new_size as u32)?;
        handle.flush()?;

        Ok(())
    }

    fn get_file_handle(&self, path: &str) -> Result<FileSystemSyncAccessHandle, JsValue> {
        if let Some(mapped_path) = self.files.get(path) {
            let pool = unsafe { FILE_POOL.read().unwrap() };
            let handle = pool.get(&*mapped_path).unwrap();
            return Ok(handle.clone());
        } else {
            return Err(JsValue::from_str("file not found"));
        }
    }

    /// Get file size by mapped file name
    pub fn file_size(&self, path: &str) -> Result<u64, JsValue> {
        let handle = self.get_file_handle(path)?;

        let size = handle.get_size()?;
        Ok(size as _)
    }

    // allocate n empty files
    async fn init_empty_files(root: &FileSystemDirectoryHandle, n: usize) -> Result<(), JsValue> {
        for _ in 0..n {
            let name = Uuid::new_v4().to_string() + ".raw";
            console_log!("create empty file: {}", name);

            let mut get_file_opts = &FileSystemGetFileOptions::default();
            Reflect::set(&mut get_file_opts, &"create".into(), &true.into())?;

            console_log!("opt=> {}", js_sys::JSON::stringify(&get_file_opts).unwrap());
            let file_handle: FileSystemFileHandle =
                JsFuture::from(root.get_file_handle_with_options(&name, &get_file_opts))
                    .await?
                    .into();

            console_log!("opfs_root: {:?}", root);
            console_log!("file_handle: {:?}", file_handle);

            let sync_handle: FileSystemSyncAccessHandle =
                JsFuture::from(file_handle.create_sync_access_handle())
                    .await?
                    .into();
            if let Ok(mut pool) = unsafe { FILE_POOL.write() } {
                pool.insert(name.clone(), sync_handle);
            }
            unsafe {
                METADATA.as_mut().unwrap().empty_files.push(name);
            }
        }
        Ok(())
    }
}

pub struct WrappedFs {}

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = console)]
    pub fn log(s: &str);
}

#[macro_export]
macro_rules! console_log {
    () => {
        log("");
    };
    ($($arg:tt)*) => {{
        self::log(&std::format!($($arg)*));
    }};
}

async fn entries_to_vec(
    entries: &JsValue,
) -> Result<Vec<(String, FileSystemSyncAccessHandle)>, JsValue> {
    let mut ret = Vec::new();
    let next_fn: Function = Reflect::get(entries, &"next".into())?.unchecked_into();
    let arr = Array::new();

    let mut entry_fut: Promise = Reflect::apply(&next_fn, entries, &arr)?.into();
    let mut entry = JsFuture::from(entry_fut).await?;

    // access the iteractor
    let mut done = Reflect::get(&entry, &"done".into())?.as_bool().unwrap();
    while !done {
        // Array<[string, FileSystemFileHandle]>
        let value: Array = Reflect::get(&entry, &"value".into())?.into();
        // console_log!("value: {:?}", value);

        let path = value.get(0).as_string().unwrap();
        let handle = value.get(1).unchecked_into::<FileSystemFileHandle>();

        // only cares about .raw files
        if path.ends_with(".raw") {
            let sync_handle: FileSystemSyncAccessHandle =
                JsFuture::from(handle.create_sync_access_handle())
                    .await?
                    .into();

            // console_log!("item path: {} {:?}", path, sync_handle);
            ret.push((path, sync_handle));
        }

        // ret.push(value.as_string().unwrap());

        entry_fut = Reflect::apply(&next_fn, entries, &arr)?.into();
        entry = JsFuture::from(entry_fut).await?;

        done = Reflect::get(&entry, &"done".into())?.as_bool().unwrap();
    }
    Ok(ret)
}

async fn list_all_raw_files(
    root: &FileSystemDirectoryHandle,
) -> Result<Vec<(String, FileSystemSyncAccessHandle)>, JsValue> {
    // call FileSystemDirectoryHandle.entries()

    let entries_fn = Reflect::get(&root, &"entries".into())?;
    let entries = Reflect::apply(entries_fn.unchecked_ref(), &root, &Array::new())?;

    let entries = entries_to_vec(&entries).await?;
    // console_log!("entries: {:#?}", entries);
    Ok(entries)
}

async fn get_file_handle(
    root: &FileSystemDirectoryHandle,
    path: &str,
) -> Result<FileSystemSyncAccessHandle, JsValue> {
    if let Some(sync_handle) = unsafe { FILE_POOL.read().unwrap().get(path) } {
        console_log!("already opened {}", path);
        return Ok(sync_handle.clone());
    }

    let mut opts = &FileSystemGetFileOptions::default();
    Reflect::set(&mut opts, &"create".into(), &true.into())?;

    let handle: FileSystemFileHandle =
        JsFuture::from(root.get_file_handle_with_options(path, &opts))
            .await?
            .into();
    let sync_handle: FileSystemSyncAccessHandle =
        JsFuture::from(handle.create_sync_access_handle())
            .await?
            .into();

    unsafe {
        FILE_POOL
            .write()
            .unwrap()
            .insert(path.to_string(), sync_handle.clone());
    }

    Ok(sync_handle)
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

    // open all files
    let global_this = js_sys::global().dyn_into::<web_sys::WorkerGlobalScope>()?;
    let navigator = global_this.navigator();

    let opfs_root: FileSystemDirectoryHandle = JsFuture::from(navigator.storage().get_directory())
        .await?
        .into();

    let metadata_file = get_file_handle(&opfs_root, "metadata.json").await?;
    let mut metadata = if metadata_file.get_size()? as u64 == 0 {
        // create new metadata
        GlobalMetadata {
            version: 1,
            empty_files: Vec::new(),
            files: HashMap::new(),
        }
    } else {
        let size = metadata_file.get_size()?;
        let mut buf = vec![0; size as usize];
        let _nread = metadata_file.read_with_u8_array(&mut buf[..])?;
        let text = String::from_utf8(buf).unwrap();
        let jsobj = JSON::parse(&text).unwrap();
        let metadata: GlobalMetadata = serde_wasm_bindgen::from_value(jsobj).unwrap();

        metadata
    };

    let entries = list_all_raw_files(&opfs_root).await?;
    for (path, handle) in entries {
        if !metadata.empty_files.contains(&path) && !metadata.files.contains_key(&path) {
            metadata.empty_files.push(path.clone());
        }

        let mut pool = unsafe { FILE_POOL.write().unwrap() };
        pool.insert(path.clone(), handle);
    }

    unsafe {
        METADATA = Some(metadata);
        METADATA.as_ref().unwrap().persist()?;
    }

    // create more empty files
    if unsafe { METADATA.as_ref().unwrap().empty_files.len() < 10 } {
        GlobalMetadata::init_empty_files(&opfs_root, 10).await?;
    }

    console_log!("metadata: {:#?}", unsafe { &METADATA });

    Ok(())
}

// sqlite part
const SECTOR_SIZE: u32 = 4096;
unsafe extern "C" fn x_close(arg1: *mut sqlite3_file) -> c_int {
    console_log!("TODO: xclose");

    SQLITE_OK
}
unsafe extern "C" fn x_read(
    arg1: *mut sqlite3_file,
    arg2: *mut c_void,
    iAmt: c_int,
    iOfst: sqlite3_int64,
) -> c_int {
    console_log!("xRead amt={} offset{}", iAmt, iOfst);

    let file: *mut FileHandle = arg1 as _;

    let mut buf = slice::from_raw_parts_mut(arg2 as *mut u8, iAmt as usize);
    POOL.read(&*file, buf, iOfst as _).unwrap();

    SQLITE_OK
}
unsafe extern "C" fn xWrite(
    arg1: *mut sqlite3_file,
    arg2: *const c_void,
    iAmt: c_int,
    iOfst: sqlite3_int64,
) -> c_int {
    console_log!("xWrite amt={} offset{}", iAmt, iOfst);

    let file: *mut FileHandle = arg1 as _;

    let buf = slice::from_raw_parts(arg2 as *const u8, iAmt as usize);
    POOL.write(&*file, buf, iOfst as _).unwrap();

    SQLITE_OK
}
unsafe extern "C" fn x_truncate(arg1: *mut sqlite3_file, size: sqlite3_int64) -> c_int {
    console_log!("xTruncate: {}", size);

    let file: *mut FileHandle = arg1 as _;
    (&*file).sah.truncate_with_u32(size as u32).unwrap();

    SQLITE_OK
}
unsafe extern "C" fn x_sync(arg1: *mut sqlite3_file, flags: c_int) -> c_int {
    console_log!("xSync: {}", flags);

    let file: *mut FileHandle = arg1 as _;
    (&*file).sah.flush().unwrap();

    SQLITE_OK
}
unsafe extern "C" fn x_file_size(arg1: *mut sqlite3_file, pSize: *mut sqlite3_int64) -> c_int {
    console_log!("calling file size");

    let file: *mut FileHandle = arg1 as _;
    let sz = (&*file).sah.get_size().unwrap() as i64;
    *pSize = sz;

    SQLITE_OK
}
unsafe extern "C" fn x_lock(arg1: *mut sqlite3_file, arg2: c_int) -> c_int {
    console_log!("TODO: lock");
    SQLITE_OK
}
unsafe extern "C" fn x_unlock(arg1: *mut sqlite3_file, arg2: c_int) -> c_int {
    console_log!("TODO: unlock");
    SQLITE_OK
}
unsafe extern "C" fn x_check_reserved_lock(arg1: *mut sqlite3_file, pResOut: *mut c_int) -> c_int {
    console_log!("TODO: check reserved lock");
    SQLITE_OK
}

extern "C" fn x_sector_size(arg1: *mut sqlite3_file) -> c_int {
    SECTOR_SIZE as i32
}
extern "C" fn x_file_control(arg1: *mut sqlite3_file, op: c_int, arg: *mut c_void) -> c_int {
    SQLITE_NOTFOUND
}
extern "C" fn x_device_characteristics(arg1: *mut sqlite3_file) -> c_int {
    SQLITE_IOCAP_UNDELETABLE_WHEN_OPEN
}

static IO_METHODS: sqlite3_io_methods = sqlite3_io_methods {
    iVersion: 1,
    xClose: Some(x_close),
    xRead: Some(x_read),
    xWrite: Some(xWrite),
    xTruncate: Some(x_truncate),
    xSync: Some(x_sync),
    xFileSize: Some(x_file_size),
    xLock: Some(x_lock),
    xUnlock: Some(x_unlock),
    xCheckReservedLock: Some(x_check_reserved_lock),
    xFileControl: Some(x_file_control),
    xSectorSize: Some(x_sector_size),
    xDeviceCharacteristics: Some(x_device_characteristics),
    /* Methods above are valid for version 1 */
    xShmMap: None,
    xShmLock: None,
    xShmBarrier: None,
    xShmUnmap: None,
    xFetch: None,
    xUnfetch: None,
};

// vfs layer

pub unsafe extern "C" fn opfs_vfs_open(
    pVfs: *mut sqlite3_vfs,
    zName: *const c_char,
    pFile: *mut sqlite3_file,
    flags: c_int,
    pOutFlags: *mut c_int,
) -> c_int {
    console_log!("opfs_vfs_open");

    let name = CStr::from_ptr(zName).to_str().unwrap();
    console_log!("open file name => {}", name);

    if SQLITE_OPEN_CREATE & flags == SQLITE_OPEN_CREATE {
        console_log!("create file");
    }

    //let file = pFile as *mut FileHandle;
    //(*file)._super.pMethods = &IO_METHODS;
    //(*file).sah = POOL.get_file_handle(name).unwrap();

    SQLITE_ERROR
}
unsafe extern "C" fn opfs_vfs_delete(
    arg1: *mut sqlite3_vfs,
    zName: *const c_char,
    syncDir: c_int,
) -> c_int {
    console_log!("opfs_vfs_delete");

    let name = CStr::from_ptr(zName).to_str().unwrap();
    console_log!("delete file name => {}", name);

    SQLITE_ERROR
}
unsafe extern "C" fn opfs_vfs_access(
    arg1: *mut sqlite3_vfs,
    zName: *const c_char,
    flags: c_int,
    pResOut: *mut c_int,
) -> c_int {
    let name = CStr::from_ptr(zName).to_str().unwrap();
    console_log!("access file name => {}", name);
    console_log!("flags => {}", flags);

    SQLITE_ERROR
}

unsafe extern "C" fn opfs_vfs_fullpathname(
    arg1: *mut sqlite3_vfs,
    zName: *const c_char,
    nOut: c_int,
    zOut: *mut c_char,
) -> c_int {
    console_log!("opfs_vfs_fullpathname");

    let name = CStr::from_ptr(zName).to_str().unwrap();
    let n = name.len();
    if n > nOut as usize {
        return SQLITE_CANTOPEN;
    }

    ptr::copy_nonoverlapping(zName, zOut, nOut as _);

    SQLITE_OK
}

unsafe extern "C" fn opfs_vfs_randomness(
    arg1: *mut sqlite3_vfs,
    nByte: c_int,
    zOut: *mut c_char,
) -> c_int {
    console_log!("opfs_vfs_randomness");

    let buf = slice::from_raw_parts_mut(zOut as *mut u8, nByte as usize);

    getrandom::getrandom(buf).unwrap();

    SQLITE_OK
}

unsafe extern "C" fn opfs_vfs_sleep(arg1: *mut sqlite3_vfs, microseconds: c_int) -> c_int {
    console_log!("opfs_vfs_sleep");

    SQLITE_OK
}

unsafe extern "C" fn opfs_vfs_currenttime(arg1: *mut sqlite3_vfs, arg2: *mut f64) -> c_int {
    console_log!("opfs_vfs_currenttime");

    let t = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_secs_f64();

    *arg2 = t;

    SQLITE_OK
}

unsafe extern "C" fn opfs_vfs_get_last_error(
    arg1: *mut sqlite3_vfs,
    arg2: c_int,
    arg3: *mut c_char,
) -> c_int {
    unimplemented!("opfs_vfs_get_last_error");
    SQLITE_OK
}

#[wasm_bindgen]
pub fn init_sqlite() -> Result<(), JsValue> {
    let vfs = sqlite3_vfs {
        iVersion: 1,
        szOsFile: std::mem::size_of::<FileHandle>() as _, // size of sqlite3_file
        mxPathname: 1024,
        pNext: ptr::null_mut(),
        zName: "logseq-sahpool-opfs\0".as_ptr() as *const c_char,
        pAppData: ptr::null_mut(),
        xOpen: Some(opfs_vfs_open),
        xDelete: Some(opfs_vfs_delete),
        xAccess: Some(opfs_vfs_access),
        xFullPathname: Some(opfs_vfs_fullpathname),
        // run-time extension support
        xDlOpen: None,
        xDlError: None,
        xDlSym: None,
        xDlClose: None,
        xRandomness: Some(opfs_vfs_randomness),
        xSleep: Some(opfs_vfs_sleep),
        xCurrentTime: Some(opfs_vfs_currenttime),
        xGetLastError: Some(opfs_vfs_get_last_error),
        // The methods above are in version 1 of the sqlite_vfs object definition
        xCurrentTimeInt64: None,
        xSetSystemCall: None,
        xGetSystemCall: None,
        xNextSystemCall: None,
    };

    unsafe {
        sqlite3_vfs_register(Box::leak(Box::new(vfs)), 1);
    }

    Ok(())
}

#[wasm_bindgen]
pub fn get_version() -> String {
    let version = unsafe { CStr::from_ptr(sqlite3_libversion()) };
    version.to_str().unwrap().to_string()
}

pub fn add(left: usize, right: usize) -> usize {
    left + right
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_works() {
        let result = add(2, 2);
        assert_eq!(result, 4);
    }
}

pub fn set_panic_hook() {
    // When the `console_error_panic_hook` feature is enabled, we can call the
    // `set_panic_hook` function at least once during initialization, and then
    // we will get better error messages if our code ever panics.
    //
    // For more details see
    // https://github.com/rustwasm/console_error_panic_hook#readme
    #[cfg(feature = "console_error_panic_hook")]
    console_error_panic_hook::set_once();
}

/// Test OPFS support on current platform
#[wasm_bindgen]
pub fn has_opfs_support() -> bool {
    let global_this = match js_sys::global().dyn_into::<web_sys::WorkerGlobalScope>() {
        Ok(v) => v,
        Err(_) => {
            log("no WorkerGlobalScope");
            return false;
        }
    };

    // check SharedArrayBuffer
    /*  if let Ok(v) = js_sys::Reflect::get(&global_this, &"SharedArrayBuffer".try_into().unwrap()) {
        if v.is_undefined() {
            log(&format!("SharedArrayBuffer {:?}", v));
            log("no SharedArrayBuffer");
            return false;
        }
    }*/
    if let Ok(v) = js_sys::Reflect::get(&global_this, &"Atomics".try_into().unwrap()) {
        if v.is_undefined() {
            log("no Atomics");
            return false;
        }
    }

    // check FileSystemSyncAccessHandle

    if let Ok(v) = js_sys::Reflect::get(&global_this, &"FileSystemFileHandle".try_into().unwrap()) {
        if v.is_undefined() {
            log("no Atomics");
            return false;
        }
        if let Ok(v) = js_sys::Reflect::get(&v, &"prototype".try_into().unwrap()) {
            if v.is_undefined() {
                log("no prototype");
                return false;
            }
            if let Ok(f) = js_sys::Reflect::get(&v, &"createSyncAccessHandle".try_into().unwrap()) {
                if f.is_undefined() {
                    log("no createSyncAccessHandle");
                    return false;
                }
            }
        }
    }

    if let Ok(v) = js_sys::Reflect::get(&global_this, &"navigator".try_into().unwrap()) {
        if v.is_undefined() {
            log("no navigator");
            return false;
        }
    }
    let navigator = global_this.navigator();
    if let Ok(v) = js_sys::Reflect::get(&navigator, &"storage".try_into().unwrap()) {
        if v.is_undefined() {
            log("no storage");
            return false;
        }
    }

    let storage = navigator.storage();
    if let Ok(v) = js_sys::Reflect::get(&storage, &"getDirectory".try_into().unwrap()) {
        if v.is_undefined() {
            log("no getDirectory");
            return false;
        }
    }

    true
}

pub fn dummy_create() -> Result<(), JsValue> {
    unsafe {
        let filename = CString::new("test.db").unwrap();
        let mut db = ptr::null_mut();
        let ret = sqlite3_open_v2(
            filename.as_ptr(),
            &mut db,
            SQLITE_OPEN_READWRITE | SQLITE_OPEN_CREATE,
            ptr::null_mut(),
        );
        console_log!("=> open db {}", ret);
    }

    Ok(())
}
