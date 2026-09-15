"""Deterministic worker process for Rust tests; does not connect to RDP."""
import json
import os
import struct
import sys
import time


def event(kind, data=b""):
    if isinstance(data, dict):
        data = json.dumps(data).encode()
    sys.stdout.buffer.write(struct.pack("<II", kind, len(data)) + data)
    sys.stdout.buffer.flush()


width = height = 0
color = b""
for line in sys.stdin.buffer:
    command = json.loads(line)
    kind = command["type"]
    if kind == "connect":
        config = command["config"]
        if config["host"] == "hang":
            time.sleep(60)
            break
        if config["host"] == "fail":
            event(255, {"message": "fixture connection failure"})
            sys.exit(1)
        width, height = config["width"], config["height"]
        color = bytes((255, 0, 0, 255) if config["host"] == "red" else (0, 0, 255, 255))
        event(1, {"width": width, "height": height})
        event(2, struct.pack("<HHHH", 0, 0, width, height) + color * width * height)
        event(0)
    elif kind == "resize":
        width, height = command["width"], command["height"]
        event(4, {"width": width, "height": height})
        event(2, struct.pack("<HHHH", 0, 0, width, height) + color * width * height)
    elif kind == "test_crash":
        os._exit(17)
    elif kind == "disconnect":
        break
