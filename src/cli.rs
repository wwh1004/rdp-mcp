use std::net::SocketAddr;

use anyhow::{Result, bail};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Transport {
    Stdio,
    Http { bind: SocketAddr, path: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Run(Transport, Option<SocketAddr>),
    Help,
    Version,
}

pub const HELP: &str = "rdp-mcp - MCP server for headless RDP control

Usage:
  rdp-mcp [stdio] [--preview-bind <IP:PORT>]
  rdp-mcp http [--bind <IP:PORT>] [--path <PATH>]
  rdp-mcp --transport <stdio|http> [--bind <IP:PORT>] [--path <PATH>]

Options:
  --preview-bind <IP:PORT>  Optional dedicated MJPEG listener (loopback only)
                           HTTP mode also serves /preview.mjpg on --bind
  --bind <IP:PORT>  HTTP listen address [default: 127.0.0.1:8000]
  --path <PATH>     Streamable HTTP MCP endpoint [default: /mcp]
  -h, --help        Print help
  -V, --version     Print version
";

pub fn parse(args: &[String]) -> Result<Command> {
    let mut mode: Option<String> = None;
    let mut bind = "127.0.0.1:8000".to_owned();
    let mut path = "/mcp".to_owned();
    let mut index = 1;
    let mut preview_bind = None;

    while index < args.len() {
        match args[index].as_str() {
            "stdio" | "http" if mode.is_none() => mode = Some(args[index].clone()),
            "--transport" => {
                index += 1;
                mode = Some(value(args, index, "--transport")?.to_owned());
            }
            "--bind" => {
                index += 1;
                bind = value(args, index, "--bind")?.to_owned();
            }
            "--path" => {
                index += 1;
                path = value(args, index, "--path")?.to_owned();
            }
            "--preview-bind" => {
                index += 1;
                let address = value(args, index, "--preview-bind")?.parse::<SocketAddr>()?;
                if !address.ip().is_loopback() {
                    bail!("--preview-bind must use a loopback address");
                }
                preview_bind = Some(address);
            }
            "-h" | "--help" => return Ok(Command::Help),
            "-V" | "--version" => return Ok(Command::Version),
            unknown => bail!("unknown argument: {unknown}"),
        }
        index += 1;
    }

    match mode.as_deref().unwrap_or("stdio") {
        "stdio" => Ok(Command::Run(Transport::Stdio, preview_bind)),
        "http" => {
            let bind = bind.parse::<SocketAddr>()?;
            if !path.starts_with('/') || path.len() < 2 {
                bail!("HTTP path must start with '/' and contain an endpoint name");
            }
            if path.trim_end_matches('/') == crate::preview::PATH {
                bail!("MCP path conflicts with /preview.mjpg");
            }
            Ok(Command::Run(
                Transport::Http { bind, path },
                preview_bind,
            ))
        }
        other => bail!("unsupported transport: {other}"),
    }
}

fn value<'a>(args: &'a [String], index: usize, option: &str) -> Result<&'a str> {
    args.get(index)
        .map(String::as_str)
        .ok_or_else(|| anyhow::anyhow!("missing value for {option}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_to_stdio() {
        assert_eq!(
            parse(&["rdp-mcp".into()]).unwrap(),
            Command::Run(Transport::Stdio, None)
        );
    }

    #[test]
    fn parses_http_options() {
        assert_eq!(
            parse(&[
                "rdp-mcp".into(),
                "http".into(),
                "--bind".into(),
                "0.0.0.0:9000".into(),
                "--path".into(),
                "/rdp".into(),
            ])
            .unwrap(),
            Command::Run(
                Transport::Http {
                    bind: "0.0.0.0:9000".parse().unwrap(),
                    path: "/rdp".into(),
                },
                None,
            )
        );
    }

    #[test]
    fn parses_preview_and_rejects_conflicting_path() {
        let args = |items: &[&str]| items.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        assert_eq!(
            parse(&args(&["rdp-mcp", "--preview-bind", "127.0.0.1:8001"])).unwrap(),
            Command::Run(Transport::Stdio, Some("127.0.0.1:8001".parse().unwrap()))
        );
        assert!(parse(&args(&["rdp-mcp", "--preview-bind", "0.0.0.0:8001"])).is_err());
        assert!(parse(&args(&["rdp-mcp", "--preview-bind"])).is_err());
        assert!(parse(&args(&["rdp-mcp", "http", "--path", "/preview.mjpg"])).is_err());
    }
}
