#!/usr/bin/env python3
"""Exercise every supported RDP modifier family and capture visible proof.

The workflow uses the Windows Run dialog, File Explorer, and Task Manager so it
does not depend on unsaved editor state. It covers Ctrl, Shift, Alt, Win,
Ctrl+Shift, Win+Shift, and explicit modifier down/up actions. It also types BMP
and supplementary Unicode text through the MCP Unicode-input path.
"""

from __future__ import annotations

import argparse
import getpass
import sys
import time
from datetime import datetime
from pathlib import Path

from win10_notepad import EXPECTED_TOOLS, McpStdioClient, Recorder, call


def send_key(
    recorder: Recorder,
    client: McpStdioClient,
    connection_id: str,
    *,
    key: str,
    purpose: str,
    modifiers: list[str] | None = None,
    action: str = "press",
) -> None:
    call(
        recorder,
        client,
        "rdp_send_key",
        purpose,
        {
            "connection_id": connection_id,
            "key": key,
            "modifiers": modifiers or [],
            "action": action,
        },
    )


def press_and_capture(
    recorder: Recorder,
    client: McpStdioClient,
    connection_id: str,
    *,
    key: str,
    modifiers: list[str],
    purpose: str,
    screenshot: str,
    wait: float = 2,
) -> None:
    send_key(
        recorder,
        client,
        connection_id,
        key=key,
        modifiers=modifiers,
        purpose=purpose,
    )
    time.sleep(wait)
    recorder.screenshot(client, connection_id, screenshot, purpose)


def type_text(
    recorder: Recorder,
    client: McpStdioClient,
    connection_id: str,
    *,
    text: str,
    purpose: str,
    screenshot: str,
    delay_ms: int = 40,
) -> None:
    call(
        recorder,
        client,
        "rdp_type",
        purpose,
        {"connection_id": connection_id, "text": text, "delay_ms": delay_ms},
    )
    time.sleep(2)
    recorder.screenshot(client, connection_id, screenshot, purpose)


def release_modifiers(
    recorder: Recorder,
    client: McpStdioClient,
    connection_id: str,
) -> None:
    """Normalize key state left by a previously interrupted RDP client."""
    for key in ("Control", "Shift", "Alt", "Windows"):
        send_key(
            recorder,
            client,
            connection_id,
            key=key,
            action="up",
            purpose=f"Release any stale {key} state before the test",
        )
        time.sleep(0.1)


