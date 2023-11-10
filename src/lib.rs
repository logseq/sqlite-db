use core::slice;
use std::{
    collections::HashMap,
    ffi::{c_char, c_int, c_void, CStr, CString},
    mem, ptr,
    sync::RwLock,
    time::SystemTime,
};

use js_sys::{Array, Function, Promise, Reflect};
use libsqlite3_sys::*;
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;
use web_sys::{
    FileSystemDirectoryHandle, FileSystemFileHandle, FileSystemGetFileOptions,
    FileSystemReadWriteOptions, FileSystemSyncAccessHandle,
};

pub mod vfs;

const METADATA_FILENAME: &str = "metadata.bincode";
const EMPTY_FILES: usize = 5;

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

/// File handle, inherits sqlite3_file
#[repr(C)]
pub struct FileHandle {
    _super: sqlite3_file,
    fname: String,
    flags: i32,
    /*
    #define SQLITE_LOCK_NONE          0       /* xUnlock() only */
    #define SQLITE_LOCK_SHARED        1       /* xLock() or xUnlock() */
    #define SQLITE_LOCK_RESERVED      2       /* xLock() only */
    #define SQLITE_LOCK_PENDING       3       /* xLock() only */
    #define SQLITE_LOCK_EXCLUSIVE     4       /* xLock() only */
     */
    lock: i32,
}

/// The pool
struct Pool {
    meta_handle: Option<FileSystemSyncAccessHandle>,
    metadata: RwLock<GlobalMetadata>,
    handle_pool: RwLock<HashMap<String, FileSystemSyncAccessHandle>>,
}

static mut POOL: Lazy<Pool> = Lazy::new(|| Pool {
    meta_handle: None,
    metadata: RwLock::new(GlobalMetadata {
        version: 1,
        empty_files: Vec::new(),
        files: HashMap::new(),
    }),
    handle_pool: RwLock::new(HashMap::new()),
});

impl Pool {
    pub fn has_file(&self, path: &str) -> bool {
        let meta = self.metadata.read().unwrap();
        meta.files.contains_key(path)
    }

    fn get_file_handle(&self, path: &str) -> Result<FileSystemSyncAccessHandle, JsValue> {
        let meta = self.metadata.read().unwrap();
        if let Some(mapped_path) = meta.files.get(path) {
            let pool = self.handle_pool.read().unwrap();
            let handle = pool.get(&*mapped_path).unwrap();
            return Ok(handle.clone());
        } else {
            return Err(JsValue::from_str("file not found"));
        }
    }

    fn get_or_create_file(&self, path: &str) -> Result<FileSystemSyncAccessHandle, JsValue> {
        if let Ok(handle) = self.get_file_handle(path) {
            return Ok(handle);
        } else {
            // find a empty file
            let handle = {
                let mut meta = self.metadata.write().unwrap();
                let pool = self.handle_pool.read().unwrap();

                let empty_file = meta.empty_files.pop().unwrap();
                let handle = pool.get(&empty_file).unwrap().clone();
                console_log!("alloc file: {} for {}", empty_file, path);
                meta.files.insert(path.to_string(), empty_file);
                handle
            };
            self.persist_metadata()?;

            Ok(handle)
        }
    }

    pub fn persist_metadata(&self) -> Result<(), JsValue> {
        let handle = self.meta_handle()?;

        // console_log!("persist metadata: {:#?}", &*self.metadata.read().unwrap());

        let raw = bincode::serialize(&*self.metadata.read().unwrap()).unwrap();

        let mut opts = FileSystemReadWriteOptions::default();
        Reflect::set(&mut opts, &"at".into(), &0.into())?;
        let new_size = raw.len();

        handle.write_with_u8_array_and_options(&raw, &opts)?;
        handle.truncate_with_u32(new_size as u32)?;
        handle.flush()?;

        Ok(())
    }

