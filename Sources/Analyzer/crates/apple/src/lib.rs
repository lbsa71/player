//! Narrow JSON C ABI for Apple clients. Invoke on a worker, never an audio callback.
use mixlib_catalog::Store;
use mixlib_types::*;
use std::ffi::{c_char, CStr, CString};

/// Returns an owned UTF-8 JSON catalogue snapshot, or a JSON diagnostic.
/// # Safety
/// `library_path` must be a valid, NUL-terminated UTF-8 string for this call.
#[no_mangle]
pub unsafe extern "C" fn mixlib_catalog_json(library_path: *const c_char) -> *mut c_char {
    let response = std::panic::catch_unwind(|| -> Result<serde_json::Value> {
        require(!library_path.is_null(), "FFI_INPUT", "null library path")?;
        let path = unsafe { CStr::from_ptr(library_path) }
            .to_str()
            .map_err(|e| Error::new("UTF8", e.to_string()))?;
        let store = Store::open(std::path::Path::new(path))?;
        Ok(serde_json::json!({"schema_version":1,"tracks":store.tracks()?}))
    });
    let value = match response {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => serde_json::json!({"error":e}),
        Err(_) => serde_json::json!({"error":{"code":"PANIC","message":"core operation failed"}}),
    };
    CString::new(value.to_string()).unwrap().into_raw()
}
/// Release a string returned by mixlib_catalog_json, exactly once.
/// # Safety
/// `value` must be null or an owned, unmodified pointer returned by this library.
#[no_mangle]
pub unsafe extern "C" fn mixlib_string_free(value: *mut c_char) {
    if !value.is_null() {
        drop(unsafe { CString::from_raw(value) });
    }
}
