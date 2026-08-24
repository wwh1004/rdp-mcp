use std::{
    ffi::{CString, c_char, c_void},
    slice,
    sync::{Condvar, Mutex},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::{
    image::{self, EncodedImage, NativeRegion},
    keyboard::{KeyStroke, key_stroke, modifier},
};

const MSG_CONNECTED: u32 = 0x01;
const MSG_BITMAP_UPDATE: u32 = 0x02;
const MSG_DISCONNECTED: u32 = 0x03;
const MSG_RESIZED: u32 = 0x04;
const MSG_ERROR: u32 = 0xff;

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
        #[link_name = "rdp_mcp_native_is_connected"]
        pub fn is_connected() -> i32;
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
    pub unsafe fn is_connected() -> i32 {
        0
    }
    pub unsafe fn abi_version() -> u32 {
        1
    }
}

#[derive(Debug, Clone)]
pub struct ConnectionInfo {
    pub id: String,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub status: &'static str,
}

#[derive(Debug, Clone, Copy)]
pub struct ScreenshotRegion {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

pub struct Screenshot {
    pub encoded: EncodedImage,
    pub native_width: u32,
    pub native_height: u32,
    pub frame_version: u64,
}

#[derive(Default)]
struct SessionState {
    connected: bool,
    disconnected: bool,
    width: u32,
    height: u32,
    framebuffer: Vec<u8>,
    frame_version: u64,
    scale_x: f64,
    scale_y: f64,
    error: Option<String>,
}

struct CallbackState {
    inner: Mutex<SessionState>,
    changed: Condvar,
}

impl CallbackState {
    fn new() -> Self {
        Self {
            inner: Mutex::new(SessionState {
                scale_x: 1.0,
                scale_y: 1.0,
                ..SessionState::default()
            }),
            changed: Condvar::new(),
        }
    }
}

struct NativeSession {
    id: String,
    name: String,
    host: String,
    port: u16,
    callback: Box<CallbackState>,
    initialized: bool,
}

impl NativeSession {
    fn connect(
        host: String,
        port: u16,
        username: String,
        password: String,
        name: Option<String>,
        width: u32,
        height: u32,
    ) -> Result<Self> {
        // SAFETY: this is a pure ABI version query with no pointer arguments.
        let abi_version = unsafe { ffi::abi_version() };
        if abi_version != 1 {
            bail!("unsupported native ABI version: {abi_version}");
        }

        let mut session = Self {
            id: Uuid::new_v4().to_string(),
            name: name.unwrap_or_else(|| format!("RDP {host}")),
            host: host.clone(),
            port,
            callback: Box::new(CallbackState::new()),
            initialized: false,
        };
        let user_data = (&mut *session.callback) as *mut CallbackState as *mut c_void;

        // SAFETY: callback and boxed user data remain valid until shutdown joins C threads.
        let result = unsafe { ffi::initialize(native_output_callback, user_data) };
        if result != 0 {
            bail!("native initialization failed with code {result}");
        }
        session.initialized = true;

        let connect = json!({
            "type": "connect",
            "config": {
                "host": host,
                "port": port,
                "username": username,
                "password": password,
                "domain": "",
                "width": width.clamp(200, 8192),
                "height": height.clamp(200, 8192),
                "enableNla": true,
                "skipCertVerification": true,
                "enableGfx": true,
                "enableH264": false,
                "desktopScaleFactor": 100,
                "deviceScaleFactor": 100,
                "enableClipboard": true,
                "drives": []
            }
        });

        if let Err(error) = session.command(&connect) {
            session.shutdown();
            return Err(error);
        }
        session.wait_connected(Duration::from_secs(30))?;
        Ok(session)
    }

    fn command(&self, command: &Value) -> Result<()> {
        let json = serde_json::to_string(command)?;
        let command = CString::new(json).context("native command contains a NUL byte")?;
        // SAFETY: CString remains valid for the synchronous native command call.
        let result = unsafe { ffi::command(command.as_ptr()) };
        if result != 0 {
            let detail = self
                .callback
                .inner
                .lock()
                .expect("session state poisoned")
                .error
                .clone();
            bail!(
                "native command failed with code {result}: {}",
                detail.unwrap_or_else(|| "no native error detail".into())
            );
        }
        Ok(())
    }