def explicit_ctrl_l(
    recorder: Recorder,
    client: McpStdioClient,
    connection_id: str,
) -> None:
    """Build Ctrl+L from separate down, press, and up MCP calls."""
    send_key(
        recorder,
        client,
        connection_id,
        key="Control",
        action="down",
        purpose="Explicit action=down for Control",
    )
    time.sleep(0.1)
    try:
        send_key(
            recorder,
            client,
            connection_id,
            key="l",
            purpose="Press L while explicit Control is held",
        )
        time.sleep(0.1)
    finally:
        send_key(
            recorder,
            client,
            connection_id,
            key="Control",
            action="up",
            purpose="Explicit action=up for Control",
        )


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
                "clientInfo": {
                    "name": "rdp-mcp-shortcut-example",
                    "version": "0.1.0",
                },
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
            "Connect to the Windows shortcut-test host",
            {
                "connection_type": "rdp",
                "host": args.host,
                "port": args.port,
                "username": args.username,
                "password": password,
                "name": "win10-shortcut-example",
            },
        )
        connection_id = str(opened["id"])
        time.sleep(6)
        release_modifiers(recorder, client, connection_id)
        recorder.screenshot(
            client,
            connection_id,
            "00-connected.jpg",
            "Connected and normalized stale modifier state",
        )

        press_and_capture(
            recorder,
            client,
            connection_id,
            key="d",
            modifiers=["ctrl", "meta"],
            purpose="Ctrl+Win+D: create an isolated virtual desktop",
            screenshot="00b-clean-virtual-desktop.jpg",
            wait=8,
        )

        press_and_capture(
            recorder,
            client,
            connection_id,
            key="r",
            modifiers=["meta"],
            purpose="Win+R: open the Run dialog",
            screenshot="01-meta-r-run.jpg",
            wait=12,
        )
        press_and_capture(
            recorder,
            client,
            connection_id,
            key="a",
            modifiers=["ctrl"],
            purpose="Ctrl+A: select any previous Run command",
            screenshot="02-ctrl-a-run.jpg",
            wait=1,
        )
        type_text(
            recorder,
            client,
            connection_id,
            text="Alpha Beta 中文 😀",
            purpose="Type a space, BMP Unicode, and a UTF-16 surrogate pair",
            screenshot="03-unicode-text.jpg",
        )
        press_and_capture(
            recorder,
            client,
            connection_id,
            key="Home",
            modifiers=["shift"],
            purpose="Shift+Home: select the whole Unicode Run command",
            screenshot="04-shift-home.jpg",
            wait=2,
        )
        send_key(
            recorder,
            client,
            connection_id,
            key="Right",
            purpose="Right: collapse the selection at its end",
        )
        type_text(
            recorder,
            client,
            connection_id,
            text=" Zeta",
            purpose="Append a second edit for the Ctrl+Z assertion",
            screenshot="05-before-ctrl-z.jpg",
        )
        press_and_capture(
            recorder,
            client,
            connection_id,
            key="z",
            modifiers=["ctrl"],
            purpose="Ctrl+Z: undo the appended Run-dialog edit",
            screenshot="06-ctrl-z-undo.jpg",
            wait=2,
        )

        send_key(
            recorder,
            client,
            connection_id,
            key="a",
            modifiers=["ctrl"],
            purpose="Ctrl+A: select the Unicode assertion text",
        )
        type_text(
            recorder,
            client,
            connection_id,
            text="explorer.exe shell:Downloads",
            purpose="Replace Run text with a deterministic Explorer command",
            screenshot="07-explorer-command.jpg",
        )
        send_key(
            recorder,
            client,
            connection_id,
            key="Enter",
            purpose="Enter: launch Explorer at Downloads",
        )
        time.sleep(12)
        recorder.screenshot(
            client,
            connection_id,
            "08-downloads-open.jpg",
            "Explorer opened at Downloads",
        )

        explicit_ctrl_l(recorder, client, connection_id)
        time.sleep(2)
        recorder.screenshot(
            client,
            connection_id,
            "09-explicit-ctrl-l.jpg",
            "Explicit Control down/L press/Control up selected the address bar",
        )
        type_text(
            recorder,
            client,
            connection_id,
            text="%USERPROFILE%\\Downloads",
            purpose="Type a visible address for another Shift assertion",
            screenshot="10-address-typed.jpg",
            delay_ms=25,
        )
        press_and_capture(
            recorder,
            client,
            connection_id,
            key="Home",
            modifiers=["shift"],
            purpose="Shift+Home: select the Explorer address",
            screenshot="11-shift-home-address.jpg",
            wait=2,
        )
        press_and_capture(
            recorder,
            client,
            connection_id,
            key="Escape",
            modifiers=[],
            purpose="Escape: cancel address editing",
            screenshot="12-address-cancelled.jpg",
            wait=2,
        )

        press_and_capture(
            recorder,
            client,
            connection_id,
            key="Escape",
            modifiers=["ctrl", "shift"],
            purpose="Ctrl+Shift+Esc: open Task Manager",
            screenshot="13-ctrl-shift-esc.jpg",
            wait=12,
        )
        press_and_capture(
            recorder,
            client,
            connection_id,
            key="F4",
            modifiers=["alt"],
            purpose="Alt+F4: close Task Manager",
            screenshot="14-alt-f4.jpg",
            wait=6,
        )
        press_and_capture(
            recorder,
            client,
            connection_id,
            key="m",
            modifiers=["meta"],
            purpose="Win+M: minimize application windows",
            screenshot="15-meta-m.jpg",
            wait=6,
        )
        press_and_capture(
            recorder,
            client,
            connection_id,
            key="m",
            modifiers=["meta", "shift"],
            purpose="Win+Shift+M: restore minimized windows",
            screenshot="16-meta-shift-m.jpg",
            wait=6,
        )
        press_and_capture(
            recorder,
            client,
            connection_id,
            key="e",
            modifiers=["meta"],
            purpose="Win+E: open another File Explorer window",
            screenshot="17-meta-e.jpg",
            wait=12,
        )
        press_and_capture(
            recorder,
            client,
            connection_id,
            key="Tab",
            modifiers=["alt"],
            purpose="Alt+Tab: switch to the previous window",
            screenshot="18-alt-tab.jpg",
            wait=6,
        )
        press_and_capture(
            recorder,
            client,
            connection_id,
            key="d",
            modifiers=["meta"],
            purpose="Win+D: show the desktop",
            screenshot="19-meta-d-desktop.jpg",
            wait=6,
        )
        press_and_capture(
            recorder,
            client,
            connection_id,
            key="d",
            modifiers=["meta"],
            purpose="Win+D: restore the prior windows",
            screenshot="20-meta-d-restored.jpg",
            wait=6,
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
            except Exception as error:  # noqa: BLE001 - preserve primary failure
                print(f"connection cleanup failed: {error}", file=sys.stderr)
        client.close()
        report = recorder.write_report(args.host, tools, failure)
        print(f"tools={len(tools)}")
        print(f"report={report}")

    return report


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("host", help="Windows host or IP address")
    parser.add_argument("username", help="RDP username")
    parser.add_argument("--password", help="RDP password; prompts securely when omitted")
    parser.add_argument("--port", type=int, default=3389)
    parser.add_argument(
        "--server",
        type=Path,
        default=Path("dist/windows-x86_64/rdp-mcp.exe"),
    )
    parser.add_argument(
        "--output",
        type=Path,
        default=Path("examples/output-shortcuts"),
    )
    run(parser.parse_args())


if __name__ == "__main__":
    main()
