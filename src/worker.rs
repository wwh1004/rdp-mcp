//! One FreeRDP runtime per child process. Only worker mode calls the C ABI.
use std::{
    ffi::{CString, c_char, c_void},
    fs::File,
    io::{self, BufRead, BufReader, Read, Write},
    process::{ChildStdin, Command, Stdio},
    sync::{Arc, Mutex, mpsc},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

// CONNECTED originates in C's PostConnect, before initialization has finished.
// READY is emitted only after the synchronous connect command has succeeded.
pub(crate) const MSG_READY: u32 = 0;
pub(crate) const MSG_CONNECTED: u32 = 1;
pub(crate) const MSG_BITMAP_UPDATE: u32 = 2;
pub(crate) const MSG_DISCONNECTED: u32 = 3;
pub(crate) const MSG_RESIZED: u32 = 4;
pub(crate) const MSG_ERROR: u32 = 0xff;
const MAX_PAYLOAD: usize = 8192 * 8192 * 4 + 8;
const MAX_COMMAND: usize = 1024 * 1024;
const CLOSE_TIMEOUT: Duration = Duration::from_secs(3);
const PARENT_PID: &str = "RDP_MCP_PARENT_PID";

type EventHandler = Arc<dyn Fn(u32, &[u8], &[u8]) + Send + Sync>;
type OutputCallback =
    unsafe extern "C" fn(u32, *const c_void, u32, *const c_void, u32, *mut c_void);

#[cfg(not(test))]
mod ffi {
    use super::*;
    unsafe extern "C" {
        #[link_name = "rdp_mcp_native_initialize"]
        pub fn initialize(callback: OutputCallback, user_data: *mut c_void) -> i32;
        #[link_name = "rdp_mcp_native_command"]
        pub fn command(json_command: *const c_char) -> i32;
        #[link_name = "rdp_mcp_native_shutdown"]
        pub fn shutdown();
        #[link_name = "rdp_mcp_native_abi_version"]
        pub fn abi_version() -> u32;
    }
}

#[cfg(test)]
mod ffi {
    use super::*;
    pub unsafe fn initialize(_: OutputCallback, _: *mut c_void) -> i32 {
        0
    }
    pub unsafe fn command(_: *const c_char) -> i32 {
        0
    }
    pub unsafe fn shutdown() {}
    pub unsafe fn abi_version() -> u32 {
        1
    }
}

enum Control {
    Close,
    Stop,
}

pub(crate) struct WorkerProcess {
    input: Mutex<Option<ChildStdin>>,
    control: mpsc::Sender<Control>,
    supervisor: Mutex<Option<JoinHandle<()>>>,
}

impl WorkerProcess {
    pub(crate) fn spawn(handler: EventHandler) -> Result<Self> {
        #[cfg(not(test))]
        let mut command = {
            let mut command = Command::new(std::env::current_exe()?);
            command.arg("--worker");
            command
        };
        #[cfg(test)]
        let mut command = {
            let python = if cfg!(windows) { "python" } else { "python3" };
            let mut command = Command::new(python);
            command.arg(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/worker_fixture.py"
            ));
            command
        };
        command.env(PARENT_PID, std::process::id().to_string());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .context("could not start RDP worker")?;
        let input = child.stdin.take().expect("piped worker stdin");
        let mut output = child.stdout.take().expect("piped worker stdout");
        let (control, commands) = mpsc::channel();
        let reader_control = control.clone();
        let reader_handler = handler.clone();
        let reader = thread::spawn(move || {
            let result = read_events(&mut output, &reader_handler, &reader_control);
            if let Err(error) = result {
                reader_handler(
                    MSG_ERROR,
                    &error_payload(&format!("worker stream: {error}")),
                    &[],
                );
            }
            let _ = reader_control.send(Control::Stop);
        });
        let supervisor = thread::spawn(move || {
            let mut deadline = None;
            let status = loop {
                match child.try_wait() {
                    Ok(Some(status)) => break Ok(status),
                    Err(error) => {
                        let _ = child.kill();
                        let _ = child.wait();
                        break Err(error);
                    }
                    Ok(None) => {}
                }
                if deadline.is_some_and(|end| Instant::now() >= end) {
                    let _ = child.kill();
                    break child.wait();
                }
                match commands.recv_timeout(Duration::from_millis(20)) {
                    Ok(Control::Close) => {
                        deadline.get_or_insert(Instant::now() + CLOSE_TIMEOUT);
                    }
                    Ok(Control::Stop) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                        let _ = child.kill();
                        break child.wait();
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                }
            };
            // Drain the last events before publishing the terminal status.
            let _ = reader.join();
            let error = match status {
                Ok(status) if status.success() => None,
                Ok(status) => Some(format!("RDP worker exited with {status}")),
                Err(error) => Some(format!("could not wait for RDP worker: {error}")),
            };
            handler(
                MSG_DISCONNECTED,
                &serde_json::to_vec(&json!({"error": error})).unwrap(),
                &[],
            );
        });
        Ok(Self {
            input: Mutex::new(Some(input)),
            control,
            supervisor: Mutex::new(Some(supervisor)),
        })
    }

    pub(crate) fn send(&self, command: &Value) -> Result<()> {
        let mut bytes = serde_json::to_vec(command)?;
        if bytes.len() >= MAX_COMMAND {
            bail!("worker command is too large");
        }
        bytes.push(b'\n');
        let mut input = self.input.lock().expect("worker input poisoned");
        input
            .as_mut()
            .context("RDP worker is closed")?
            .write_all(&bytes)
            .context("could not write to RDP worker")
    }

    pub(crate) fn shutdown(&self) {
        // Start the deadline BEFORE writing. A blocked pipe write is released
        // when the independent supervisor terminates an unresponsive child.
        let _ = self.control.send(Control::Close);
        if let Ok(mut input) = self.input.try_lock()
            && let Some(input) = input.as_mut()
        {
            let _ = input.write_all(b"{\"type\":\"disconnect\"}\n");
        }
        let mut supervisor = self.supervisor.lock().expect("worker supervisor poisoned");
        if let Some(thread) = supervisor.take() {
            let _ = thread.join();
        }
        self.input.lock().expect("worker input poisoned").take();
    }
}

