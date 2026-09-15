//! Pull-based MJPEG: no viewers means no work, and slow viewers do not queue frames.
use std::{convert::Infallible, sync::Arc, time::Duration};

use axum::{
    Router,
    body::{Body, Bytes},
    extract::{Query, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use futures_util::stream;
use tokio_util::sync::CancellationToken;

use crate::{
    image,
    session::{PreviewSource, SessionManager},
};

pub const PATH: &str = "/preview.mjpg";
const PERIOD: Duration = Duration::from_millis(100);

#[derive(Clone)]
struct PreviewState {
    manager: Arc<SessionManager>,
    cancellation: CancellationToken,
}

pub fn router(manager: Arc<SessionManager>, cancellation: CancellationToken) -> Router {
    Router::new()
        .route(PATH, get(preview))
        .with_state(PreviewState {
            manager,
            cancellation,
        })
}

struct Viewer {
    source: PreviewSource,
    cancellation: CancellationToken,
    key: Option<(u32, u32, u64)>,
    part: Bytes,
}

#[derive(serde::Deserialize)]
struct PreviewParams {
    connection_id: String,
}

async fn preview(
    State(state): State<PreviewState>,
    Query(params): Query<PreviewParams>,
) -> Response {
    let source = match state.manager.preview(&params.connection_id) {
        Ok(source) => source,
        Err(_) => return (StatusCode::NOT_FOUND, "RDP connection not found").into_response(),
    };
    stream_preview(source, state.cancellation)
}

fn stream_preview(source: PreviewSource, cancellation: CancellationToken) -> Response {
    let viewer = Viewer {
        source,
        cancellation,
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
                let Some(frame) = viewer.source.frame().ok()? else {
                    // Do not send stale data while disconnected/busy.
                    viewer.key = None;
                    viewer.part = Bytes::new();
                    return Some(viewer);
                };
                let key = (frame.width, frame.height, frame.version);
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
                Some(viewer)
            })
            .await
            .ok()??;
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
        .into_response()
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
        let source = crate::session::test_preview_source();
        let response = stream_preview(source, cancellation.clone());
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

    #[tokio::test]
    async fn previews_are_bound_to_the_selected_connection_and_end_on_close() {
        use futures_util::StreamExt;

        let manager = Arc::new(SessionManager::new());
        let open = |host: &str| {
            manager
                .open(
                    host.into(),
                    3389,
                    Some("test".into()),
                    Some("test".into()),
                    None,
                    200,
                    200,
                )
                .unwrap()
        };
        let a = open("red");
        let b = open("blue");
        let cancellation = CancellationToken::new();
        let mut a_frames = stream_preview(manager.preview(&a.id).unwrap(), cancellation.clone())
            .into_body()
            .into_data_stream();
        let mut b_frames = stream_preview(manager.preview(&b.id).unwrap(), cancellation)
            .into_body()
            .into_data_stream();
        let first_a = a_frames.next().await.unwrap().unwrap();
        let first_b = b_frames.next().await.unwrap().unwrap();
        assert_ne!(first_a, first_b);
        manager.close(&a.id).unwrap();
        assert!(
            tokio::time::timeout(Duration::from_secs(1), a_frames.next())
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(b_frames.next().await.unwrap().unwrap(), first_b);
        manager.shutdown();
    }
}
