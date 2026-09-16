//! Native SDK ABI 1.1 from Cua Driver 0.28.0 (cua_driver_abi.h).
//! Only the host/protocol boundary lives here; Cua implements every tool.

use std::ffi::c_void;
use std::mem::ManuallyDrop;
use std::path::Path;
use std::ptr;
use std::sync::{Arc, mpsc};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use libloading::Library;
use serde_json::Value;

type Handle = *mut c_void;
type Status = i32;
type Free = unsafe extern "C" fn(*mut Buffer);
type Completion = unsafe extern "C" fn(*mut c_void, Status, Buffer, Buffer);
type Invoke = unsafe extern "C" fn(
    Handle,
    *const u8,
    usize,
    *const u8,
    usize,
    Completion,
    *mut c_void,
    *mut Handle,
    *mut Buffer,
) -> Status;
type Shutdown =
    unsafe extern "C" fn(Handle, Completion, *mut c_void, *mut Handle, *mut Buffer) -> Status;

#[repr(C)]
#[derive(Default)]
struct Buffer {
    data: *mut u8,
    len: usize,
    capacity: usize,
}

#[repr(C)]
#[derive(Default)]
struct Version {
    struct_size: u32,
    major: u16,
    minor: u16,
    patch: u16,
    reserved: u16,
}

struct Api {
    // Cua owns process-global UI and executor threads. Keep its code mapped
    // until process exit, including after the last session shuts down.
    _library: ManuallyDrop<Library>,
    free: Free,
    destroy: unsafe extern "C" fn(*mut Handle),
    list: unsafe extern "C" fn(Handle, *mut Buffer, *mut Buffer) -> Status,
    invoke: Invoke,
    shutdown: Shutdown,
    cancel: unsafe extern "C" fn(Handle),
    release: unsafe extern "C" fn(*mut Handle),
}

pub struct Driver {
    api: Arc<Api>,
    handle: Handle,
    shutdown: bool,
}

impl Driver {
    pub fn load(path: &Path, cursor_enabled: bool) -> Result<Self> {
        // Load only an absolute, packaged path, never a library from PATH/cwd.
        let path = path
            .canonicalize()
            .context("Cua Driver SDK is missing from this Waku build")?;
        unsafe {
            let library = Library::new(&path)
                .with_context(|| format!("load Cua Driver SDK {}", path.display()))?;
            let version_fn = *library.get::<unsafe extern "C" fn(*mut Version) -> Status>(
                b"cua_driver_abi_version_v1",
            )?;
            let compatible = *library.get::<unsafe extern "C" fn(u16, u16) -> bool>(
                b"cua_driver_abi_is_compatible_v1",
            )?;
            let mut version = Version {
                struct_size: size_of::<Version>() as u32,
                ..Version::default()
            };
            if version_fn(&mut version) != 0 || !compatible(1, 1) {
                bail!("Cua Driver SDK ABI 1.1 is required");
            }
            let create = *library.get::<unsafe extern "C" fn(
                bool,
                *const u8,
                usize,
                *mut Handle,
                *mut Buffer,
            ) -> Status>(b"waku_cua_driver_create_v1")?;
            let api = Arc::new(Api {
                free: *library.get(b"cua_driver_buffer_free_v1")?,
                destroy: *library.get(b"cua_driver_destroy_v1")?,
                list: *library.get(b"cua_driver_list_tools_json_v1")?,
                invoke: *library.get(b"cua_driver_invoke_v1")?,
                shutdown: *library.get(b"cua_driver_shutdown_v1")?,
                cancel: *library.get(b"cua_driver_operation_cancel_v1")?,
                release: *library.get(b"cua_driver_operation_release_v1")?,
                _library: ManuallyDrop::new(library),
            });
            let mut handle = ptr::null_mut();
            let mut error = Buffer::default();
            let status = create(cursor_enabled, ptr::null(), 0, &mut handle, &mut error);
            check(status, take_buffer(&api, &mut error))?;
            Ok(Self {
                api,
                handle,
                shutdown: false,
            })
        }
    }

