mod cli;
mod image;
mod keyboard;
mod native;
mod server;

use std::{ffi::CStr, os::raw::c_char, panic::AssertUnwindSafe};

use anyhow::Result;
use axum::Router;
use cli::{Command, HELP, Transport};
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
};
use rmcp::{ServiceExt, transport::stdio};
use tokio_util::sync::CancellationToken;

fn run(args: Vec<String>) -> Result<()> {
    match cli::parse(&args)? {
        Command::Help => {
            print!("{HELP}");
            Ok(())
        }
        Command::Version => {
            println!("rdp-mcp {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Command::Run(transport) => {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?;
            runtime.block_on(run_server(transport))
        }
    }
}

async fn run_server(transport: Transport) -> Result<()> {
    let service = server::RdpMcpServer::new();
    match transport {
        Transport::Stdio => {
            let running = service.serve(stdio()).await?;
            running.waiting().await?;
        }
        Transport::Http { bind, path } => {
            let cancellation = CancellationToken::new();
            let shared = service.clone();
            let http_service = StreamableHttpService::new(
                move || Ok(shared.clone()),
                LocalSessionManager::default().into(),
                StreamableHttpServerConfig::default()
                    .with_cancellation_token(cancellation.child_token()),
            );
            let app = Router::new().nest_service(&path, http_service);
            let listener = tokio::net::TcpListener::bind(bind).await?;
            eprintln!("rdp-mcp Streamable HTTP listening on http://{bind}{path}");
            axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    let _ = tokio::signal::ctrl_c().await;
                    cancellation.cancel();
                })
                .await?;
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
