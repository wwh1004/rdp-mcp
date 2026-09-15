//! Pull-based MJPEG: no viewers means no work, and slow viewers do not queue frames.
use std::{convert::Infallible, sync::Arc, time::Duration};

use axum::{
    Router,
    body::{Body, Bytes},
    extract::State,
    http::header,
    response::IntoResponse,
    routing::get,
};
use futures_util::stream;
use tokio_util::sync::CancellationToken;

use crate::{image, native::NativeManager};

pub const PATH: &str = "/preview.mjpg";
const PERIOD: Duration = Duration::from_millis(100);

#[derive(Clone)]
struct PreviewState {
    manager: Arc<NativeManager>,
    cancellation: CancellationToken,
}

pub fn router(manager: Arc<NativeManager>, cancellation: CancellationToken) -> Router {
    Router::new()
        .route(PATH, get(preview))
        .with_state(PreviewState {
            manager,
            cancellation,
        })
}

struct Viewer {
    manager: Arc<NativeManager>,
    cancellation: CancellationToken,
    key: Option<(String, u32, u32, u64)>,
    part: Bytes,
}

async fn preview(State(state): State<PreviewState>) -> impl IntoResponse {
    let viewer = Viewer {
        manager: state.manager,
        cancellation: state.cancellation,
        key: None,
        part: Bytes::new(),
    };
    let frames = stream::unfold(viewer, |mut viewer| async move {
        loop {
            // Sleep between pulls rather than catching up after a slow consumer.
            tokio::select! {
                _ = viewer.cancellation.cancelled() => return None,
                _ = tokio::time::sleep(PERIOD) => {}
            }
            viewer = tokio::task::spawn_blocking(move || {
                let Some(frame) = viewer.manager.preview_frame() else {
                    // Do not send stale data while disconnected/busy.
                    viewer.key = None;
                    viewer.part = Bytes::new();
                    return viewer;
                };
                let key = (frame.connection_id, frame.width, frame.height, frame.version);
                if viewer.key.as_ref() != Some(&key) {
                    match image::encode(
                        &frame.pixels,
                        frame.width,
                        frame.height,
                        "jpeg",
                        65,
                        None,
                        None,
                    ) {
                        Ok(jpeg) => {
                            viewer.part = multipart(&jpeg.bytes);
                            viewer.key = Some(key);
                        }
                        Err(_) => {
                            viewer.key = None;
                            viewer.part = Bytes::new();
                        }
                    }
                }
                viewer
            })
            .await
            .ok()?;
            if !viewer.part.is_empty() {
                // Re-send cached JPEGs on a static desktop to keep player pacing.
                return Some((Ok::<_, Infallible>(viewer.part.clone()), viewer));
            }
        }
    });
    (
        [
            (
                header::CONTENT_TYPE,
                "multipart/x-mixed-replace; boundary=rdp-frame",
            ),
            (header::CACHE_CONTROL, "no-store"),
            (header::HeaderName::from_static("x-accel-buffering"), "no"),
        ],
        Body::from_stream(frames),
    )
}

fn multipart(jpeg: &[u8]) -> Bytes {
    let mut part = format!(
        "--rdp-frame\r\nContent-Type: image/jpeg\r\nContent-Length: {}\r\n\r\n",
        jpeg.len()
    )
    .into_bytes();
    part.extend_from_slice(jpeg);
    part.extend_from_slice(b"\r\n");
    Bytes::from(part)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multipart_preserves_binary_payload_and_length() {
        let jpeg = [0xff, 0xd8, 0, 0xff, 0xd9];
        let part = multipart(&jpeg);
        let header = b"--rdp-frame\r\nContent-Type: image/jpeg\r\nContent-Length: 5\r\n\r\n";
        assert!(part.starts_with(header));
        assert_eq!(&part[header.len()..part.len() - 2], &jpeg);
        assert!(part.ends_with(b"\r\n"));
    }

    #[tokio::test]
    async fn idle_preview_ends_on_shutdown() {
        use futures_util::StreamExt;

        let cancellation = CancellationToken::new();
        let response = preview(State(PreviewState {
            manager: Arc::new(NativeManager::new()),
            cancellation: cancellation.clone(),
        }))
        .await
        .into_response();
        assert_eq!(
            response.headers()[header::CONTENT_TYPE],
            "multipart/x-mixed-replace; boundary=rdp-frame"
        );
        let mut frames = response.into_body().into_data_stream();
        assert!(
            tokio::time::timeout(Duration::from_millis(150), frames.next())
                .await
                .is_err()
        );
        cancellation.cancel();
        assert!(
            tokio::time::timeout(Duration::from_secs(1), frames.next())
                .await
                .unwrap()
                .is_none()
        );
    }
}
