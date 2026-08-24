#!/usr/bin/env python3
"""Verify rdp-mcp against Windows 10 through the MCP stdio protocol.

The script opens the Run dialog, launches Notepad, types ``Hello World!``, and
saves screenshots plus a redacted JSON operation log for every visible step.
It uses only the Python standard library.
"""

from __future__ import annotations

import argparse
import base64
import getpass
import json
import subprocess
import sys
import threading
import time
from datetime import UTC, datetime
from pathlib import Path
from typing import Any


EXPECTED_TOOLS = {
    "connection_list",
    "connection_open",
    "connection_close",
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


class McpStdioClient:
    def __init__(self, executable: Path) -> None:
        self._next_id = 1
        self._stderr: list[str] = []
        self.process = subprocess.Popen(
            [str(executable), "stdio"],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            encoding="utf-8",
            bufsize=1,
        )
        threading.Thread(target=self._read_stderr, daemon=True).start()

    def _read_stderr(self) -> None:
        assert self.process.stderr is not None
        for line in self.process.stderr:
            line = line.rstrip()
            self._stderr.append(line)
            print(f"[server] {line}", file=sys.stderr)

    def notify(self, method: str, params: dict[str, Any] | None = None) -> None:
        self._write({"jsonrpc": "2.0", "method": method, "params": params or {}})

    def request(self, method: str, params: dict[str, Any] | None = None) -> dict[str, Any]:
        request_id = self._next_id
        self._next_id += 1
        self._write(
            {
                "jsonrpc": "2.0",
                "id": request_id,
                "method": method,
                "params": params or {},
            }
        )
        assert self.process.stdout is not None
        while True:
            line = self.process.stdout.readline()
            if not line:
                detail = "\n".join(self._stderr[-20:])
                raise RuntimeError(f"MCP server exited before response\n{detail}")
            message = json.loads(line)
            if message.get("id") != request_id:
                continue
            if "error" in message:
                raise RuntimeError(json.dumps(message["error"], ensure_ascii=False))
            return message["result"]

    def call_tool(self, name: str, arguments: dict[str, Any]) -> dict[str, Any]:
        result = self.request("tools/call", {"name": name, "arguments": arguments})
        if result.get("isError"):
            text = "\n".join(
                block.get("text", "")
                for block in result.get("content", [])
                if block.get("type") == "text"
            )
            raise RuntimeError(text or f"MCP tool failed: {name}")
        return result

    def _write(self, message: dict[str, Any]) -> None:
        if self.process.stdin is None:
            raise RuntimeError("MCP server stdin is closed")
        self.process.stdin.write(json.dumps(message, separators=(",", ":")) + "\n")
        self.process.stdin.flush()

    def close(self) -> None:
        if self.process.stdin:
            self.process.stdin.close()
        try:
            self.process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.process.terminate()
            self.process.wait(timeout=5)


class Recorder:
    def __init__(self, output: Path) -> None:
        self.output = output
        self.output.mkdir(parents=True, exist_ok=True)
        self.steps: list[dict[str, Any]] = []

    def record(
        self,
        tool: str,
        purpose: str,
        arguments: dict[str, Any],
        *,
        status: str = "ok",
        error: str | None = None,
    ) -> None:
        safe_arguments = dict(arguments)
        if "password" in safe_arguments:
            safe_arguments["password"] = "***REDACTED***"
        step = {
            "time": datetime.now(UTC).isoformat(),
            "tool": tool,
            "purpose": purpose,
            "arguments": safe_arguments,
            "status": status,
        }
        if error is not None:
            step["error"] = error
        self.steps.append(step)

    def screenshot(
        self,
        client: McpStdioClient,
        connection_id: str,
        filename: str,
        purpose: str,
    ) -> None:
        arguments = {
            "connection_id": connection_id,
            "format": "jpeg",
            "quality": 70,
            "max_width": 1280,
        }
        try:
            result = client.call_tool("rdp_screenshot", arguments)
            images = [
                block for block in result["content"] if block.get("type") == "image"
            ]
            if len(images) != 1 or images[0].get("mimeType") != "image/jpeg":
                raise RuntimeError("rdp_screenshot did not return one JPEG image block")
            image = base64.b64decode(images[0]["data"], validate=True)
            if not image.startswith(b"\xff\xd8"):
                raise RuntimeError("rdp_screenshot returned invalid JPEG bytes")
            (self.output / filename).write_bytes(image)
            self.record("rdp_screenshot", purpose, arguments)
        except Exception as error:
            self.record(
                "rdp_screenshot", purpose, arguments, status="failed", error=str(error)
            )
            raise

    def write_report(
        self, host: str, tools: list[str], failure: str | None = None
    ) -> Path:
        report = self.output / "report.json"
        report.write_text(
            json.dumps(
                {
                    "project": "rdp-mcp",
                    "target": host,
                    "tools": tools,
                    "steps": self.steps,
                    "outcome": "failed" if failure else "success",
                    "failure": failure,
                    "password_saved": False,
                },
                ensure_ascii=False,
                indent=2,
            )
            + "\n",
            encoding="utf-8",
        )
        return report


def structured(result: dict[str, Any]) -> dict[str, Any]:
    value = result.get("structuredContent")
    if isinstance(value, dict):
        return value
    for block in result.get("content", []):
        if block.get("type") == "text":
            value = json.loads(block["text"])
            if isinstance(value, dict):
                return value
    raise RuntimeError("tool result did not contain a structured object")


def call(
    recorder: Recorder,
    client: McpStdioClient,
    tool: str,
    purpose: str,
    arguments: dict[str, Any],
) -> dict[str, Any]:
    try:
        result = client.call_tool(tool, arguments)
        recorder.record(tool, purpose, arguments)
        return structured(result)
    except Exception as error:
        recorder.record(tool, purpose, arguments, status="failed", error=str(error))
        raise


def run(args: argparse.Namespace) -> Path:
    password = args.password or getpass.getpass("RDP password: ")
    timestamp = datetime.now().strftime("%Y%m%d-%H%M%S")
    recorder = Recorder(args.output / timestamp)
    client = McpStdioClient(args.server.resolve())
    connection_id: str | None = None
    tools: list[str] = []
    failure: str | None = None
    try:
        client.request(
            "initialize",
            {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {"name": "rdp-mcp-win10-example", "version": "0.1.0"},
            },
        )
        client.notify("notifications/initialized")
        listed = client.request("tools/list")
        tools = sorted(tool["name"] for tool in listed["tools"])
        if set(tools) != EXPECTED_TOOLS:
            raise RuntimeError(f"unexpected MCP tools: {tools}")

        opened = call(
            recorder,
            client,
            "connection_open",
            "Connect to the Windows 10 RDP host",
            {
                "connection_type": "rdp",
                "host": args.host,
                "port": args.port,
                "username": args.username,
                "password": password,
                "name": "win10-notepad-example",
            },
        )
        connection_id = str(opened["id"])
        time.sleep(2)
        recorder.screenshot(
            client, connection_id, "01-connected.jpg", "Initial Windows desktop"
        )

        call(
            recorder,
            client,
            "rdp_send_key",
            "Open the Windows Run dialog with Win+R",
            {"connection_id": connection_id, "key": "r", "modifiers": ["meta"]},
        )
        time.sleep(1)
        recorder.screenshot(
            client, connection_id, "02-run-dialog.jpg", "Windows Run dialog opened"
        )

        call(
            recorder,
            client,
            "rdp_type",
            "Type the Notepad command",
            {"connection_id": connection_id, "text": "notepad", "delay_ms": 40},
        )
        recorder.screenshot(
            client, connection_id, "03-notepad-command.jpg", "Notepad command entered"
        )
        call(
            recorder,
            client,
            "rdp_send_key",
            "Launch Notepad",
            {"connection_id": connection_id, "key": "Enter"},
        )
        time.sleep(2)
        recorder.screenshot(
            client, connection_id, "04-notepad-open.jpg", "Notepad opened"
        )

        call(
            recorder,
            client,
            "rdp_type",
            "Type Hello World into Notepad",
            {"connection_id": connection_id, "text": "Hello World!", "delay_ms": 60},
        )
        time.sleep(1)
        recorder.screenshot(
            client,
            connection_id,
            "05-hello-world.jpg",
            "Notepad contains Hello World!",
        )
    except Exception as error:
        failure = f"{type(error).__name__}: {error}"
        raise
    finally:
        if connection_id is not None:
            try:
                call(
                    recorder,
                    client,
                    "connection_close",
                    "Close the RDP connection",
                    {"connection_id": connection_id},
                )
            except Exception as error:  # noqa: BLE001 - preserve the primary failure
                print(f"connection cleanup failed: {error}", file=sys.stderr)
        client.close()
        report = recorder.write_report(args.host, tools, failure)
        print(f"tools={len(tools)}")
        print(f"report={report}")

    return report


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("host", help="Windows 10 host or IP address")
    parser.add_argument("username", help="RDP username")
    parser.add_argument("--password", help="RDP password; prompts securely when omitted")
    parser.add_argument("--port", type=int, default=3389)
    parser.add_argument(
        "--server",
        type=Path,
        default=Path("dist/windows-x86_64/rdp-mcp.exe"),
        help="Path to the rdp-mcp executable",
    )
    parser.add_argument(
        "--output", type=Path, default=Path("examples/output"), help="Evidence directory"
    )
    run(parser.parse_args())


if __name__ == "__main__":
    main()
