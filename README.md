# rdp-mcp

`rdp-mcp` is a headless Remote Desktop client exposed as a Model Context
Protocol server. The MCP layer is written in Rust with the official
[`rmcp`](https://github.com/modelcontextprotocol/rust-sdk) SDK. Its native C
layer is adapted from
[`conduit-desktop/freerdp-helper`](https://github.com/advenimus/conduit-desktop/tree/main/freerdp-helper)
and embeds [FreeRDP](https://www.freerdp.com/).

The server supports MCP over stdio and Streamable HTTP. One server manages
multiple independent RDP connections. Each connection runs in a child process
of the same executable, started with the internal `--worker` argument.

The Rust worker calls the existing native C ABI. Commands travel over its stdin
as JSON lines; events travel over stdout as length-prefixed binary messages.
FreeRDP dirty rectangles carry raw RGBA pixels, with H.264 disabled. The parent
continuously reads these events into a separate framebuffer for each connection.
Native logs are redirected to stderr before FreeRDP initializes.

The connection registry is locked only for lookups and changes. Compound input
operations are serialized per connection; other connections, screenshots, and
previews continue independently. A worker crash affects its own connection.
`rdp_list` includes `connecting`, `connected`, and `disconnected` entries;
disconnected entries remain until `rdp_close` removes them. Connection IDs are
unique during the server's lifetime and HTTP MCP clients share the registry.

Opening a connection waits up to 30 seconds for the native connect call to
finish successfully. Input calls report successful writes to the worker, not
per-command execution acknowledgments; native failures arrive as error events.
Resize waits up to 10 seconds for the requested dimensions and reports timeout.
Closing a connection cancels waiting operations and allows the worker three
seconds to shut down before terminating and reaping it. Server shutdown closes
all workers; a worker also exits if its parent disappears, including while a
native connect call is blocked.

## Build in WSL2

The reproducible build must run from the WSL2 native filesystem, not from a
`/mnt/*` path. The script downloads and builds the pinned OpenSSL 3.4.1, zlib
1.3.1, and FreeRDP 3.15.0 sources when required.

On Debian or Ubuntu WSL2, install the build prerequisites and Rust targets:

```sh
sudo dpkg --add-architecture i386
sudo apt update
sudo apt install build-essential cmake curl g++-multilib gcc-multilib git \
  libssl-dev libssl-dev:i386 mingw-w64 ninja-build perl pkg-config \
  zlib1g-dev zlib1g-dev:i386
rustup target add x86_64-pc-windows-gnu i686-pc-windows-gnu \
  x86_64-unknown-linux-gnu i686-unknown-linux-gnu
```

Copy or clone the repository below the WSL home directory, then build one or
all targets:

```sh
cd ~/rdp-mcp
./scripts/build-wsl.sh all
# Or: windows-x86_64, windows-i686, linux-x86_64, linux-i686
```

Each distribution directory contains exactly one executable:

| Target | Output |
| --- | --- |
| Windows x64 | `dist/windows-x86_64/rdp-mcp.exe` |
| Windows x86 | `dist/windows-i686/rdp-mcp.exe` |
| Linux x64 | `dist/linux-x86_64/rdp-mcp` |
| Linux x86 | `dist/linux-i686/rdp-mcp` |

The Windows executables statically link the Rust server, native helper,
FreeRDP, WinPR, OpenSSL, and zlib; the build rejects imports of their runtime
DLLs. Only Windows system DLLs remain. Linux also has one distribution file,
with FreeRDP and WinPR linked statically, but still uses normal host system
libraries such as libc, OpenSSL, and zlib.

### Release size and path privacy

The release profile is optimized for size: Rust uses `opt-level = "z"`, one
codegen unit, aborting panics, symbol stripping, and Thin LTO. C dependencies
use CMake `MinSizeRel`, `-Os`, per-function/data sections, and final-section
garbage collection. The build also omits unused FreeRDP channels and optional
OpenSSL programs, modules, documentation, tests, and legacy provider files.

Pinned-toolchain artifacts verified on 2026-08-24 have these exact sizes:

| Target | Bytes |
| --- | ---: |
| Windows x64 | 9,987,584 |
| Windows x86 | 10,213,888 |
| Linux x64 | 4,551,456 |
| Linux x86 | 4,159,024 |

Only the executable under each `dist/<target>` directory is distributable.
Files such as `librdp-mcp.dll.a` and `librdp_mcp_native.a` in a CMake build
directory are linker intermediates and are neither needed at runtime nor copied
to `dist`.

Rust and C source paths are remapped, dependencies use the neutral compiled-in
prefix `/rdp-mcp`, ELF RPATH is disabled, and PE timestamps are fixed at zero.
Every build scans both ASCII and UTF-16 strings and fails if it finds the WSL
home, project directory, `/mnt/`, `C:\Users\`, or `D:\Projects\`. Repeated
Windows x64 links with the same inputs are byte-for-byte identical.

`RDP_MCP_RUST_OPT_LEVEL=s` (or `0`, `1`, `2`, or `3`) can be used for controlled
measurements; `z` was the smallest tested setting. The baseline deliberately
does not use UPX. Rust `std` cannot be omitted while Tokio, HTTP, image I/O, and
the MCP SDK are used. Nightly `-Z build-std`, cross-language LLVM linker-plugin
LTO, PGO, and identical-code folding require a different or training-dependent
toolchain and are not used by the reproducible release. Disabling additional
TLS/crypto algorithms or RDP channels may reduce size further, but changes
endpoint compatibility and must be treated as a separate product profile.

## Run

Stdio is the default transport:

```sh
rdp-mcp.exe
rdp-mcp.exe stdio
```

Start a Streamable HTTP endpoint at `http://127.0.0.1:8000/mcp`:

```sh
rdp-mcp.exe http
rdp-mcp.exe http --bind 127.0.0.1:8765 --path /mcp
```

The HTTP server does not add authentication. Keep it on loopback or put an
authenticated, TLS-terminating proxy in front of it before exposing it to a
network.

## Live desktop preview (MJPEG)

HTTP mode also serves a selected RDP desktop at
`/preview.mjpg?connection_id=<ID>` on the same listener. For stdio mode, enable
a separate loopback listener:

```powershell
rdp-mcp.exe stdio --preview-bind 127.0.0.1:8001
# Use the id returned by rdp_open or rdp_list:
ffplay -f mpjpeg 'http://127.0.0.1:8001/preview.mjpg?connection_id=rdp_1'
```

For `rdp-mcp.exe http`, use port 8000 in the URL instead.
`--preview-bind` is also available in HTTP mode for an additional listener.
Each preview is bound to the specified existing connection.
Video bytes are sent over HTTP, leaving MCP stdio available for protocol traffic.

Preview samples at up to 10 FPS, at native desktop size and JPEG quality 65.
Each viewer caches its last JPEG and repeats it when the desktop is unchanged.
Encoding runs outside the RDP locks on a blocking worker. Busy sessions skip
samples; slow viewers pull the latest frame without an application frame queue.
There is no preview encoding when no player is connected. Multiple viewers
encode independently. Desktop resizing is reflected in subsequent JPEGs.

Missing `connection_id` returns HTTP 400; an unknown ID returns HTTP 404.
The stream waits for the selected connection's first frame and ends when that
connection closes or disconnects. It never switches to another connection.
Players may retain their last displayed image after the stream ends. The
independent RDP cursor is not composited into the preview, and audio is not included.
The endpoint has the same unauthenticated access as the HTTP MCP server;
`--preview-bind` accepts loopback addresses only.

## MCP tools

The RDP tools and parameter names follow the RDP MCP implementation in the
upstream `conduit-desktop/mcp` directory.

| Tool | Purpose |
| --- | --- |
| `rdp_list` | List all managed connections and their status. |
| `rdp_open` | Open an RDP connection. |
| `rdp_close` | Close an RDP connection. |
| `rdp_screenshot` | Capture JPEG or PNG, optionally cropped or resized. |
| `rdp_click` | Click left, middle, or right at native desktop coordinates. |
| `rdp_type` | Type text with RDP Unicode keyboard events. |
| `rdp_send_key` | Send a scancode key with Ctrl, Alt, Shift, or Meta/Win. |
| `rdp_mouse_move` | Move the remote pointer. |
| `rdp_mouse_drag` | Press, interpolate ten moves, and release a mouse button. |
| `rdp_mouse_scroll` | Scroll vertically or horizontally. |
| `rdp_resize` | Request a desktop resize through Display Control. |
| `rdp_get_dimensions` | Return the current desktop dimensions. |

`rdp_type` encodes text as UTF-16 code units and sends a key-down/key-up pair
for each RDP Unicode event. This avoids dependence on the remote keyboard
layout or IME; supplementary characters are transmitted as UTF-16 surrogate
pairs. The RDP server must advertise Unicode-input support.

`rdp_send_key` is for physical keys and shortcuts. It uses US-layout PS/2
scancodes, accepts the modifiers `ctrl`, `alt`, `shift`, and `meta`, and accepts
the actions `press`, `down`, and `up`. A `press` sends modifiers down in the
given order and releases them in reverse order.

## Examples and verification

The Python examples require only the standard library and can be run with the
workspace `.venv`. Omit `--password` to enter the RDP password through a secure
interactive prompt; reports redact it and never save it.

Smoke-test all 12 tools over stdio or Streamable HTTP without opening RDP:

```powershell
.\.venv\Scripts\python.exe examples\mcp_smoke.py
.\.venv\Scripts\python.exe examples\mcp_smoke.py --transport http
```

Run Rust tests (Python 3 is used for deterministic worker subprocess fixtures):

```sh
cargo test --locked --lib
```

The tests cover independent workers, framebuffer isolation, concurrent input
and screenshots, resize, crash handling, close during connect, and fragmented
or malformed binary messages. They use synthetic desktops and require no RDP
credentials.

Exercise the built executable's real native worker, pipe framing, concurrent
connect failure, and parent-exit cleanup using local TCP test sockets:

```powershell
.\.venv\Scripts\python.exe examples\worker_smoke.py dist\windows-x86_64\rdp-mcp.exe
```

This verifies lifecycle handling without authenticating to an RDP server.

Run the complete Windows workflow. It opens Edge, opens Downloads in Explorer,
creates `helloworld.txt`, types and saves `Hello World!`, and reopens the file.
Every visible step is saved as a screenshot with a redacted JSON report:

```powershell
.\.venv\Scripts\python.exe examples\win10_notepad.py 192.0.2.10 username
```

Exercise Ctrl, Shift, Alt, Win, multi-modifier chords, and explicit modifier
down/up actions with a screenshot after every visible assertion:

```powershell
.\.venv\Scripts\python.exe examples\win10_shortcuts.py 192.0.2.10 username
```

Evidence directories under `examples/output*` are intentionally ignored by
Git because they can contain remote desktop contents.

## Native origin and patch

[`native/freerdp-helper/UPSTREAM.md`](native/freerdp-helper/UPSTREAM.md) records
the exact upstream commit. The complete directly applicable difference from
that upstream `freerdp-helper` is in
[`native/freerdp-helper-rdp-mcp.patch`](native/freerdp-helper-rdp-mcp.patch).

## Why there is no `legacy` folder

OpenSSL 3 normally places optional provider modules in an `ossl-modules`
directory; its `legacy` provider contains older algorithms such as MD4. This
project builds OpenSSL with `no-legacy no-module` so the Windows distribution
can remain one EXE, and builds FreeRDP with `WITH_INTERNAL_MD4`,
`WITH_INTERNAL_MD5`, and `WITH_INTERNAL_RC4` as recommended for legacy RDP
algorithms. FreeRDP/WinPR can still print a warning that the OpenSSL legacy
provider was not loaded. The tested Windows 10 NLA connection works without a
sidecar provider directory. An unusually old endpoint or a specialized
pass-the-hash flow should be tested separately before relying on this build.

## License

Apache-2.0. See [`LICENSE`](LICENSE).