impl Drop for WorkerProcess {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn read_events(
    reader: &mut impl Read,
    handler: &EventHandler,
    control: &mpsc::Sender<Control>,
) -> io::Result<()> {
    let mut payload = Vec::new();
    while let Some(kind) = read_message(reader, &mut payload)? {
        if kind == MSG_BITMAP_UPDATE {
            if payload.len() < 8 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "short bitmap header",
                ));
            }
            handler(kind, &payload[..8], &payload[8..]);
        } else {
            handler(kind, &payload, &[]);
        }
        if kind == MSG_DISCONNECTED {
            let _ = control.send(Control::Stop);
        } else if kind == MSG_ERROR {
            let _ = control.send(Control::Close);
        }
    }
    Ok(())
}

fn read_message(reader: &mut impl Read, payload: &mut Vec<u8>) -> io::Result<Option<u32>> {
    let mut header = [0; 8];
    loop {
        match reader.read(&mut header[..1]) {
            Ok(0) => return Ok(None),
            Ok(_) => break,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
    reader.read_exact(&mut header[1..])?;
    let kind = u32::from_le_bytes(header[..4].try_into().unwrap());
    let length = u32::from_le_bytes(header[4..].try_into().unwrap()) as usize;
    if length > MAX_PAYLOAD {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "worker message exceeds maximum desktop size",
        ));
    }
    payload.resize(length, 0);
    reader.read_exact(payload)?;
    Ok(Some(kind))
}

fn write_message(writer: &mut impl Write, kind: u32, a: &[u8], b: &[u8]) -> io::Result<()> {
    let length = a
        .len()
        .checked_add(b.len())
        .filter(|&n| n <= MAX_PAYLOAD)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "worker message too large"))?;
    writer.write_all(&kind.to_le_bytes())?;
    writer.write_all(&(length as u32).to_le_bytes())?;
    writer.write_all(a)?;
    writer.write_all(b)?;
    writer.flush()
}

fn error_payload(message: &str) -> Vec<u8> {
    serde_json::to_vec(&json!({"message": message})).unwrap()
}

struct Output(Mutex<File>);
impl Output {
    fn send(&self, kind: u32, a: &[u8], b: &[u8]) {
        if write_message(
            &mut *self.0.lock().expect("worker output poisoned"),
            kind,
            a,
            b,
        )
        .is_err()
        {
            // The parent can no longer receive events. Exit even if a native
            // thread is blocked; the OS releases this worker's RDP resources.
            std::process::exit(1);
        }
    }
}

