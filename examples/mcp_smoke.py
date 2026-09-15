#!/usr/bin/env python3
"""Smoke-test rdp-mcp over stdio or Streamable HTTP without opening RDP."""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path
from typing import Any


EXPECTED_TOOLS = {
    "rdp_list",
    "rdp_open",
    "rdp_close",
    "rdp_screenshot",
    "rdp_click",
    "rdp_type",
    "rdp_send_key",
    "rdp_mouse_move",
    "rdp_mouse_drag",
    "rdp_mouse_scroll",
    "rdp_resize",
    "rdp_get_dimensions",
}


def request_message(request_id: int, method: str, params: dict[str, Any]) -> dict[str, Any]:
    return {"jsonrpc": "2.0", "id": request_id, "method": method, "params": params}


def initialize_message() -> dict[str, Any]:
    return request_message(
        1,
        "initialize",
        {
            "protocolVersion": "2025-11-25",
            "capabilities": {},
            "clientInfo": {"name": "rdp-mcp-smoke", "version": "0.1.0"},
        },
    )


def check_result(message: dict[str, Any], request_id: int) -> dict[str, Any]:
    if message.get("id") != request_id:
        raise RuntimeError(f"unexpected response id: {message.get('id')!r}")
    if "error" in message:
        raise RuntimeError(json.dumps(message["error"], ensure_ascii=False))
    result = message.get("result")
    if not isinstance(result, dict):
        raise RuntimeError("MCP response has no result object")
    return result


def check_tools(result: dict[str, Any]) -> None:
    tools = result.get("tools")
    if not isinstance(tools, list):
        raise RuntimeError("tools/list returned no tools array")
    names = {str(tool.get("name")) for tool in tools if isinstance(tool, dict)}
    if names != EXPECTED_TOOLS:
        raise RuntimeError(f"unexpected MCP tools: {sorted(names)}")
    print(f"tools={len(names)}")


def run_stdio(server: Path) -> None:
    process = subprocess.Popen(
        [str(server.resolve()), "stdio"],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        encoding="utf-8",
        bufsize=1,
    )
    assert process.stdin is not None
    assert process.stdout is not None

    def exchange(message: dict[str, Any]) -> dict[str, Any]:
        process.stdin.write(json.dumps(message, separators=(",", ":")) + "\n")
        process.stdin.flush()
        line = process.stdout.readline()
        if not line:
            stderr = process.stderr.read() if process.stderr else ""
            raise RuntimeError(f"stdio server exited before responding: {stderr}")
        return json.loads(line)

    try:
        initialized = check_result(exchange(initialize_message()), 1)
        print(f"protocol={initialized['protocolVersion']}")
        process.stdin.write(
            '{"jsonrpc":"2.0","method":"notifications/initialized","params":{}}\n'
        )
        process.stdin.flush()
        check_tools(check_result(exchange(request_message(2, "tools/list", {})), 2))
        listed = check_result(
            exchange(request_message(3, "tools/call", {"name": "rdp_list", "arguments": {}})),
            3,
        )
        if listed.get("isError"):
            raise RuntimeError("rdp_list failed")
        print("rdp_list=ok")
    finally:
        process.stdin.close()
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            process.terminate()
            process.wait(timeout=5)
    if process.returncode != 0:
        stderr = process.stderr.read() if process.stderr else ""
        raise RuntimeError(f"stdio server exited with {process.returncode}: {stderr}")


def decode_http_message(body: bytes, content_type: str) -> dict[str, Any] | None:
    text = body.decode("utf-8")
    if not text.strip():
        return None
    if "text/event-stream" in content_type:
        for line in text.splitlines():
            if line.startswith("data:"):
                payload = line[5:].strip()
                if payload:
                    return json.loads(payload)
        raise RuntimeError(f"SSE response contains no data event: {text}")
    return json.loads(text)


def run_http(server: Path, bind: str, path: str) -> None:
    endpoint = f"http://{bind}{path}"
    loopback_opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
    process = subprocess.Popen(
        [str(server.resolve()), "http", "--bind", bind, "--path", path],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.PIPE,
        text=True,
        encoding="utf-8",
    )
    session_id: str | None = None

    def post(message: dict[str, Any], *, allow_empty: bool = False) -> dict[str, Any] | None:
        headers = {
            "Content-Type": "application/json",
            "Accept": "application/json, text/event-stream",
        }
        if session_id is not None:
            headers["Mcp-Session-Id"] = session_id
        request = urllib.request.Request(
            endpoint,
            data=json.dumps(message, separators=(",", ":")).encode("utf-8"),
            headers=headers,
            method="POST",
        )
        with loopback_opener.open(request, timeout=10) as response:
            nonlocal_session[0] = response.headers.get("Mcp-Session-Id") or nonlocal_session[0]
            decoded = decode_http_message(
                response.read(), response.headers.get("Content-Type", "")
            )
        if decoded is None and not allow_empty:
            raise RuntimeError("HTTP MCP response was empty")
        return decoded

    nonlocal_session: list[str | None] = [None]
    try:
        deadline = time.monotonic() + 10
        while True:
            try:
                response = post(initialize_message())
                session_id = nonlocal_session[0]
                break
            except urllib.error.URLError:
                if process.poll() is not None:
                    stderr = process.stderr.read() if process.stderr else ""
                    raise RuntimeError(f"HTTP server exited before startup: {stderr}")
                if time.monotonic() >= deadline:
                    raise RuntimeError(f"HTTP server did not listen at {endpoint}")
                time.sleep(0.1)

        assert response is not None
        initialized = check_result(response, 1)
        print(f"protocol={initialized['protocolVersion']}")
        if not session_id:
            raise RuntimeError("HTTP MCP initialize returned no Mcp-Session-Id")
        post(
            {"jsonrpc": "2.0", "method": "notifications/initialized", "params": {}},
            allow_empty=True,
        )
        listed = post(request_message(2, "tools/list", {}))
        assert listed is not None
        check_tools(check_result(listed, 2))
        connections = post(
            request_message(3, "tools/call", {"name": "rdp_list", "arguments": {}})
        )
        assert connections is not None
        if check_result(connections, 3).get("isError"):
            raise RuntimeError("rdp_list failed")
        print("rdp_list=ok")
        print(f"session={session_id}")
    finally:
        process.terminate()
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(timeout=5)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "server", nargs="?", type=Path, default=Path("dist/windows-x86_64/rdp-mcp.exe")
    )
    parser.add_argument("--transport", choices=("stdio", "http"), default="stdio")
    parser.add_argument("--bind", default="127.0.0.1:8765")
    parser.add_argument("--path", default="/mcp")
    args = parser.parse_args()
    if args.transport == "stdio":
        run_stdio(args.server)
    else:
        run_http(args.server, args.bind, args.path)
    print(f"transport={args.transport} status=ok")


if __name__ == "__main__":
    try:
        main()
    except Exception as error:
        print(f"smoke test failed: {error}", file=sys.stderr)
        raise
