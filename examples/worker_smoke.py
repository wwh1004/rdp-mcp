#!/usr/bin/env python3
"""Verify real worker IPC and process cleanup using local, non-RDP test sockets."""
from __future__ import annotations

import argparse
import ctypes
import json
import os
from pathlib import Path
import queue
import socket
import subprocess
import sys
import threading
import time


def connect_command(port: int) -> dict:
    return {"type": "connect", "config": {
        "host": "127.0.0.1", "port": port, "username": "test", "password": "test",
        "width": 200, "height": 200, "enableNla": True, "skipCertVerification": True,
        "enableGfx": True, "enableH264": False, "enableClipboard": False,
    }}


class Mcp:
    def __init__(self, executable: Path):
        self.process = subprocess.Popen([str(executable), "stdio"], stdin=subprocess.PIPE,
                                        stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                                        text=True, encoding="utf-8")
        self.messages: queue.Queue = queue.Queue()
        self.pending: dict = {}
        self.next_id = 0

        def read():
            try:
                for line in self.process.stdout:
                    self.messages.put(json.loads(line))
            except Exception as error:
                self.messages.put(error)
            self.messages.put(RuntimeError("MCP stdout closed"))

        threading.Thread(target=read, daemon=True).start()
        request = self.send("initialize", {"protocolVersion": "2025-11-25", "capabilities": {},
                            "clientInfo": {"name": "worker-smoke", "version": "1"}})
        assert "result" in self.receive(request)
        self.process.stdin.write(json.dumps({"jsonrpc": "2.0", "method": "notifications/initialized"}) + "\n")
        self.process.stdin.flush()

    def send(self, method: str, params: dict) -> int:
        self.next_id += 1
        self.process.stdin.write(json.dumps({"jsonrpc": "2.0", "id": self.next_id,
                                            "method": method, "params": params}) + "\n")
        self.process.stdin.flush()
        return self.next_id

    def tool(self, name: str, arguments: dict) -> int:
        return self.send("tools/call", {"name": name, "arguments": arguments})

    def receive(self, request_id: int, timeout: float = 8) -> dict:
        deadline = time.monotonic() + timeout
        while request_id not in self.pending:
            message = self.messages.get(timeout=max(0.01, deadline - time.monotonic()))
            if isinstance(message, Exception):
                raise message
            if "id" in message:
                self.pending[message["id"]] = message
        return self.pending.pop(request_id)

    def connections(self) -> list:
        message = self.receive(self.tool("rdp_list", {}))
        return json.loads(message["result"]["content"][0]["text"])["connections"]

    def close(self):
        if self.process.stdin and not self.process.stdin.closed:
            self.process.stdin.close()
        try:
            self.process.wait(timeout=8)
        except subprocess.TimeoutExpired:
            self.process.kill()
            self.process.wait(timeout=3)
            raise
        assert self.process.returncode == 0, self.process.returncode


def listen() -> socket.socket:
    listener = socket.socket()
    listener.bind(("127.0.0.1", 0))
    listener.listen()
    listener.settimeout(8)
    return listener


def open_args(port: int) -> dict:
    return {"host": "127.0.0.1", "port": port, "username": "test", "password": "test"}


def assert_socket_closed(connection: socket.socket):
    connection.settimeout(5)
    try:
        while connection.recv(4096):
            pass
    except ConnectionResetError:
        pass


def check_mcp_workers(executable: Path):
    mcp = Mcp(executable)
    try:
        with listen() as listener:
            first = mcp.tool("rdp_open", open_args(listener.getsockname()[1]))
            with listener.accept()[0] as connection:
                entries = mcp.connections()
                assert len(entries) == 1 and entries[0]["status"] == "connecting", entries
                # A second worker fails while the first worker remains blocked in connect.
                with listen() as unused:
                    refused_port = unused.getsockname()[1]
                failed = mcp.receive(mcp.tool("rdp_open", open_args(refused_port)))
                assert "error" in failed, failed
                assert mcp.connections() == entries
                closed = mcp.receive(mcp.tool("rdp_close", {"connection_id": entries[0]["id"]}))
                assert "result" in closed, closed
                assert "error" in mcp.receive(first)
                assert not mcp.connections()
                assert_socket_closed(connection)

        # EOF from the MCP client must also cancel an in-progress native connect.
        with listen() as listener:
            mcp.tool("rdp_open", open_args(listener.getsockname()[1]))
            with listener.accept()[0] as connection:
                mcp.close()
                assert_socket_closed(connection)
        print("native IPC, concurrent open failure, close, and MCP EOF cleanup: ok")
    finally:
        if mcp.process.poll() is None:
            mcp.close()


def is_alive(pid: int) -> bool:
    if os.name == "nt":
        kernel = ctypes.WinDLL("kernel32", use_last_error=True)
        kernel.OpenProcess.argtypes = [ctypes.c_uint32, ctypes.c_int, ctypes.c_uint32]
        kernel.OpenProcess.restype = ctypes.c_void_p
        kernel.WaitForSingleObject.argtypes = [ctypes.c_void_p, ctypes.c_uint32]
        kernel.CloseHandle.argtypes = [ctypes.c_void_p]
        handle = kernel.OpenProcess(0x100000, 0, pid)  # SYNCHRONIZE
        if not handle:
            return False
        try:
            return kernel.WaitForSingleObject(handle, 0) == 258  # WAIT_TIMEOUT
        finally:
            kernel.CloseHandle(handle)
    try:
        return Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()[0] != "Z"
    except FileNotFoundError:
        return False


def worker_parent(executable: Path, port: int):
    environment = dict(os.environ, RDP_MCP_PARENT_PID=str(os.getpid()))
    worker = subprocess.Popen([str(executable), "--worker"], env=environment,
                              stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                              stderr=subprocess.DEVNULL)
    worker.stdin.write(json.dumps(connect_command(port)).encode() + b"\n")
    worker.stdin.flush()
    print(worker.pid, flush=True)
    sys.stdin.read()


def check_parent_death(executable: Path):
    with listen() as listener:
        parent = subprocess.Popen([sys.executable, __file__, str(executable), "--worker-parent",
                                   str(listener.getsockname()[1])], stdin=subprocess.PIPE,
                                  stdout=subprocess.PIPE, text=True)
        try:
            worker_pid = int(parent.stdout.readline())
            with listener.accept()[0] as connection:
                parent.kill()
                parent.wait(timeout=3)
                deadline = time.monotonic() + 5
                while is_alive(worker_pid) and time.monotonic() < deadline:
                    time.sleep(0.05)
                assert not is_alive(worker_pid), f"worker {worker_pid} survived its parent"
                assert_socket_closed(connection)
            print("parent death during native connect: ok")
        finally:
            if parent.poll() is None:
                parent.kill()
                parent.wait(timeout=3)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("server", type=Path)
    parser.add_argument("--worker-parent", type=int)
    args = parser.parse_args()
    executable = args.server.resolve()
    if args.worker_parent:
        worker_parent(executable, args.worker_parent)
    else:
        check_mcp_workers(executable)
        check_parent_death(executable)