unsafe extern "C" fn native_output(
    kind: u32,
    a: *const c_void,
    a_len: u32,
    b: *const c_void,
    b_len: u32,
    data: *mut c_void,
) {
    // SAFETY: run() keeps Output alive until native shutdown has joined all callbacks.
    let output = unsafe { &*data.cast::<Output>() };
    // SAFETY: C borrows these buffers for this callback; all bytes are written before returning.
    let a = if a_len == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(a.cast::<u8>(), a_len as usize) }
    };
    let b = if b_len == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(b.cast::<u8>(), b_len as usize) }
    };
    output.send(kind, a, b);
}

pub(crate) fn run() -> Result<()> {
    let output = Box::new(Output(Mutex::new(platform::protocol_output()?)));
    if let Some(parent) = std::env::var_os(PARENT_PID) {
        let parent: u32 = parent
            .to_string_lossy()
            .parse()
            .context("invalid worker parent PID")?;
        platform::watch_parent(parent)?;
    }
    // SAFETY: pure query; this process owns exactly one native runtime.
    if unsafe { ffi::abi_version() } != 1 {
        bail!("unsupported native ABI version");
    }
    // SAFETY: output's allocation is stable and lives beyond NativeRuntime below.
    let result =
        unsafe { ffi::initialize(native_output, (&*output as *const Output).cast_mut().cast()) };
    if result != 0 {
        bail!("native initialization failed with code {result}");
    }
    struct NativeRuntime;
    impl Drop for NativeRuntime {
        fn drop(&mut self) {
            // SAFETY: only this worker thread starts or shuts down the native runtime.
            unsafe { ffi::shutdown() };
        }
    }
    let _runtime = NativeRuntime;
    let mut input = BufReader::new(io::stdin());
    let mut line = Vec::new();
    let mut connected = false;
    loop {
        line.clear();
        let count = input
            .by_ref()
            .take(MAX_COMMAND as u64 + 1)
            .read_until(b'\n', &mut line)?;
        if count == 0 {
            return Ok(());
        }
        if count > MAX_COMMAND || !line.ends_with(b"\n") {
            bail!("invalid worker command length or truncated command");
        }
        let value: Value = serde_json::from_slice(&line)
            .map_err(|_| anyhow::anyhow!("invalid worker JSON command"))?;
        let kind = value["type"]
            .as_str()
            .context("worker command has no type")?;
        if kind == "disconnect" {
            return Ok(());
        }
        if (connected && kind == "connect") || (!connected && kind != "connect") {
            bail!("worker requires exactly one connect command before input");
        }
        let command = CString::new(line.as_slice()).context("worker command contains NUL")?;
        // SAFETY: CString is valid throughout the synchronous native call.
        let result = unsafe { ffi::command(command.as_ptr()) };
        if result != 0 {
            output.send(
                MSG_ERROR,
                &error_payload(&format!("native command failed with code {result}")),
                &[],
            );
            bail!("native command failed with code {result}");
        }
        if kind == "connect" {
            connected = true;
            output.send(MSG_READY, &[], &[]);
        }
    }
}

