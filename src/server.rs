use std::sync::Arc;

use base64::Engine;
use rmcp::{
    ErrorData as McpError, ServerHandler,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{CallToolResult, ContentBlock},
    schemars, tool, tool_handler, tool_router,
};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::native::{NativeManager, ScreenshotRegion as NativeScreenshotRegion};

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ConnectionOpenParams {
    /// Host to connect to.
    pub host: String,
    /// Port. RDP defaults to 3389.
    pub port: Option<u16>,
    /// Username for authentication.
    pub username: Option<String>,
    /// Password for authentication.
    pub password: Option<String>,
    /// Optional connection name.
    pub name: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ConnectionIdParams {
    /// RDP connection ID returned by rdp_open or rdp_list, e.g. rdp_1.
    pub connection_id: String,
}

#[derive(Debug, Clone, Copy, Deserialize, schemars::JsonSchema)]
pub struct ScreenshotRegion {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ScreenshotParams {
    /// RDP connection ID returned by rdp_open or rdp_list, e.g. rdp_1.
    pub connection_id: String,
    /// Image format: png or jpeg.
    #[serde(default = "default_image_format")]
    pub format: String,
    /// JPEG quality from 1 through 100.
    #[serde(default = "default_quality")]
    pub quality: u8,
    /// Maximum returned width. Zero disables resizing.
    #[serde(default = "default_max_width")]
    pub max_width: u32,
    /// Optional capture region in native desktop pixels, independent of screenshot size.
    pub region: Option<ScreenshotRegion>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ClickParams {
    pub connection_id: String,
    pub x: i32,
    pub y: i32,
    #[serde(default = "default_button")]
    pub button: String,
    #[serde(default)]
    pub double_click: bool,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct TypeParams {
    pub connection_id: String,
    pub text: String,
    #[serde(default)]
    pub delay_ms: u64,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SendKeyParams {
    pub connection_id: String,
    pub key: String,
    #[serde(default)]
    pub modifiers: Vec<String>,
    #[serde(default = "default_key_action")]
    pub action: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct MouseMoveParams {
    pub connection_id: String,
    pub x: i32,
    pub y: i32,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct MouseDragParams {
    pub connection_id: String,
    pub from_x: i32,
    pub from_y: i32,
    pub to_x: i32,
    pub to_y: i32,
    #[serde(default = "default_button")]
    pub button: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct MouseScrollParams {
    pub connection_id: String,
    pub x: i32,
    pub y: i32,
    pub delta: f64,
    #[serde(default = "default_true")]
    pub vertical: bool,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ResizeParams {
    pub connection_id: String,
    pub width: u32,
    pub height: u32,
}

fn default_image_format() -> String {
    "jpeg".into()
}

fn default_quality() -> u8 {
    40
}

fn default_max_width() -> u32 {
    1024
}

fn default_button() -> String {
    "left".into()
}

fn default_key_action() -> String {
    "press".into()
}

fn default_true() -> bool {
    true
}

#[derive(Clone)]
pub struct RdpMcpServer {
    manager: Arc<NativeManager>,
    #[allow(dead_code)]
    tool_router: ToolRouter<Self>,
}

impl RdpMcpServer {
    pub fn new() -> Self {
        Self {
            manager: Arc::new(NativeManager::new()),
            tool_router: Self::tool_router(),
        }
    }
}

#[tool_router]
impl RdpMcpServer {
    #[tool(
        description = "List all active RDP connections. Returns the session id used by all RDP tools."
    )]
    async fn rdp_list(&self) -> Result<CallToolResult, McpError> {
        let connections = self
            .manager
            .list()
            .into_iter()
            .map(|connection| {
                json!({
                    "id": connection.id,
                    "name": connection.name,
                    "connection_type": "rdp",
                    "host": connection.host,
                    "port": connection.port,
                    "status": connection.status,
                })
            })
            .collect::<Vec<_>>();
        json_result(json!({"connections": connections}))
    }

    #[tool(
        description = "Open a new connection by specifying host, port, and credentials manually. This standalone server supports RDP only."
    )]
    async fn rdp_open(
        &self,
        Parameters(params): Parameters<ConnectionOpenParams>,
    ) -> Result<CallToolResult, McpError> {
        let manager = self.manager.clone();
        let info = blocking(move || {
            manager.open(
                params.host,
                params.port.unwrap_or(3389),
                params.username,
                params.password,
                params.name,
                1280,
                720,
            )
        })
        .await?;
        json_result(json!({
            "id": info.id,
            "name": info.name,
            "connection_type": "rdp",
            "host": info.host,
            "port": info.port,
            "status": info.status,
        }))
    }

    #[tool(description = "Close an active connection")]
    async fn rdp_close(
        &self,
        Parameters(params): Parameters<ConnectionIdParams>,
    ) -> Result<CallToolResult, McpError> {
        let manager = self.manager.clone();
        let closed_id = params.connection_id;
        let id = closed_id.clone();
        blocking(move || manager.close(&id)).await?;
        json_result(json!({"success": true, "closed_id": closed_id}))
    }

    #[tool(
        description = "Capture a screenshot of an RDP session. Returns a native MCP image block. Region and input coordinates use native desktop pixels; screenshot resizing does not change input coordinates. Set max_width to 0 for an original-size image."
    )]
    async fn rdp_screenshot(
        &self,
        Parameters(params): Parameters<ScreenshotParams>,
    ) -> Result<CallToolResult, McpError> {
        let manager = self.manager.clone();
        let full_capture = params.region.is_none();
        let region = params.region.map(|region| NativeScreenshotRegion {
            x: region.x,
            y: region.y,
            width: region.width,
            height: region.height,
        });
        let screenshot = blocking(move || {
            manager.screenshot(
                &params.connection_id,
                &params.format,
                params.quality,
                (params.max_width != 0).then_some(params.max_width),
                region,
            )
        })
        .await?;
        let metadata = json!({
            "width": screenshot.encoded.width,
            "height": screenshot.encoded.height,
            "native_width": screenshot.native_width,
            "native_height": screenshot.native_height,
            "frame_version": screenshot.frame_version,
            "full_capture": full_capture,
        });
        let data = base64::engine::general_purpose::STANDARD.encode(screenshot.encoded.bytes);
        let mut result = CallToolResult::success(vec![
            ContentBlock::image(data, screenshot.encoded.mime_type),
            ContentBlock::json(&metadata)?,
        ]);
        result.structured_content = Some(metadata);
        Ok(result)
    }

    #[tool(
        description = "Send a mouse click to an RDP session. Coordinates are in native desktop pixels."
    )]
    async fn rdp_click(
        &self,
        Parameters(params): Parameters<ClickParams>,
    ) -> Result<CallToolResult, McpError> {
        let manager = self.manager.clone();
        let connection_id = params.connection_id;
        let button = params.button;
        let response_button = button.clone();
        blocking(move || {
            manager.mouse_click(
                &connection_id,
                params.x,
                params.y,
                &button,
                params.double_click,
            )
        })
        .await?;
        json_result(json!({
            "success": true, "x": params.x, "y": params.y, "button": response_button
        }))
    }

    #[tool(description = "Type text in an RDP session")]
    async fn rdp_type(
        &self,
        Parameters(params): Parameters<TypeParams>,
    ) -> Result<CallToolResult, McpError> {
        let manager = self.manager.clone();
        let characters_typed = params.text.chars().count();
        blocking(move || manager.type_text(&params.connection_id, &params.text, params.delay_ms))
            .await?;
        json_result(json!({"success": true, "characters_typed": characters_typed}))
    }

    #[tool(description = "Send a keyboard event to an RDP session (key press, down, or up)")]
    async fn rdp_send_key(
        &self,
        Parameters(params): Parameters<SendKeyParams>,
    ) -> Result<CallToolResult, McpError> {
        let manager = self.manager.clone();
        let key = params.key;
        let modifiers = params.modifiers;
        let action = params.action;
        let response_key = key.clone();
        let response_modifiers = modifiers.clone();
        blocking(move || manager.send_key(&params.connection_id, &key, &modifiers, &action))
            .await?;
        json_result(json!({
            "success": true, "key": response_key, "modifiers": response_modifiers
        }))
    }

    #[tool(
        description = "Move the mouse cursor in an RDP session. Coordinates are in native desktop pixels."
    )]
    async fn rdp_mouse_move(
        &self,
        Parameters(params): Parameters<MouseMoveParams>,
    ) -> Result<CallToolResult, McpError> {
        let manager = self.manager.clone();
        blocking(move || manager.mouse_move(&params.connection_id, params.x, params.y)).await?;
        json_result(json!({"success": true, "x": params.x, "y": params.y}))
    }

    #[tool(
        description = "Perform a mouse drag operation in an RDP session. Coordinates are in native desktop pixels."
    )]
    async fn rdp_mouse_drag(
        &self,
        Parameters(params): Parameters<MouseDragParams>,
    ) -> Result<CallToolResult, McpError> {
        let manager = self.manager.clone();
        let button = params.button;
        blocking(move || {
            manager.mouse_drag(
                &params.connection_id,
                params.from_x,
                params.from_y,
                params.to_x,
                params.to_y,
                &button,
            )
        })
        .await?;
        json_result(json!({
            "success": true,
            "from_x": params.from_x,
            "from_y": params.from_y,
            "to_x": params.to_x,
            "to_y": params.to_y,
        }))
    }

    #[tool(
        description = "Send a mouse scroll event to an RDP session. Coordinates are in native desktop pixels."
    )]
    async fn rdp_mouse_scroll(
        &self,
        Parameters(params): Parameters<MouseScrollParams>,
    ) -> Result<CallToolResult, McpError> {
        let manager = self.manager.clone();
        blocking(move || {
            manager.mouse_scroll(
                &params.connection_id,
                params.x,
                params.y,
                params.delta.round() as i32,
                params.vertical,
            )
        })
        .await?;
        json_result(json!({
            "success": true,
            "x": params.x,
            "y": params.y,
            "delta": params.delta,
            "vertical": params.vertical,
        }))
    }

    #[tool(
        description = "Resize the RDP display via RDPEDISP. Dimensions are clamped to 200-8192 and rounded to even numbers."
    )]
    async fn rdp_resize(
        &self,
        Parameters(params): Parameters<ResizeParams>,
    ) -> Result<CallToolResult, McpError> {
        let manager = self.manager.clone();
        let (width, height) =
            blocking(move || manager.resize(&params.connection_id, params.width, params.height))
                .await?;
        json_result(json!({"success": true, "width": width, "height": height}))
    }

    #[tool(description = "Get the native dimensions of an RDP session display")]
    async fn rdp_get_dimensions(
        &self,
        Parameters(params): Parameters<ConnectionIdParams>,
    ) -> Result<CallToolResult, McpError> {
        let manager = self.manager.clone();
        let (width, height) = blocking(move || manager.dimensions(&params.connection_id)).await?;
        json_result(json!({"width": width, "height": height}))
    }
}

#[tool_handler(
    name = "rdp-mcp",
    version = "0.1.0",
    instructions = "Headless RDP control backed by an in-process FreeRDP C library"
)]
impl ServerHandler for RdpMcpServer {}

async fn blocking<T, F>(operation: F) -> Result<T, McpError>
where
    T: Send + 'static,
    F: FnOnce() -> anyhow::Result<T> + Send + 'static,
{
    tokio::task::spawn_blocking(operation)
        .await
        .map_err(|error| McpError::internal_error(error.to_string(), None))?
        .map_err(|error| McpError::internal_error(error.to_string(), None))
}

fn json_result(value: Value) -> Result<CallToolResult, McpError> {
    let mut result = CallToolResult::success(vec![ContentBlock::json(&value)?]);
    result.structured_content = Some(value);
    Ok(result)
}
