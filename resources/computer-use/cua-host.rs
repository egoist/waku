// Included in the pinned SDK's ABI module when building Waku's native host.
// Extend host initialization only: all tools, authorization and rendering stay
// in Cua. Language-SDK create() intentionally has no native cursor facility.
#[no_mangle]
pub unsafe extern "C" fn waku_cua_driver_create_v1(
    cursor_enabled: bool,
    options_json: *const u8,
    options_len: usize,
    out_handle: *mut *mut CuaDriverHandle,
    out_error: *mut CuaDriverBuffer,
) -> CuaDriverStatus {
    with_ffi_guard(out_error, || {
        let out_handle = out_handle.as_mut().ok_or_else(|| {
            AbiFailure::new(CuaDriverStatus::NullPointer, "out_handle must not be null")
        })?;
        *out_handle = ptr::null_mut();
        let bytes = input_bytes(options_json, options_len)?;
        let options = if bytes.is_empty() {
            AbiDriverOptions::default()
        } else {
            serde_json::from_slice(bytes).map_err(|error| {
                AbiFailure::new(CuaDriverStatus::InvalidArgument, error.to_string())
            })?
        };
        let mut options = runtime_options_from_abi(options)?;
        options.cursor.enabled = cursor_enabled;
        let runtime = DriverRuntime::create(options).map_err(runtime_create_failure)?;
        *out_handle = Box::into_raw(Box::new(CuaDriverHandle { runtime }));
        Ok(())
    })
}

#[no_mangle]
pub extern "C" fn waku_cua_driver_run_cursor_v1() {
    // Windows/Linux start their native overlay thread during registration.
    // AppKit requires the actual OS main thread, owned by Waku's signed host.
    #[cfg(target_os = "macos")]
    if platform_macos::session::has_graphic_access() {
        platform_macos::cursor::overlay::run_on_main_thread();
    }
}