    pub fn read(&self, handle: &FileHandle, buf: &mut [u8], offset: i64) -> Result<u64, JsValue> {
        let mut opts = FileSystemReadWriteOptions::default();
        Reflect::set(&mut opts, &"at".into(), &(offset as f64).into())?;

        let handle = self.get_file_handle(&handle.fname)?;
        let n = handle.read_with_u8_array_and_options(buf, &opts)? as u64;

        Ok(n)
    }

    pub fn write(&self, handle: &FileHandle, buf: &[u8], offset: i64) -> Result<(), JsValue> {
        let mut opts = FileSystemReadWriteOptions::default();
        Reflect::set(&mut opts, &"at".into(), &(offset as f64).into())?;

        let handle = self.get_file_handle(&handle.fname)?;
        let nwritten = handle.write_with_u8_array_and_options(buf, &opts)?;
        assert_eq!(nwritten, buf.len() as f64);
        Ok(())
    }

    pub fn flush(&self, handle: &FileHandle) -> Result<(), JsValue> {
        let handle = self.get_file_handle(&handle.fname)?;
        handle.flush()?;
        Ok(())
    }

    pub fn file_size(&self, path: &str) -> Result<u64, JsValue> {
        let handle = self.get_file_handle(path)?;
        let size = handle.get_size()?;
        console_log!("{:?}  size={}", path, size);
        Ok(size as _)
    }

    pub fn truncate(&self, handle: &FileHandle, new_size: i64) -> Result<(), JsValue> {
        let handle = self.get_file_handle(&handle.fname)?;
        handle.truncate_with_f64(new_size as f64)?;
        Ok(())
    }

    pub fn delete(&self, path: &str) -> Result<(), JsValue> {
        {
            let mut meta = self.metadata.write().unwrap();
            let pool = self.handle_pool.write().unwrap();

            let mapped_path = meta.files.get(path).unwrap().clone();

            let handle = pool.get(&mapped_path).unwrap();

            meta.files.remove(path);
            meta.empty_files.push(mapped_path);

            handle.truncate_with_u32(0).unwrap();
            handle.flush().unwrap();
        }

        self.persist_metadata()?;

        Ok(())
    }

    fn add_new_empty_file(&self, name: String, handle: FileSystemSyncAccessHandle) {
        let mut pool = self.handle_pool.write().unwrap();
        pool.insert(name.clone(), handle);

        let mut meta = self.metadata.write().unwrap();
        meta.empty_files.push(name);
    }

    async fn init_empty_files(
        &self,
        root: &FileSystemDirectoryHandle,
        n: usize,
    ) -> Result<(), JsValue> {
        for _ in 0..n {
            let name = Uuid::new_v4().to_string() + ".raw";
            console_log!("create empty file: {}", name);

            let mut get_file_opts = &FileSystemGetFileOptions::default();
            Reflect::set(&mut get_file_opts, &"create".into(), &true.into())?;

            let file_handle: FileSystemFileHandle =
                JsFuture::from(root.get_file_handle_with_options(&name, &get_file_opts))
                    .await?
                    .into();

            let sync_handle: FileSystemSyncAccessHandle =
                JsFuture::from(file_handle.create_sync_access_handle())
                    .await?
                    .into();

            self.add_new_empty_file(name, sync_handle);
        }
        Ok(())
    }

    fn meta_handle(&self) -> Result<&FileSystemSyncAccessHandle, JsValue> {
        if let Some(handle) = &self.meta_handle {
            Ok(handle)
        } else {
            Err(JsValue::from_str("no meta handle"))
        }
    }

