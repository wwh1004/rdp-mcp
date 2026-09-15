use std::{
    collections::HashMap,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use crate::{
    image::{self, EncodedImage, NativeRegion},
    keyboard::{KeyStroke, key_stroke, modifier},
    worker::{
        MSG_BITMAP_UPDATE, MSG_CONNECTED, MSG_DISCONNECTED, MSG_ERROR, MSG_READY, MSG_RESIZED,
        WorkerProcess,
    },
};

static NEXT_CONNECTION_ID: AtomicU64 = AtomicU64::new(1);

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
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
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
    has_frame: bool,
    frame_version: u64,
    error: Option<String>,
}

impl SessionState {
    fn check_connected(&self) -> Result<()> {
        if let Some(error) = &self.error {
            bail!("RDP connection error: {error}");
        }
        if self.disconnected {
            bail!("RDP connection is closed");
        }
        if !self.connected {
            bail!("RDP connection is still connecting");
        }
        Ok(())
    }
}

struct CallbackState {
    inner: Mutex<SessionState>,
    changed: Condvar,
}

impl CallbackState {
    fn new() -> Self {
        Self {
            inner: Mutex::new(SessionState::default()),
            changed: Condvar::new(),
        }
    }
    fn stop(&self, error: Option<String>) {
        let mut state = self.inner.lock().expect("session state poisoned");
        if !state.disconnected && state.error.is_none() {
            state.error = error;
        }
        state.connected = false;
        state.disconnected = true;
        state.framebuffer = Vec::new();
        state.has_frame = false;
        self.changed.notify_all();
    }
}

struct Session {
    id: String,
    name: String,
    host: String,
    port: u16,
    callback: Arc<CallbackState>,
    operation: Mutex<()>,
    worker: Mutex<Option<Arc<WorkerProcess>>>,
}

impl Session {
    fn new(host: String, port: u16, name: Option<String>) -> Self {
        Self {
            id: format!("rdp_{}", NEXT_CONNECTION_ID.fetch_add(1, Ordering::Relaxed)),
            name: name.unwrap_or_else(|| format!("RDP {host}")),
            host,
            port,
            callback: Arc::new(CallbackState::new()),
            operation: Mutex::new(()),
            worker: Mutex::new(None),
        }
    }

    fn connect(&self, username: String, password: String, width: u32, height: u32) -> Result<()> {
        let callback = self.callback.clone();
        let worker = Arc::new(WorkerProcess::spawn(Arc::new(move |kind, a, b| {
            handle_native_output(&callback, kind, a, b);
        }))?);
        {
            let mut slot = self.worker.lock().expect("session worker poisoned");
            if self
                .callback
                .inner
                .lock()
                .expect("session state poisoned")
                .disconnected
            {
                bail!("RDP connection was closed during worker startup");
            }
            *slot = Some(worker.clone());
        }
        worker.send(&json!({
            "type": "connect",
            "config": {
                "host": self.host, "port": self.port,
                "username": username, "password": password, "domain": "",
                "width": width.clamp(200, 8192), "height": height.clamp(200, 8192),
                "enableNla": true, "skipCertVerification": true,
                "enableGfx": true, "enableH264": false,
                "desktopScaleFactor": 100, "deviceScaleFactor": 100,
                "enableClipboard": true, "drives": []
            }
        }))?;
        self.wait_connected(Duration::from_secs(30))
    }

    fn command(&self, command: &Value) -> Result<()> {
        self.callback
            .inner
            .lock()
            .expect("session state poisoned")
            .check_connected()?;
        let worker = self
            .worker
            .lock()
            .expect("session worker poisoned")
            .clone()
            .context("RDP worker is closed")?;
        if let Err(error) = worker.send(command) {
            self.callback.stop(Some(error.to_string()));
            return Err(error);
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
            state = self
                .callback
                .changed
                .wait_timeout(state, remaining)
                .expect("session state poisoned")
                .0;
        }
        state.check_connected()
    }

    // Delays in compound input operations are interruptible by close/disconnect.
    fn pause(&self, delay: Duration) -> Result<()> {
        let state = self.callback.inner.lock().expect("session state poisoned");
        let (state, _) = self
            .callback
            .changed
            .wait_timeout_while(state, delay, |state| {
                state.connected && !state.disconnected && state.error.is_none()
            })
            .expect("session state poisoned");
        state.check_connected()
    }