    fn wait_connected(&self, timeout: Duration) -> Result<()> {
        let deadline = Instant::now() + timeout;
        let mut state = self.callback.inner.lock().expect("session state poisoned");
        while !state.connected && state.error.is_none() && !state.disconnected {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                bail!(
                    "RDP connection timed out after {} seconds",
                    timeout.as_secs()
                );
            }
            let (next, wait) = self
                .callback
                .changed
                .wait_timeout(state, remaining)
                .expect("session state poisoned");
            state = next;
            if wait.timed_out() && !state.connected {
                bail!(
                    "RDP connection timed out after {} seconds",
                    timeout.as_secs()
                );
            }
        }
        if let Some(error) = &state.error {
            bail!("RDP connection failed: {error}");
        }
        if !state.connected {
            bail!("RDP connection closed before initialization completed");
        }
        Ok(())
    }

    fn info(&self) -> ConnectionInfo {
        let callback_connected = self
            .callback
            .inner
            .lock()
            .expect("session state poisoned")
            .connected;
        // SAFETY: this query has no pointer arguments and the session owns the runtime.
        let connected = callback_connected && unsafe { ffi::is_connected() != 0 };
        ConnectionInfo {
            id: self.id.clone(),
            name: self.name.clone(),
            host: self.host.clone(),
            port: self.port,
            status: if connected {
                "connected"
            } else {
                "disconnected"
            },
        }
    }

    fn dimensions(&self) -> Result<(u32, u32)> {
        let state = self.callback.inner.lock().expect("session state poisoned");
        if let Some(error) = &state.error {
            bail!("native RDP error: {error}");
        }
        if state.width == 0 || state.height == 0 {
            bail!("RDP desktop dimensions are not available yet");
        }
        Ok((state.width, state.height))
    }

    fn scale_to_native(&self, x: f64, y: f64) -> (i32, i32) {
        let state = self.callback.inner.lock().expect("session state poisoned");
        (
            (x * state.scale_x).round() as i32,
            (y * state.scale_y).round() as i32,
        )
    }

    fn screenshot(
        &self,
        format: &str,
        quality: u8,
        max_width: Option<u32>,
        region: Option<ScreenshotRegion>,
    ) -> Result<Screenshot> {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut state = self.callback.inner.lock().expect("session state poisoned");
        let waited_for_frame = state.frame_version == 0;
        while state.frame_version == 0 && state.error.is_none() && !state.disconnected {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                bail!("no RDP desktop frame was received within 10 seconds");
            }
            let (next, wait) = self
                .callback
                .changed
                .wait_timeout(state, remaining)
                .expect("session state poisoned");
            state = next;
            if wait.timed_out() && state.frame_version == 0 {
                bail!("no RDP desktop frame was received within 10 seconds");
            }
        }
        if let Some(error) = &state.error {
            bail!("native RDP error: {error}");
        }
        drop(state);

        if waited_for_frame {
            thread::sleep(Duration::from_millis(500));
        }

        let mut state = self.callback.inner.lock().expect("session state poisoned");
        let native_width = state.width;
        let native_height = state.height;
        let frame_version = state.frame_version;
        let native_region = region.map(|region| NativeRegion {
            x: (region.x * state.scale_x).max(0.0).round() as u32,
            y: (region.y * state.scale_y).max(0.0).round() as u32,
            width: (region.width * state.scale_x).max(1.0).round() as u32,
            height: (region.height * state.scale_y).max(1.0).round() as u32,
        });
        let encoded = image::encode(
            &state.framebuffer,
            native_width,
            native_height,
            format,
            quality,
            max_width,
            native_region,
        )?;
        if region.is_none() {
            state.scale_x = native_width as f64 / encoded.width as f64;
            state.scale_y = native_height as f64 / encoded.height as f64;
        }
        Ok(Screenshot {
            encoded,
            native_width,
            native_height,
            frame_version,
        })
    }

    fn shutdown(&mut self) {
        if self.initialized {
            // SAFETY: shutdown joins all callback-producing native threads before return.
            unsafe { ffi::shutdown() };
            self.initialized = false;
        }
    }
}

impl Drop for NativeSession {
    fn drop(&mut self) {
        self.shutdown();
    }
}

pub struct NativeManager {
    session: Mutex<Option<NativeSession>>,
}

impl NativeManager {
    pub fn new() -> Self {
        Self {
            session: Mutex::new(None),
        }
    }

    pub fn list(&self) -> Vec<ConnectionInfo> {
        self.session
            .lock()
            .expect("manager state poisoned")
            .as_ref()
            .map(NativeSession::info)
            .into_iter()
            .collect()
    }