    // load from metadata.json
    pub async fn init(&mut self) -> Result<(), JsValue> {
        let global_this = js_sys::global().dyn_into::<web_sys::WorkerGlobalScope>()?;
        let navigator = global_this.navigator();

        let opfs_root: FileSystemDirectoryHandle =
            JsFuture::from(navigator.storage().get_directory())
                .await?
                .into();

        let metadata_file = get_file_handle_from_root(&opfs_root, METADATA_FILENAME).await?;
        // save handle
        self.meta_handle = Some(metadata_file);

        let meta_size = self.meta_handle()?.get_size()? as usize;
        let metadata = if meta_size == 0 {
            // create new metadata
            GlobalMetadata {
                version: 1,
                empty_files: Vec::new(),
                files: HashMap::new(),
            }
        } else {
            let mut buf = vec![0; meta_size];
            let _nread = self.meta_handle()?.read_with_u8_array(&mut buf[..])?;
            let metadata = bincode::deserialize(&buf).unwrap();

            metadata
        };

        console_log!("loading init metadata: {:#?}", metadata);

        let entries = list_all_raw_files(&opfs_root).await?;
        for (path, handle) in entries {
            //if !metadata.empty_files.contains(&path) && !metadata.files.contains_key(&path) {
            //    metadata.empty_files.push(path.clone());
            // }

            let mut pool = self.handle_pool.write().unwrap();
            pool.insert(path.clone(), handle);
        }

        *self.metadata.write().unwrap() = metadata;

        // create more empty files
        if self.metadata.read().unwrap().empty_files.len() < EMPTY_FILES {
            self.init_empty_files(&opfs_root, EMPTY_FILES).await?;
        }
        self.persist_metadata()?;

        Ok(())
    }
}

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = console)]
    pub fn log(s: &str);
}

#[macro_export]
macro_rules! function {
    () => {{
        fn f() {}
        fn type_name_of<T>(_: T) -> &'static str {
            std::any::type_name::<T>()
        }
        let name = type_name_of(f);

        // Find and cut the rest of the path
        match &name[..name.len() - 3].rfind(':') {
            Some(pos) => &name[pos + 1..name.len() - 3],
            None => &name[..name.len() - 3],
        }
    }};
}

