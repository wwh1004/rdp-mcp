use std::net::SocketAddr;

use anyhow::{Result, bail};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Transport {
    Stdio,
    Http { bind: SocketAddr, path: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Run(Transport),
    Help,
    Version,
}

pub const HELP: &str = "rdp-mcp - MCP server for headless RDP control

Usage:
  rdp-mcp [stdio]
  rdp-mcp http [--bind <IP:PORT>] [--path <PATH>]
  rdp-mcp --transport <stdio|http> [--bind <IP:PORT>] [--path <PATH>]

Options:
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
            "-h" | "--help" => return Ok(Command::Help),
            "-V" | "--version" => return Ok(Command::Version),
            unknown => bail!("unknown argument: {unknown}"),
        }
        index += 1;
    }

    match mode.as_deref().unwrap_or("stdio") {
        "stdio" => Ok(Command::Run(Transport::Stdio)),
        "http" => {
            let bind = bind.parse::<SocketAddr>()?;
            if !path.starts_with('/') || path.len() < 2 {
                bail!("HTTP path must start with '/' and contain an endpoint name");
            }
            Ok(Command::Run(Transport::Http { bind, path }))
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
            Command::Run(Transport::Stdio)
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
            Command::Run(Transport::Http {
                bind: "0.0.0.0:9000".parse().unwrap(),
                path: "/rdp".into(),
            })
        );
    }
}