    fn info(&self) -> ConnectionInfo {
        let state = self.callback.inner.lock().expect("session state poisoned");
        ConnectionInfo {
            id: self.id.clone(),
            name: self.name.clone(),
            host: self.host.clone(),
            port: self.port,
            status: if state.disconnected || state.error.is_some() {
                "disconnected"
            } else if state.connected {
                "connected"
            } else {
                "connecting"
            },
        }
    }

    fn dimensions(&self) -> Result<(u32, u32)> {
        let state = self.callback.inner.lock().expect("session state poisoned");
        state.check_connected()?;
        if state.width == 0 || state.height == 0 {
            bail!("RDP desktop dimensions are not available yet");
        }
        Ok((state.width, state.height))
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
        let waited_for_frame = !state.has_frame;
        while !state.has_frame && state.error.is_none() && !state.disconnected {
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
            if wait.timed_out() && !state.has_frame {
                bail!("no RDP desktop frame was received within 10 seconds");
            }
        }
        state.check_connected()?;
        drop(state);

        if waited_for_frame {
            self.pause(Duration::from_millis(500))?;
        }

        let state = self.callback.inner.lock().expect("session state poisoned");
        state.check_connected()?;
        if !state.has_frame {
            bail!("RDP desktop resized before capture; retry the screenshot");
        }
        let native_width = state.width;
        let native_height = state.height;
        let frame_version = state.frame_version;
        let pixels = state.framebuffer.clone();
        drop(state);
        let native_region = region.map(|region| NativeRegion {
            x: region.x.max(0) as u32,
            y: region.y.max(0) as u32,
            width: region.width.max(1),
            height: region.height.max(1),
        });
        let encoded = image::encode(
            &pixels,
            native_width,
            native_height,
            format,
            quality,
            max_width,
            native_region,
        )?;
        Ok(Screenshot {
            encoded,
            native_width,
            native_height,
            frame_version,
        })
    }