#[macro_export]
macro_rules! console_log {
    ($($arg:tt)*) => {{
        self::log(&std::format!("{}:{} {} : {}", file!(), line!(), function!(), std::format!($($arg)*)));
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

async fn get_file_handle_from_root(
    root: &FileSystemDirectoryHandle,
    path: &str,
) -> Result<FileSystemSyncAccessHandle, JsValue> {
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

    unsafe {
        POOL.init().await?;
    }

    Ok(())
}

// sqlite part
mod io_methods {
    use super::*;
    const SECTOR_SIZE: u32 = 4096;

    pub unsafe extern "C" fn close(fobj: *mut sqlite3_file) -> c_int {
        let file = &mut *(fobj as *mut FileHandle);

        POOL.flush(file).unwrap();

        if file.flags & SQLITE_OPEN_DELETEONCLOSE != 0 {
            POOL.delete(&file.fname).unwrap();

            console_log!("delete file {}", file.fname);
        }

        let name = mem::take(&mut file.fname);
        drop(name);
        sqlite3_free(fobj as *mut c_void);

        SQLITE_OK
    }
    pub unsafe extern "C" fn read(
        arg1: *mut sqlite3_file,
        arg2: *mut c_void,
        amount: c_int,
        offset: sqlite3_int64,
    ) -> c_int {
        let file: *mut FileHandle = arg1 as _;

        console_log!("{:?} {} @ {}", (*file).fname, amount, offset);

        let buf = slice::from_raw_parts_mut(arg2 as *mut u8, amount as usize);
        let nread = POOL.read(&*file, buf, offset).unwrap() as c_int;

        if nread < amount {
            // fill remain with zero0
            buf[nread as usize..].fill(0);
            return SQLITE_IOERR_SHORT_READ;
        }

        SQLITE_OK
    }
    pub unsafe extern "C" fn write(
        arg1: *mut sqlite3_file,
        arg2: *const c_void,
        amount: c_int,
        offset: sqlite3_int64,
    ) -> c_int {
        let file: *mut FileHandle = arg1 as _;

        console_log!(
            "{:?} size={} offset={}",
            (*file).fname,
            amount,
            offset
        );

        let buf = slice::from_raw_parts(arg2 as *const u8, amount as usize);
        POOL.write(&*file, buf, offset as _).unwrap();

        SQLITE_OK
    }
    pub unsafe extern "C" fn truncate(arg1: *mut sqlite3_file, size: sqlite3_int64) -> c_int {
        let file: *mut FileHandle = arg1 as _;
        POOL.truncate(&*file, size as _).unwrap();
        console_log!("truncate {} to {}", (*file).fname, size);

        SQLITE_OK
    }
    pub unsafe extern "C" fn sync(arg1: *mut sqlite3_file, _flags: c_int) -> c_int {
        let file: *mut FileHandle = arg1 as _;

        console_log!("syncing {}", (*file).fname);
        POOL.flush(&*file).unwrap();

        SQLITE_OK
    }
    pub unsafe extern "C" fn file_size(
        arg1: *mut sqlite3_file,
        res_size: *mut sqlite3_int64,
    ) -> c_int {
        let file: *mut FileHandle = arg1 as _;

        *res_size = POOL.file_size(&(*file).fname).unwrap() as _;

        SQLITE_OK
    }

    // lock & unlock related
    pub unsafe extern "C" fn lock(arg1: *mut sqlite3_file, lock_type: c_int) -> c_int {
        let file: *mut FileHandle = arg1 as _;
        (*file).lock = lock_type;

        SQLITE_OK
    }
    pub unsafe extern "C" fn unlock(arg1: *mut sqlite3_file, lock_type: c_int) -> c_int {
        let file: *mut FileHandle = arg1 as _;
        (*file).lock = lock_type;

        SQLITE_OK
    }
    pub unsafe extern "C" fn check_reserved_lock(
        _: *mut sqlite3_file,
        res_out: *mut c_int,
    ) -> c_int {
        console_log!("TODO: check reserved lock");
        *res_out = 1;
        SQLITE_OK
    }

    pub extern "C" fn sector_size(_: *mut sqlite3_file) -> c_int {
        SECTOR_SIZE as i32
    }
    pub extern "C" fn file_control(_: *mut sqlite3_file, _op: c_int, _arg: *mut c_void) -> c_int {
        SQLITE_NOTFOUND
    }
    pub extern "C" fn device_characteristics(_: *mut sqlite3_file) -> c_int {
        SQLITE_IOCAP_UNDELETABLE_WHEN_OPEN | SQLITE_IOCAP_SAFE_APPEND
    }
}

static IO_METHODS: sqlite3_io_methods = sqlite3_io_methods {
    iVersion: 1,
    xClose: Some(io_methods::close),
    xRead: Some(io_methods::read),
    xWrite: Some(io_methods::write),
    xTruncate: Some(io_methods::truncate),
    xSync: Some(io_methods::sync),
    xFileSize: Some(io_methods::file_size),
    xLock: Some(io_methods::lock),
    xUnlock: Some(io_methods::unlock),
    xCheckReservedLock: Some(io_methods::check_reserved_lock),
    xFileControl: Some(io_methods::file_control),
    xSectorSize: Some(io_methods::sector_size),
    xDeviceCharacteristics: Some(io_methods::device_characteristics),
    /* Methods above are valid for version 1 */
    xShmMap: None,
    xShmLock: None,
    xShmBarrier: None,
    xShmUnmap: None,
    xFetch: None,
    xUnfetch: None,
};

// - vfs layer
mod opfs_vfs {
    use super::*;

    pub unsafe extern "C" fn open(
        _vfs: *mut sqlite3_vfs,
        fname: *const c_char,
        fobj: *mut sqlite3_file,
        flags: c_int,
        out_flags: *mut c_int,
    ) -> c_int {
        if fname.is_null() {
            return SQLITE_IOERR;
        }

        let name = CStr::from_ptr(fname).to_str().unwrap();

        let file = fobj as *mut FileHandle;

        if SQLITE_OPEN_CREATE & flags == SQLITE_OPEN_CREATE {
            console_log!("open create file: {}", name);

            let handle = POOL.get_or_create_file(name).unwrap();
            (*file).fname = name.to_string();
            (*file).lock = SQLITE_LOCK_NONE;
        } else {
            console_log!("open open file: {}", name);

            let handle = POOL.get_file_handle(name).unwrap();
            (*file).fname = name.to_string();
            (*file).lock = SQLITE_LOCK_NONE;
        }
        (*file).flags = flags; // save open flags
        (*file)._super.pMethods = &IO_METHODS;

        *out_flags = flags;

        SQLITE_OK
    }
    pub unsafe extern "C" fn delete(
        _: *mut sqlite3_vfs,
        fname: *const c_char,
        _sync_dir: c_int,
    ) -> c_int {
        let name = CStr::from_ptr(fname).to_str().unwrap();
        console_log!("xDelete {:?}", name);

        POOL.delete(name).unwrap();

        SQLITE_OK
    }
    /// Query the file-system to see if the named file exists, is readable or
    /// is both readable and writable.
    /// #define SQLITE_ACCESS_EXISTS    0
    /// #define SQLITE_ACCESS_READWRITE 1   /* Used by PRAGMA temp_store_directory */
    /// #define SQLITE_ACCESS_READ      2   /* Unused */
    pub unsafe extern "C" fn access(
        _: *mut sqlite3_vfs,
        fname: *const c_char,
        _flags: c_int,
        res_out: *mut c_int,
    ) -> c_int {
        let name = CStr::from_ptr(fname).to_str().unwrap();

        let exists = POOL.has_file(name);
        *res_out = if exists { 1 } else { 0 };
        // console_log!("xAccess(exists) {:?} ret={}", name, *res_out);
        SQLITE_OK
    }

    pub unsafe extern "C" fn fullpathname(
        _: *mut sqlite3_vfs,
        zName: *const c_char,
        out: c_int,
        zOut: *mut c_char,
    ) -> c_int {
        let name = CStr::from_ptr(zName).to_str().unwrap();

        console_log!("xFullpathname: {:?}", name);

        let n = name.len();
        if n > out as usize {
            return SQLITE_CANTOPEN;
        }

        ptr::copy_nonoverlapping(zName, zOut, name.len() + 1);
        zOut.offset(out as isize).write(0);

        SQLITE_OK
    }

    pub unsafe extern "C" fn randomness(_: *mut sqlite3_vfs, n: c_int, out: *mut c_char) -> c_int {
        let buf = slice::from_raw_parts_mut(out as *mut u8, n as usize);
        getrandom::getrandom(buf).unwrap();
        SQLITE_OK
    }

    pub unsafe extern "C" fn sleep(_: *mut sqlite3_vfs, microseconds: c_int) -> c_int {
        console_log!("sleep {} microseconds", microseconds);

        let duration = std::time::Duration::from_micros(microseconds as u64);
        std::thread::sleep(duration);

        SQLITE_OK
    }

    pub unsafe extern "C" fn currenttime(_: *mut sqlite3_vfs, arg2: *mut f64) -> c_int {
        console_log!("currenttime");

        let t = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs_f64();

        *arg2 = t;

        SQLITE_OK
    }

    pub unsafe extern "C" fn get_last_error(
        _: *mut sqlite3_vfs,
        _len: c_int,
        _buf: *mut c_char,
    ) -> c_int {
        unimplemented!("get_last_error");
        SQLITE_OK
    }
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
        xOpen: Some(opfs_vfs::open),
        xDelete: Some(opfs_vfs::delete),
        xAccess: Some(opfs_vfs::access),
        xFullPathname: Some(opfs_vfs::fullpathname),
        // run-time extension support
        xDlOpen: None,
        xDlError: None,
        xDlSym: None,
        xDlClose: None,
        xRandomness: Some(opfs_vfs::randomness),
        xSleep: Some(opfs_vfs::sleep),
        xCurrentTime: Some(opfs_vfs::currenttime),
        xGetLastError: Some(opfs_vfs::get_last_error),
        // The methods above are in version 1 of the sqlite_vfs object definition
        xCurrentTimeInt64: None,
        xSetSystemCall: None,
        xGetSystemCall: None,
        xNextSystemCall: None,
    };

    // unsafe {
    //     sqlite3_config(SQLITE_CONFIG_LOG, Some(rust_puts));
    //}

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

#[wasm_bindgen]
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
    conn.execute(
        "INSERT INTO person (name, data) VALUES (?1, ?2)",
        params![me.name, me.data],
    )
    .unwrap();

    let mut stmt = conn.prepare("SELECT id, name, data FROM person").unwrap();
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

pub fn dummy_create() -> Result<(), JsValue> {
    unsafe {
        let filename = CString::new("test.db").unwrap();
        let mut db = ptr::null_mut();
        // let mut stmt = ptr::null_mut();

        let ret = sqlite3_open_v2(
            filename.as_ptr(),
            &mut db,
            SQLITE_OPEN_READWRITE | SQLITE_OPEN_CREATE | SQLITE_OPEN_SHAREDCACHE,
            ptr::null_mut(),
        );
        console_log!("=> open db ret {} db={:p}", ret, db);

        // IF NOT EXISTS
        let sql = b"CREATE TABLE IF NOT EXISTS demo (id INTEGER, name TEXT)\0";

        /* // exec
        let mut err_msg = ptr::null_mut();
        extern "C" fn callback(
            _context: *mut c_void,
            ncols: c_int,
            cols: *mut *mut c_char,
            col_names: *mut *mut c_char,
        ) -> c_int {
            console_log!("callback: ncols={}", ncols);
            0
        }
        let rc = sqlite3_exec(db, sql.as_ptr(), Some(callback), ptr::null_mut(), &mut err_msg);
        let err_msg = CStr::from_ptr(err_msg).to_str();

        console_log!("=> exec ret {} err_msg={:?}", rc, err_msg);
        */

        let mut stmt = ptr::null_mut();
        let rc = sqlite3_prepare_v2(db, sql.as_ptr() as *const _, -1, &mut stmt, ptr::null_mut());
        assert_eq!(rc, SQLITE_OK);

        console_log!("================================== => begin step - create table");
        let ret = sqlite3_step(stmt);
        if ret == SQLITE_DONE || ret == SQLITE_OK {
            console_log!("=> step done");
        } else {
            console_log!("====== => step ret {}", ret);
            let s = sqlite3_errmsg(db);
            let s = CStr::from_ptr(s).to_str().unwrap();
            console_log!("ERROR: {}", s);
            panic!();
        }

        let ret = sqlite3_finalize(stmt);
        console_log!("=> finalize ret {}", ret);

        console_log!("=======================================================");

        let mut stmt = ptr::null_mut();
        let sql = CString::new("INSERT INTO demo (name) VALUES ('hello world')").unwrap();
        let ret = sqlite3_prepare_v2(db, sql.as_ptr(), -1, &mut stmt, ptr::null_mut());
        console_log!("=> prepare ret {}", ret);
        if ret != SQLITE_OK {
            console_log!("=> step ret {}", ret);
            let s = sqlite3_errmsg(db);
            let s = CStr::from_ptr(s).to_str().unwrap();
            console_log!("ERROR: {}", s);
            panic!();
        }
        let ret = sqlite3_step(stmt);
        if ret != SQLITE_OK {
            console_log!("=> step ret {}", ret);
            let s = sqlite3_errmsg(db);
            let s = CStr::from_ptr(s).to_str().unwrap();
            console_log!("ERROR: {}", s);
            panic!();
        }

        let ret = sqlite3_finalize(stmt);
        console_log!("=> finalize ret {}", ret);

        console_log!("=======================================================");

        let ret = sqlite3_close_v2(db);
        console_log!("=> close db {}", ret);
    }

    Ok(())
}