    pub fn list_tools(&self) -> Result<Value> {
        let mut output = Buffer::default();
        let mut error = Buffer::default();
        let status = unsafe { (self.api.list)(self.handle, &mut output, &mut error) };
        let data = take_buffer(&self.api, &mut output);
        check(status, take_buffer(&self.api, &mut error))?;
        Ok(serde_json::from_slice(&data)?)
    }

    pub fn call(
        &self,
        name: &str,
        arguments: &Value,
        cancelled: impl Fn() -> bool,
    ) -> Result<Value> {
        let arguments = serde_json::to_vec(arguments)?;
        self.operation(
            |context, operation, error| unsafe {
                (self.api.invoke)(
                    self.handle,
                    name.as_ptr(),
                    name.len(),
                    arguments.as_ptr(),
                    arguments.len(),
                    complete,
                    context,
                    operation,
                    error,
                )
            },
            cancelled,
        )
    }

    pub fn shutdown(&mut self) -> Result<()> {
        if self.shutdown {
            return Ok(());
        }
        self.operation(
            |context, operation, error| unsafe {
                (self.api.shutdown)(self.handle, complete, context, operation, error)
            },
            || false,
        )?;
        self.shutdown = true;
        Ok(())
    }

    fn operation(
        &self,
        start: impl FnOnce(*mut c_void, *mut Handle, *mut Buffer) -> Status,
        cancelled: impl Fn() -> bool,
    ) -> Result<Value> {
        if cancelled() {
            bail!("Computer Use stopped before the action started");
        }
        let (tx, rx) = mpsc::channel();
        let context = Box::into_raw(Box::new(Callback {
            api: self.api.clone(),
            tx,
        }))
        .cast();
        let mut operation = ptr::null_mut();
        let mut error = Buffer::default();
        let status = start(context, &mut operation, &mut error);
        let error = take_buffer(&self.api, &mut error);
        if status != 0 {
            // A rejected admission never invokes the completion callback.
            unsafe {
                drop(Box::from_raw(context.cast::<Callback>()));
            }
            check(status, error)?;
        }
        let mut was_cancelled = false;
        let result = loop {
            match rx.recv_timeout(Duration::from_millis(50)) {
                Ok(result) => break result,
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    break Err(anyhow::anyhow!("Cua Driver completion channel closed"));
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if !was_cancelled && cancelled() {
                        unsafe {
                            (self.api.cancel)(operation);
                        }
                        was_cancelled = true;
                    }
                }
            }
        };
        unsafe {
            (self.api.release)(&mut operation);
        }
        if was_cancelled {
            bail!(
                "Computer Use stopped; action completion is unknown. Inspect fresh state before retrying."
            );
        }
        Ok(serde_json::from_slice(&result?)?)
    }
}

impl Drop for Driver {
    fn drop(&mut self) {
        let _ = self.shutdown();
        unsafe {
            (self.api.destroy)(&mut self.handle);
        }
    }
}

struct Callback {
    api: Arc<Api>,
    tx: mpsc::Sender<Result<Vec<u8>>>,
}

unsafe extern "C" fn complete(
    context: *mut c_void,
    status: Status,
    mut result: Buffer,
    mut error: Buffer,
) {
    let callback = unsafe { Box::from_raw(context.cast::<Callback>()) };
    let result = take_buffer(&callback.api, &mut result);
    let error = take_buffer(&callback.api, &mut error);
    let _ = callback.tx.send(check(status, error).map(|()| result));
}

fn take_buffer(api: &Api, buffer: &mut Buffer) -> Vec<u8> {
    let data = if buffer.data.is_null() || buffer.len == 0 {
        Vec::new()
    } else {
        unsafe { std::slice::from_raw_parts(buffer.data, buffer.len).to_vec() }
    };
    unsafe {
        (api.free)(buffer);
    }
    data
}

fn check(status: Status, error: Vec<u8>) -> Result<()> {
    if status != 0 {
        bail!(
            "Cua Driver failed ({status}): {}",
            String::from_utf8_lossy(&error)
        );
    }
    Ok(())
}