    #[allow(clippy::too_many_arguments)]
    pub fn open(
        &self,
        connection_type: &str,
        host: String,
        port: u16,
        username: Option<String>,
        password: Option<String>,
        credential_id: Option<String>,
        name: Option<String>,
        width: u32,
        height: u32,
    ) -> Result<ConnectionInfo> {
        if !connection_type.eq_ignore_ascii_case("rdp") {
            bail!("rdp-mcp only supports connection_type='rdp'");
        }
        if credential_id.is_some() {
            bail!("credential_id is unavailable because this standalone server has no vault");
        }
        let username = username.ok_or_else(|| anyhow::anyhow!("username is required"))?;
        let password = password.ok_or_else(|| anyhow::anyhow!("password is required"))?;

        let mut slot = self.session.lock().expect("manager state poisoned");
        if slot.is_some() {
            bail!("only one active RDP connection is supported per rdp-mcp process");
        }
        let session = NativeSession::connect(host, port, username, password, name, width, height)?;
        let info = session.info();
        *slot = Some(session);
        Ok(info)
    }

    pub fn close(&self, connection_id: &str) -> Result<()> {
        let mut slot = self.session.lock().expect("manager state poisoned");
        let mut session = slot
            .take()
            .ok_or_else(|| anyhow::anyhow!("connection not found: {connection_id}"))?;
        if session.id != connection_id {
            *slot = Some(session);
            bail!("connection not found: {connection_id}");
        }
        let _ = session.command(&json!({"type": "disconnect"}));
        session.shutdown();
        Ok(())
    }

    pub fn dimensions(&self, connection_id: &str) -> Result<(u32, u32)> {
        self.with_session(connection_id, NativeSession::dimensions)
    }

    pub fn screenshot(
        &self,
        connection_id: &str,
        format: &str,
        quality: u8,
        max_width: Option<u32>,
        region: Option<ScreenshotRegion>,
    ) -> Result<Screenshot> {
        self.with_session(connection_id, |session| {
            session.screenshot(format, quality, max_width, region)
        })
    }

    pub fn mouse_move(&self, connection_id: &str, x: f64, y: f64) -> Result<()> {
        self.with_session(connection_id, |session| {
            let (x, y) = session.scale_to_native(x, y);
            session.command(&json!({"type": "mouse_move", "x": x, "y": y}))
        })
    }