#[cfg(unix)]
mod platform {
    use super::*;
    use std::os::fd::FromRawFd;
    pub fn protocol_output() -> io::Result<File> {
        // SAFETY: duplicate stdout for protocol bytes, then redirect process stdout
        // to stderr before FreeRDP initializes its logging system.
        let fd = unsafe { libc::dup(libc::STDOUT_FILENO) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let file = unsafe { File::from_raw_fd(fd) };
        if unsafe { libc::dup2(libc::STDERR_FILENO, libc::STDOUT_FILENO) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(file)
    }
    pub fn watch_parent(parent: u32) -> io::Result<()> {
        thread::Builder::new()
            .name("worker-parent".into())
            .spawn(move || {
                loop {
                    // SAFETY: getppid has no arguments or mutable state. Reparenting also
                    // catches parent death while freerdp_connect/shutdown is blocked.
                    if unsafe { libc::getppid() } as u32 != parent {
                        std::process::exit(0);
                    }
                    thread::sleep(Duration::from_millis(100));
                }
            })?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fragmented<R>(R);
    impl<R: Read> Read for Fragmented<R> {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            let length = buffer.len().min(3);
            self.0.read(&mut buffer[..length])
        }
    }

    #[test]
    fn protocol_handles_fragmented_and_consecutive_messages() {
        let mut wire = Vec::new();
        let bitmap = [0, 0, 0, 0, 1, 0, 1, 0];
        write_message(&mut wire, MSG_BITMAP_UPDATE, &bitmap, &[10, 20, 30, 255]).unwrap();
        write_message(&mut wire, MSG_READY, &[], &[]).unwrap();
        let mut reader = Fragmented(io::Cursor::new(wire));
        let mut payload = Vec::new();
        assert_eq!(
            read_message(&mut reader, &mut payload).unwrap(),
            Some(MSG_BITMAP_UPDATE)
        );
        assert_eq!(&payload[..8], &bitmap);
        assert_eq!(&payload[8..], &[10, 20, 30, 255]);
        assert_eq!(
            read_message(&mut reader, &mut payload).unwrap(),
            Some(MSG_READY)
        );
        assert!(payload.is_empty());
        assert_eq!(read_message(&mut reader, &mut payload).unwrap(), None);
    }

    #[test]
    fn protocol_rejects_truncation_and_oversized_lengths() {
        let mut payload = Vec::new();
        assert!(read_message(&mut &b"abc"[..], &mut payload).is_err());
        let mut wire = Vec::new();
        write_message(&mut wire, MSG_ERROR, b"abc", &[]).unwrap();
        wire.pop();
        assert!(read_message(&mut wire.as_slice(), &mut payload).is_err());
        let wire = [MSG_BITMAP_UPDATE.to_le_bytes(), u32::MAX.to_le_bytes()].concat();
        payload.clear();
        assert!(read_message(&mut wire.as_slice(), &mut payload).is_err());
        assert!(payload.is_empty());
    }
}

#[cfg(windows)]
mod platform {
    use super::*;
    use std::os::windows::io::FromRawHandle;
    use windows_sys::Win32::{
        Foundation::{CloseHandle, DUPLICATE_SAME_ACCESS, DuplicateHandle, INVALID_HANDLE_VALUE},
        System::{
            Console::{GetStdHandle, STD_ERROR_HANDLE, STD_OUTPUT_HANDLE, SetStdHandle},
            Threading::{
                GetCurrentProcess, INFINITE, OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject,
            },
        },
    };
    unsafe extern "C" {
        fn _dup2(source: i32, target: i32) -> i32;
    }
    pub fn protocol_output() -> io::Result<File> {
        // SAFETY: duplicate the inherited pipe into an owned File, then redirect
        // BOTH the C runtime descriptor and Win32 stdout used by Rust logging.
        unsafe {
            let stdout = GetStdHandle(STD_OUTPUT_HANDLE);
            if stdout.is_null() || stdout == INVALID_HANDLE_VALUE {
                return Err(io::Error::last_os_error());
            }
            let process = GetCurrentProcess();
            let mut duplicate = std::ptr::null_mut();
            if DuplicateHandle(
                process,
                stdout,
                process,
                &mut duplicate,
                0,
                0,
                DUPLICATE_SAME_ACCESS,
            ) == 0
            {
                return Err(io::Error::last_os_error());
            }
            let file = File::from_raw_handle(duplicate);
            if _dup2(2, 1) != 0 {
                return Err(io::Error::last_os_error());
            }
            if SetStdHandle(STD_OUTPUT_HANDLE, GetStdHandle(STD_ERROR_HANDLE)) == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(file)
        }
    }
    pub fn watch_parent(parent: u32) -> io::Result<()> {
        // Open inside the thread so the raw handle never crosses threads.
        thread::Builder::new()
            .name("worker-parent".into())
            .spawn(move || {
                // SAFETY: wait-only access to the parent; a missing parent means it
                // already exited. This watcher stays independent of native calls.
                unsafe {
                    let process = OpenProcess(PROCESS_SYNCHRONIZE, 0, parent);
                    if !process.is_null() {
                        WaitForSingleObject(process, INFINITE);
                        CloseHandle(process);
                    }
                }
                std::process::exit(0);
            })?;
        Ok(())
    }
}
