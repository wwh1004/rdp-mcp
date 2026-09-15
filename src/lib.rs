mod cli;
mod image;
mod keyboard;
mod preview;
mod server;
mod session;
mod worker;

use std::{
    ffi::CStr,
    net::SocketAddr,
    os::raw::c_char,
    panic::AssertUnwindSafe,
    pin::Pin,
    task::{Context as TaskContext, Poll},
};

use anyhow::Result;
use cli::{Command, HELP, Transport};
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
};
use rmcp::{ServiceExt, transport::stdio};
use tokio::io::{AsyncRead, ReadBuf};
use tokio_util::sync::CancellationToken;

// The MCP service waits for in-flight tools before completing. Observe transport
// EOF directly so those tools can be interrupted by closing their workers first.
struct EofReader<R> {
    inner: R,
    cancellation: CancellationToken,
}
impl<R: AsyncRead + Unpin> AsyncRead for EofReader<R> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();
        let before = buffer.filled().len();
        let has_space = buffer.remaining() != 0;
        let result = Pin::new(&mut this.inner).poll_read(cx, buffer);
        if let Poll::Ready(status) = &result
            && (status.is_err() || (has_space && buffer.filled().len() == before))
        {
            this.cancellation.cancel();
        }
        result
    }
}

fn run(args: Vec<String>) -> Result<()> {
    match cli::parse(&args)? {
        Command::Worker => worker::run(),
        Command::Help => {
            print!("{HELP}");
            Ok(())
        }
        Command::Version => {
            println!("rdp-mcp {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Command::Run(transport, preview_bind) => {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?;
            runtime.block_on(run_server(transport, preview_bind))
        }
    }
}

async fn run_server(transport: Transport, preview_bind: Option<SocketAddr>) -> Result<()> {
    let service = server::RdpMcpServer::new();
    let cancellation = CancellationToken::new();
    let _cancel_on_exit = cancellation.clone().drop_guard();
    let preview_app = preview::router(service.manager(), cancellation.clone());
    // Bind before starting MCP so startup reports port conflicts immediately.
    let preview_listener = match preview_bind {
        Some(bind) => {
            let listener = tokio::net::TcpListener::bind(bind).await?;
            eprintln!(
                "rdp-mcp preview listening on http://{}{}",
                listener.local_addr()?,
                preview::PATH
            );
            Some(listener)
        }
        None => None,
    };
    let manager = service.manager();
    let serving = async {
        let mcp = run_transport(transport, service, cancellation.clone());
        if let Some(listener) = preview_listener {
            tokio::select! {
                result = mcp => result,
                result = async { axum::serve(listener, preview_app).await } => {
                    result?;
                    Ok(())
                }
            }
        } else {
            mcp.await
        }
    };
    let result = tokio::select! {
        result = serving => result,
        _ = cancellation.cancelled() => Ok(()),
        signal = tokio::signal::ctrl_c() => signal.map_err(anyhow::Error::from),
    };
    cancellation.cancel();
    tokio::task::spawn_blocking(move || manager.shutdown()).await?;
    result
}

async fn run_transport(
    transport: Transport,
    service: server::RdpMcpServer,
    cancellation: CancellationToken,
) -> Result<()> {
    match transport {
        Transport::Stdio => {
            let (input, output) = stdio();
            let running = service
                .serve((
                    EofReader {
                        inner: input,
                        cancellation,
                    },
                    output,
                ))
                .await?;
            running.waiting().await?;
        }
        Transport::Http { bind, path } => {
            let shared = service.clone();
            let http_service = StreamableHttpService::new(
                move || Ok(shared.clone()),
                LocalSessionManager::default().into(),
                StreamableHttpServerConfig::default()
                    .with_cancellation_token(cancellation.child_token()),
            );
            let app = preview::router(service.manager(), cancellation.clone())
                .nest_service(&path, http_service);
            let listener = tokio::net::TcpListener::bind(bind).await?;
            eprintln!("rdp-mcp Streamable HTTP listening on http://{bind}{path}");
            axum::serve(listener, app).await?;
        }
    }
    Ok(())
}

unsafe fn collect_args(argc: i32, argv: *const *const c_char) -> Result<Vec<String>> {
    if argc < 0 || (argc > 0 && argv.is_null()) {
        anyhow::bail!("invalid argc/argv from launcher");
    }

    let mut args = Vec::with_capacity(argc as usize);
    for index in 0..argc as isize {
        // SAFETY: the C launcher provides argc valid, non-null argv entries.
        let pointer = unsafe { *argv.offset(index) };
        if pointer.is_null() {
            anyhow::bail!("null argv entry at index {index}");
        }
        // SAFETY: argv strings are NUL-terminated for the duration of this call.
        args.push(
            unsafe { CStr::from_ptr(pointer) }
                .to_string_lossy()
                .into_owned(),
        );
    }
    Ok(args)
}

/// C launcher entry point used by the final single-file executable.
///
/// # Safety
///
/// `argv` must point to `argc` valid, NUL-terminated C strings for the entire
/// duration of this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rdp_mcp_main(argc: i32, argv: *const *const c_char) -> i32 {
    let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: arguments come directly from the platform C main function.
        let args = unsafe { collect_args(argc, argv) }?;
        run(args)
    }));

    match result {
        Ok(Ok(())) => 0,
        Ok(Err(error)) => {
            eprintln!("rdp-mcp: {error:#}");
            1
        }
        Err(_) => {
            eprintln!("rdp-mcp: fatal Rust panic");
            101
        }
    }
}