    pub fn mouse_click(
        &self,
        connection_id: &str,
        x: f64,
        y: f64,
        button: &str,
        double_click: bool,
    ) -> Result<()> {
        let button = mouse_button(button)?;
        self.with_session(connection_id, |session| {
            let (x, y) = session.scale_to_native(x, y);
            let count = if double_click { 2 } else { 1 };
            for index in 0..count {
                session.command(&json!({
                    "type": "mouse_button_down", "x": x, "y": y, "button": button
                }))?;
                session.command(&json!({
                    "type": "mouse_button_up", "x": x, "y": y, "button": button
                }))?;
                if index + 1 < count {
                    thread::sleep(Duration::from_millis(80));
                }
            }
            Ok(())
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn mouse_drag(
        &self,
        connection_id: &str,
        from_x: f64,
        from_y: f64,
        to_x: f64,
        to_y: f64,
        button: &str,
    ) -> Result<()> {
        let button = mouse_button(button)?;
        self.with_session(connection_id, |session| {
            let (from_x, from_y) = session.scale_to_native(from_x, from_y);
            let (to_x, to_y) = session.scale_to_native(to_x, to_y);
            session.command(&json!({"type": "mouse_move", "x": from_x, "y": from_y}))?;
            session.command(&json!({
                "type": "mouse_button_down", "x": from_x, "y": from_y, "button": button
            }))?;
            let movement = (|| {
                for step in 1..=10 {
                    let x = from_x + (to_x - from_x) * step / 10;
                    let y = from_y + (to_y - from_y) * step / 10;
                    session.command(&json!({"type": "mouse_move", "x": x, "y": y}))?;
                    thread::sleep(Duration::from_millis(15));
                }
                Ok(())
            })();
            let release = session.command(&json!({
                "type": "mouse_button_up", "x": to_x, "y": to_y, "button": button
            }));
            movement.and(release)
        })
    }

    pub fn mouse_scroll(
        &self,
        connection_id: &str,
        x: f64,
        y: f64,
        delta: i32,
        vertical: bool,
    ) -> Result<()> {
        self.with_session(connection_id, |session| {
            let (x, y) = session.scale_to_native(x, y);
            session.command(&json!({
                "type": "mouse_scroll", "x": x, "y": y,
                "delta": delta, "vertical": vertical
            }))
        })
    }

    pub fn type_text(&self, connection_id: &str, text: &str, delay_ms: u64) -> Result<()> {
        self.with_session(connection_id, |session| {
            for character in text.chars() {
                let stroke = key_stroke(&character.to_string())?;
                send_key_stroke(session, stroke, &[], "press")?;
                if delay_ms > 0 {
                    thread::sleep(Duration::from_millis(delay_ms));
                }
            }
            Ok(())
        })
    }

    pub fn send_key(
        &self,
        connection_id: &str,
        key: &str,
        modifiers: &[String],
        action: &str,
    ) -> Result<()> {
        let stroke = key_stroke(key)?;
        let modifiers = modifiers
            .iter()
            .map(|name| modifier(name))
            .collect::<Result<Vec<_>>>()?;
        self.with_session(connection_id, |session| {
            send_key_stroke(session, stroke, &modifiers, action)
        })
    }

    pub fn resize(&self, connection_id: &str, width: u32, height: u32) -> Result<(u32, u32)> {
        let width = width.clamp(200, 8192) & !1;
        let height = height.clamp(200, 8192) & !1;
        self.with_session(connection_id, |session| {
            session.command(&json!({
                "type": "resize", "width": width, "height": height,
                "desktopScaleFactor": 100, "deviceScaleFactor": 100
            }))?;
            let deadline = Instant::now() + Duration::from_secs(10);
            let mut state = session
                .callback
                .inner
                .lock()
                .expect("session state poisoned");
            while (state.width != width || state.height != height) && state.error.is_none() {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    break;
                }
                let (next, wait) = session
                    .callback
                    .changed
                    .wait_timeout(state, remaining)
                    .expect("session state poisoned");
                state = next;
                if wait.timed_out() {
                    break;
                }
            }
            if let Some(error) = &state.error {
                bail!("native RDP error: {error}");
            }
            Ok((state.width, state.height))
        })
    }

    fn with_session<T>(
        &self,
        connection_id: &str,
        operation: impl FnOnce(&NativeSession) -> Result<T>,
    ) -> Result<T> {
        let slot = self.session.lock().expect("manager state poisoned");
        let session = slot
            .as_ref()
            .filter(|session| session.id == connection_id)
            .ok_or_else(|| anyhow::anyhow!("connection not found: {connection_id}"))?;
        operation(session)
    }
}

fn mouse_button(button: &str) -> Result<u8> {
    match button.to_ascii_lowercase().as_str() {
        "left" => Ok(0),
        "middle" => Ok(1),
        "right" => Ok(2),
        _ => bail!("mouse button must be 'left', 'right', or 'middle'"),
    }
}

fn send_key_stroke(
    session: &NativeSession,
    stroke: KeyStroke,
    modifiers: &[KeyStroke],
    action: &str,
) -> Result<()> {
    let mut effective_modifiers = modifiers.to_vec();
    if stroke.shift && !effective_modifiers.iter().any(|item| item.scancode == 0x2a) {
        effective_modifiers.push(KeyStroke {
            scancode: 0x2a,
            extended: false,
            shift: false,
        });
    }

    match action {
        "press" => {
            for modifier in &effective_modifiers {
                key_event(session, *modifier, true)?;
            }
            key_event(session, stroke, true)?;
            key_event(session, stroke, false)?;
            for modifier in effective_modifiers.iter().rev() {
                key_event(session, *modifier, false)?;
            }
        }
        "down" => key_event(session, stroke, true)?,
        "up" => key_event(session, stroke, false)?,
        _ => bail!("key action must be 'press', 'down', or 'up'"),
    }
    Ok(())
}

fn key_event(session: &NativeSession, stroke: KeyStroke, down: bool) -> Result<()> {
    session.command(&json!({
        "type": if down { "key_down" } else { "key_up" },
        "scancode": stroke.scancode,
        "extended": stroke.extended
    }))
}

unsafe extern "C" fn native_output_callback(
    message_type: u32,
    part_a: *const c_void,
    part_a_length: u32,
    part_b: *const c_void,
    part_b_length: u32,
    user_data: *mut c_void,
) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if user_data.is_null() {
            return;
        }
        // SAFETY: user_data points to CallbackState until native shutdown completes.
        let callback = unsafe { &*(user_data as *const CallbackState) };
        // SAFETY: C guarantees borrowed buffers are valid for this callback invocation.
        let part_a = unsafe { borrowed_bytes(part_a, part_a_length) };
        // SAFETY: same contract as part_a.
        let part_b = unsafe { borrowed_bytes(part_b, part_b_length) };
        handle_native_output(callback, message_type, part_a, part_b);
    }));
}