    fn shutdown(&self) {
        self.callback.stop(None);
        let worker = self.worker.lock().expect("session worker poisoned").take();
        if let Some(worker) = worker {
            worker.shutdown();
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.shutdown();
    }
}

pub struct PreviewFrame {
    pub width: u32,
    pub height: u32,
    pub version: u64,
    pub pixels: Vec<u8>,
}

// A preview owns only the shared frame state, so it cannot keep a worker alive.
pub struct PreviewSource(Arc<CallbackState>);
impl PreviewSource {
    pub fn frame(&self) -> Result<Option<PreviewFrame>> {
        let Ok(state) = self.0.inner.try_lock() else {
            return Ok(None);
        };
        if state.disconnected || state.error.is_some() {
            bail!("RDP connection is closed");
        }
        if !state.connected || !state.has_frame {
            return Ok(None);
        }
        Ok(Some(PreviewFrame {
            width: state.width,
            height: state.height,
            version: state.frame_version,
            pixels: state.framebuffer.clone(),
        }))
    }
}

#[derive(Default)]
struct Registry {
    sessions: HashMap<String, Arc<Session>>,
    closed: bool,
}

pub struct SessionManager {
    inner: Mutex<Registry>,
}

impl SessionManager {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(Registry::default()),
        }
    }

    fn get(&self, id: &str) -> Result<Arc<Session>> {
        self.inner
            .lock()
            .expect("manager state poisoned")
            .sessions
            .get(id)
            .cloned()
            .with_context(|| format!("connection not found: {id}"))
    }

    pub fn preview(&self, id: &str) -> Result<PreviewSource> {
        Ok(PreviewSource(self.get(id)?.callback.clone()))
    }

    pub fn list(&self) -> Vec<ConnectionInfo> {
        let sessions: Vec<_> = self
            .inner
            .lock()
            .expect("manager state poisoned")
            .sessions
            .values()
            .cloned()
            .collect();
        let mut result: Vec<_> = sessions.iter().map(|session| session.info()).collect();
        result.sort_by(|a, b| a.id.cmp(&b.id));
        result
    }

    #[allow(clippy::too_many_arguments)]
    pub fn open(
        &self,
        host: String,
        port: u16,
        username: Option<String>,
        password: Option<String>,
        name: Option<String>,
        width: u32,
        height: u32,
    ) -> Result<ConnectionInfo> {
        let username = username.context("username is required")?;
        let password = password.context("password is required")?;
        let session = Arc::new(Session::new(host, port, name));
        {
            let mut registry = self.inner.lock().expect("manager state poisoned");
            if registry.closed {
                bail!("RDP manager is shutting down");
            }
            registry
                .sessions
                .insert(session.id.clone(), session.clone());
        }
        if let Err(error) = session.connect(username, password, width, height) {
            self.inner
                .lock()
                .expect("manager state poisoned")
                .sessions
                .remove(&session.id);
            session.shutdown();
            return Err(error);
        }
        Ok(session.info())
    }

    pub fn close(&self, id: &str) -> Result<()> {
        let session = self
            .inner
            .lock()
            .expect("manager state poisoned")
            .sessions
            .remove(id)
            .with_context(|| format!("connection not found: {id}"))?;
        session.shutdown();
        Ok(())
    }

    pub fn shutdown(&self) {
        let sessions: Vec<_> = {
            let mut registry = self.inner.lock().expect("manager state poisoned");
            registry.closed = true;
            registry
                .sessions
                .drain()
                .map(|(_, session)| session)
                .collect()
        };
        thread::scope(|scope| {
            for session in sessions {
                scope.spawn(move || session.shutdown());
            }
        });
    }

    pub fn dimensions(&self, connection_id: &str) -> Result<(u32, u32)> {
        self.get(connection_id)?.dimensions()
    }

    pub fn screenshot(
        &self,
        connection_id: &str,
        format: &str,
        quality: u8,
        max_width: Option<u32>,
        region: Option<ScreenshotRegion>,
    ) -> Result<Screenshot> {
        self.get(connection_id)?
            .screenshot(format, quality, max_width, region)
    }

    pub fn mouse_move(&self, connection_id: &str, x: i32, y: i32) -> Result<()> {
        self.with_session(connection_id, |session| {
            session.command(&json!({"type": "mouse_move", "x": x, "y": y}))
        })
    }

    pub fn mouse_click(
        &self,
        connection_id: &str,
        x: i32,
        y: i32,
        button: &str,
        double_click: bool,
    ) -> Result<()> {
        let button = mouse_button(button)?;
        self.with_session(connection_id, |session| {
            let count = if double_click { 2 } else { 1 };
            for index in 0..count {
                session.command(&json!({
                    "type": "mouse_button_down", "x": x, "y": y, "button": button
                }))?;
                session.command(&json!({
                    "type": "mouse_button_up", "x": x, "y": y, "button": button
                }))?;
                if index + 1 < count {
                    session.pause(Duration::from_millis(80))?;
                }
            }
            Ok(())
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn mouse_drag(
        &self,
        connection_id: &str,
        from_x: i32,
        from_y: i32,
        to_x: i32,
        to_y: i32,
        button: &str,
    ) -> Result<()> {
        let button = mouse_button(button)?;
        self.with_session(connection_id, |session| {
            session.command(&json!({"type": "mouse_move", "x": from_x, "y": from_y}))?;
            session.command(&json!({
                "type": "mouse_button_down", "x": from_x, "y": from_y, "button": button
            }))?;
            let movement = (|| {
                for step in 1..=10 {
                    let x = from_x + (to_x - from_x) * step / 10;
                    let y = from_y + (to_y - from_y) * step / 10;
                    session.command(&json!({"type": "mouse_move", "x": x, "y": y}))?;
                    session.pause(Duration::from_millis(15))?;
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
        x: i32,
        y: i32,
        delta: i32,
        vertical: bool,
    ) -> Result<()> {
        self.with_session(connection_id, |session| {
            session.command(&json!({
                "type": "mouse_scroll", "x": x, "y": y,
                "delta": delta, "vertical": vertical
            }))
        })
    }

    pub fn type_text(&self, connection_id: &str, text: &str, delay_ms: u64) -> Result<()> {
        self.with_session(connection_id, |session| {
            for character in text.chars() {
                let mut code_units = [0; 2];
                for code_unit in character.encode_utf16(&mut code_units) {
                    session.command(&json!({
                        "type": "unicode_key_down", "code_unit": *code_unit
                    }))?;
                    session.command(&json!({
                        "type": "unicode_key_up", "code_unit": *code_unit
                    }))?;
                }
                if delay_ms > 0 {
                    session.pause(Duration::from_millis(delay_ms))?;
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
            while (state.width != width || state.height != height)
                && state.error.is_none()
                && !state.disconnected
            {
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
            state.check_connected()?;
            if state.width != width || state.height != height {
                bail!("RDP resize timed out waiting for {width}x{height}");
            }
            Ok((state.width, state.height))
        })
    }

    fn with_session<T>(
        &self,
        id: &str,
        operation: impl FnOnce(&Session) -> Result<T>,
    ) -> Result<T> {
        let session = self.get(id)?;
        let _guard = session
            .operation
            .lock()
            .expect("session operation poisoned");
        session
            .callback
            .inner
            .lock()
            .expect("session state poisoned")
            .check_connected()?;
        operation(&session)
    }
}

impl Drop for SessionManager {
    fn drop(&mut self) {
        self.shutdown();
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
    session: &Session,
    stroke: KeyStroke,
    modifiers: &[KeyStroke],
    action: &str,
) -> Result<()> {
    const CHORD_EVENT_GAP: Duration = Duration::from_millis(75);

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
                session.pause(CHORD_EVENT_GAP)?;
            }
            key_event(session, stroke, true)?;
            session.pause(CHORD_EVENT_GAP)?;
            key_event(session, stroke, false)?;
            for modifier in effective_modifiers.iter().rev() {
                session.pause(CHORD_EVENT_GAP)?;
                key_event(session, *modifier, false)?;
            }
            session.pause(CHORD_EVENT_GAP)?;
        }
        "down" => key_event(session, stroke, true)?,
        "up" => key_event(session, stroke, false)?,
        _ => bail!("key action must be 'press', 'down', or 'up'"),
    }
    Ok(())
}

fn key_event(session: &Session, stroke: KeyStroke, down: bool) -> Result<()> {
    session.command(&json!({
        "type": if down { "key_down" } else { "key_up" },
        "scancode": stroke.scancode,
        "extended": stroke.extended
    }))
}

fn handle_native_output(callback: &CallbackState, message_type: u32, part_a: &[u8], part_b: &[u8]) {
    if message_type == MSG_DISCONNECTED {
        let error = serde_json::from_slice::<Value>(part_a)
            .ok()
            .and_then(|value| value["error"].as_str().map(ToOwned::to_owned));
        callback.stop(error);
        return;
    }
    let mut state = callback.inner.lock().expect("session state poisoned");
    if state.disconnected {
        return;
    }
    match message_type {
        MSG_READY => {
            state.connected = true;
        }
        MSG_CONNECTED | MSG_RESIZED => {
            if let Ok(value) = serde_json::from_slice::<Value>(part_a) {
                let width = value["width"].as_u64().unwrap_or(0) as u32;
                let height = value["height"].as_u64().unwrap_or(0) as u32;
                if (1..=8192).contains(&width) && (1..=8192).contains(&height) {
                    state.width = width;
                    state.height = height;
                    state.framebuffer = vec![0; width as usize * height as usize * 4];
                    // Keep versions monotonic even when resizing back to the
                    // same dimensions, so a viewer cannot reuse an old JPEG.
                    state.has_frame = false;
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
    state.has_frame = true;
}

#[cfg(test)]
pub(crate) fn test_preview_source() -> PreviewSource {
    PreviewSource(Arc::new(CallbackState::new()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open(manager: &SessionManager, host: &str) -> Result<ConnectionInfo> {
        manager.open(
            host.into(),
            3389,
            Some("test".into()),
            Some("test".into()),
            None,
            200,
            200,
        )
    }

    fn until(mut condition: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !condition() {
            assert!(Instant::now() < deadline, "condition did not complete");
            thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn ready_and_terminal_events_cannot_revive_a_closed_session() {
        let source = test_preview_source();
        handle_native_output(&source.0, MSG_CONNECTED, br#"{"width":1,"height":1}"#, &[]);
        assert!(!source.0.inner.lock().unwrap().connected);
        handle_native_output(
            &source.0,
            MSG_BITMAP_UPDATE,
            &[0, 0, 0, 0, 1, 0, 1, 0],
            &[255, 0, 0, 255],
        );
        assert!(source.frame().unwrap().is_none());
        handle_native_output(&source.0, MSG_READY, &[], &[]);
        let frame = source.frame().unwrap().unwrap();
        source.0.inner.lock().unwrap().framebuffer.fill(0);
        assert_eq!(frame.pixels, [255, 0, 0, 255]);
        handle_native_output(&source.0, MSG_RESIZED, br#"{"width":1,"height":1}"#, &[]);
        assert!(source.frame().unwrap().is_none());
        handle_native_output(
            &source.0,
            MSG_BITMAP_UPDATE,
            &[0, 0, 0, 0, 1, 0, 1, 0],
            &[0, 0, 255, 255],
        );
        assert!(source.frame().unwrap().unwrap().version > frame.version);
        let guard = source.0.inner.lock().unwrap();
        assert!(source.frame().unwrap().is_none());
        drop(guard);
        source.0.stop(None);
        handle_native_output(&source.0, MSG_READY, &[], &[]);
        handle_native_output(&source.0, MSG_RESIZED, br#"{"width":2,"height":2}"#, &[]);
        assert!(source.frame().is_err());
        assert_eq!(source.0.inner.lock().unwrap().width, 1);
        assert!(source.0.inner.lock().unwrap().framebuffer.is_empty());
    }

    #[test]
    fn independent_workers_keep_frames_resize_and_operations_separate() {
        let manager = Arc::new(SessionManager::new());
        let (a, b) = thread::scope(|scope| {
            let a = scope.spawn(|| open(&manager, "red").unwrap());
            let b = scope.spawn(|| open(&manager, "blue").unwrap());
            (a.join().unwrap(), b.join().unwrap())
        });
        assert_ne!(a.id, b.id);
        let a_source = manager.preview(&a.id).unwrap();
        let b_source = manager.preview(&b.id).unwrap();
        assert_eq!(
            &a_source.frame().unwrap().unwrap().pixels[..4],
            &[255, 0, 0, 255]
        );
        assert_eq!(
            &b_source.frame().unwrap().unwrap().pixels[..4],
            &[0, 0, 255, 255]
        );

        let typing_manager = manager.clone();
        let a_id = a.id.clone();
        let typing = thread::spawn(move || typing_manager.type_text(&a_id, "abcdef", 30_000));
        let a_session = manager.get(&a.id).unwrap();
        until(|| a_session.operation.try_lock().is_err());
        assert_eq!(manager.list().len(), 2);
        assert!(manager.screenshot(&a.id, "png", 40, None, None).is_ok());
        assert_eq!(manager.resize(&b.id, 202, 204).unwrap(), (202, 204));
        assert_eq!(manager.dimensions(&a.id).unwrap(), (200, 200));
        assert!(open(&manager, "fail").is_err());
        assert_eq!(manager.list().len(), 2);

        manager.close(&a.id).unwrap();
        assert!(typing.join().unwrap().is_err());
        assert!(a_source.frame().is_err());
        assert_eq!(manager.dimensions(&b.id).unwrap(), (202, 204));
        assert!(manager.close("unknown").is_err());
        assert_eq!(manager.list().len(), 1);
        manager.shutdown();
        assert!(b_source.frame().is_err());
        assert!(open(&manager, "red").is_err());
    }

    #[test]
    fn worker_crash_is_reported_without_affecting_other_connections() {
        let manager = SessionManager::new();
        let a = open(&manager, "red").unwrap();
        let b = open(&manager, "blue").unwrap();
        manager
            .get(&a.id)
            .unwrap()
            .command(&json!({"type":"test_crash"}))
            .unwrap();
        until(|| manager.get(&a.id).unwrap().info().status == "disconnected");
        assert!(manager.dimensions(&a.id).is_err());
        assert!(manager.screenshot(&a.id, "png", 40, None, None).is_err());
        assert_eq!(manager.dimensions(&b.id).unwrap(), (200, 200));
        assert_eq!(manager.list().len(), 2);
        manager.close(&a.id).unwrap();
    }

    #[test]
    fn shutdown_cancels_an_open_and_reaps_an_unresponsive_worker() {
        let manager = Arc::new(SessionManager::new());
        let opening_manager = manager.clone();
        let opening = thread::spawn(move || open(&opening_manager, "hang"));
        until(|| !manager.list().is_empty());
        let id = manager.list()[0].id.clone();
        until(|| manager.get(&id).unwrap().worker.lock().unwrap().is_some());
        let started = Instant::now();
        manager.shutdown();
        assert!(started.elapsed() < Duration::from_secs(6));
        assert!(opening.join().unwrap().is_err());
        assert!(manager.list().is_empty());
    }
}