unsafe fn borrowed_bytes<'a>(pointer: *const c_void, length: u32) -> &'a [u8] {
    if pointer.is_null() || length == 0 {
        &[]
    } else {
        // SAFETY: caller upholds validity for `length` bytes.
        unsafe { slice::from_raw_parts(pointer.cast::<u8>(), length as usize) }
    }
}

fn handle_native_output(callback: &CallbackState, message_type: u32, part_a: &[u8], part_b: &[u8]) {
    let mut state = callback.inner.lock().expect("session state poisoned");
    match message_type {
        MSG_CONNECTED | MSG_RESIZED => {
            if let Ok(value) = serde_json::from_slice::<Value>(part_a) {
                let width = value["width"].as_u64().unwrap_or(0) as u32;
                let height = value["height"].as_u64().unwrap_or(0) as u32;
                if (1..=8192).contains(&width) && (1..=8192).contains(&height) {
                    state.width = width;
                    state.height = height;
                    state.framebuffer = vec![0; width as usize * height as usize * 4];
                    state.frame_version = 0;
                    state.scale_x = 1.0;
                    state.scale_y = 1.0;
                    state.connected = true;
                    state.disconnected = false;
                }
            }
        }
        MSG_BITMAP_UPDATE => apply_bitmap(&mut state, part_a, part_b),
        MSG_ERROR => {
            state.error = serde_json::from_slice::<Value>(part_a)
                .ok()
                .and_then(|value| value["message"].as_str().map(ToOwned::to_owned))
                .or_else(|| Some("unknown native FreeRDP error".into()));
        }
        MSG_DISCONNECTED => {
            state.connected = false;
            state.disconnected = true;
            if let Ok(value) = serde_json::from_slice::<Value>(part_a)
                && let Some(error) = value["error"].as_str()
            {
                state.error = Some(error.to_owned());
            }
        }
        _ => {}
    }
    callback.changed.notify_all();
}

fn apply_bitmap(state: &mut SessionState, header: &[u8], pixels: &[u8]) {
    if header.len() != 8 || state.width == 0 || state.height == 0 {
        return;
    }
    let x = u16::from_le_bytes([header[0], header[1]]) as u32;
    let y = u16::from_le_bytes([header[2], header[3]]) as u32;
    let width = u16::from_le_bytes([header[4], header[5]]) as u32;
    let height = u16::from_le_bytes([header[6], header[7]]) as u32;
    if pixels.len() != width as usize * height as usize * 4 {
        state.error = Some("invalid native bitmap payload".into());
        return;
    }
    let copy_width = width.min(state.width.saturating_sub(x));
    let copy_height = height.min(state.height.saturating_sub(y));
    if copy_width == 0 || copy_height == 0 {
        return;
    }
    let source_stride = width as usize * 4;
    let target_stride = state.width as usize * 4;
    let copy_bytes = copy_width as usize * 4;
    for row in 0..copy_height as usize {
        let source_offset = row * source_stride;
        let target_offset = (y as usize + row) * target_stride + x as usize * 4;
        state.framebuffer[target_offset..target_offset + copy_bytes]
            .copy_from_slice(&pixels[source_offset..source_offset + copy_bytes]);
    }
    state.frame_version += 1;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn applies_bitmap_region() {
        let mut state = SessionState {
            width: 2,
            height: 2,
            framebuffer: vec![0; 16],
            ..SessionState::default()
        };
        let header = [1, 0, 0, 0, 1, 0, 2, 0];
        let pixels = [1, 2, 3, 4, 5, 6, 7, 8];
        apply_bitmap(&mut state, &header, &pixels);
        assert_eq!(&state.framebuffer[4..8], &pixels[..4]);
        assert_eq!(&state.framebuffer[12..16], &pixels[4..]);
        assert_eq!(state.frame_version, 1);
    }
}
