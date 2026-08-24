#!/usr/bin/env bash
# Build the four supported single-file rdp-mcp distributions in WSL2.
set -euo pipefail

OPENSSL_VERSION="3.4.1"
ZLIB_VERSION="1.3.1"
FREERDP_VERSION="3.15.0"
BUILD_CACHE_REVISION="minsize-v4"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
BUILD_ROOT="$PROJECT_DIR/build/wsl-cross"
SOURCE_ROOT="$BUILD_ROOT/sources"
DIST_ROOT="$PROJECT_DIR/dist"
MINGW_TOOLCHAIN="$SCRIPT_DIR/toolchains/mingw-w64.cmake"
LINUX_I686_TOOLCHAIN="$SCRIPT_DIR/toolchains/linux-i686.cmake"
NCPU="${RDP_MCP_BUILD_JOBS:-$(nproc 2>/dev/null || printf '4')}"
RUST_OPT_LEVEL="${RDP_MCP_RUST_OPT_LEVEL:-z}"
CMAKE_BIN="${CMAKE_BIN:-$(command -v cmake || true)}"
NINJA_BIN="${NINJA_BIN:-$(command -v ninja || true)}"
BUILD_USER_HOME="${HOME:?HOME must be set}"
COMMON_SIZE_CFLAGS="-Os -ffunction-sections -fdata-sections -fno-ident"
PRIVATE_PATH_REMAP_CFLAGS="-ffile-prefix-map=$BUILD_USER_HOME=/build -fdebug-prefix-map=$BUILD_USER_HOME=/build -ffile-prefix-map=$PROJECT_DIR=/src/rdp-mcp -fdebug-prefix-map=$PROJECT_DIR=/src/rdp-mcp"

SUPPORTED_TARGETS=(
    windows-x86_64
    windows-i686
    linux-x86_64
    linux-i686
)

usage() {
    printf 'usage: %s [all|windows-x86_64|windows-i686|linux-x86_64|linux-i686|clean]\n' "$0"
}

fail() {
    printf 'error: %s\n' "$*" >&2
    exit 1
}

require_tool() {
    command -v "$1" >/dev/null 2>&1 || fail "required tool not found: $1"
}

if ! grep -qi microsoft /proc/sys/kernel/osrelease 2>/dev/null; then
    fail "this build must run inside WSL2"
fi

case "$PROJECT_DIR" in
    /mnt/*) fail "copy or clone the repository to the WSL2 native filesystem before building" ;;
esac

if [ "${1:-all}" = "clean" ]; then
    [ "$#" -eq 1 ] || { usage >&2; exit 2; }
    rm -rf -- "$BUILD_ROOT" "$DIST_ROOT"
    exit 0
fi

if [ "$#" -gt 1 ]; then
    usage >&2
    exit 2
fi

case "${1:-all}" in
    all) REQUESTED_TARGETS=("${SUPPORTED_TARGETS[@]}") ;;
    windows-x86_64|windows-i686|linux-x86_64|linux-i686)
        REQUESTED_TARGETS=("$1")
        ;;
    *) usage >&2; exit 2 ;;
esac

[ -n "$CMAKE_BIN" ] && [ -x "$CMAKE_BIN" ] || fail "cmake was not found"
[ -n "$NINJA_BIN" ] && [ -x "$NINJA_BIN" ] || fail "ninja was not found"
case "$RUST_OPT_LEVEL" in
    0|1|2|3|s|z) ;;
    *) fail "invalid RDP_MCP_RUST_OPT_LEVEL: $RUST_OPT_LEVEL" ;;
esac
for tool in cargo curl make perl pkg-config sha256sum strings tar; do
    require_tool "$tool"
done

mkdir -p "$BUILD_ROOT" "$SOURCE_ROOT" "$DIST_ROOT"

download_extract() {
    local url="$1"
    local destination="$2"
    local archive="$SOURCE_ROOT/$(basename "$url")"

    if [ -d "$destination" ]; then
        return
    fi

    printf 'Downloading %s\n' "$url"
    curl --fail --location --retry 3 --output "$archive" "$url"
    mkdir -p "$destination"
    tar -xzf "$archive" -C "$destination" --strip-components=1
}

freerdp_source() {
    FREERDP_SOURCE="$SOURCE_ROOT/freerdp-$FREERDP_VERSION"
    download_extract \
        "https://github.com/FreeRDP/FreeRDP/releases/download/${FREERDP_VERSION}/freerdp-${FREERDP_VERSION}.tar.gz" \
        "$FREERDP_SOURCE"
}

freerdp_flags() {
    FREERDP_FLAGS=(
        -DBUILD_SHARED_LIBS=OFF
        -DBUILTIN_CHANNELS=ON
        -DCHANNEL_AINPUT=OFF
        -DCHANNEL_AUDIN=OFF
        -DCHANNEL_CLIPRDR=ON
        -DCHANNEL_DISP=ON
        -DCHANNEL_DRDYNVC=ON
        -DCHANNEL_DRIVE=ON
        -DCHANNEL_ECHO=OFF
        -DCHANNEL_ENCOMSP=OFF
        -DCHANNEL_GEOMETRY=OFF
        -DCHANNEL_LOCATION=OFF
        -DCHANNEL_PARALLEL=OFF
        -DCHANNEL_PRINTER=OFF
        -DCHANNEL_RAIL=OFF
        -DCHANNEL_RDPECAM=OFF
        # FreeRDP 3.15 client-common references RDPEI pen types unconditionally.
        -DCHANNEL_RDPEI=ON
        -DCHANNEL_RDPEMSC=OFF
        -DCHANNEL_RDPDR=ON
        -DCHANNEL_RDPGFX=ON
        # FreeRDP's default settings request the audio playback channel during
        # pre-connect even though this headless client does not consume audio.
        -DCHANNEL_RDPSND=ON
        -DCHANNEL_REMDESK=OFF
        -DCHANNEL_SERIAL=OFF
        -DCHANNEL_SMARTCARD=OFF
        -DCHANNEL_TELEMETRY=OFF
        -DCHANNEL_URBDRC=OFF
        -DCHANNEL_VIDEO=OFF
        -DUSE_UNWIND=OFF
        -DUSE_VERSION_FROM_GIT_TAG=OFF
        -DWITH_AAD=OFF
        -DWITH_ALSA=OFF
        -DWITH_CHANNELS=ON
        -DWITH_CLIENT=ON
        -DWITH_CLIENT_COMMON=ON
        -DWITH_CLIENT_SDL=OFF
        -DWITH_CLIENT_SDL2=OFF
        -DWITH_CLIENT_SDL3=OFF
        -DWITH_CLIENT_WINDOWS=OFF
        -DWITH_CUPS=OFF
        -DWITH_FFMPEG=OFF
        -DWITH_FUSE=OFF
        -DWITH_GFX_H264=OFF
        -DWITH_INTERNAL_MD4=ON
        -DWITH_INTERNAL_MD5=ON
        -DWITH_INTERNAL_RC4=ON
        -DWITH_JPEG=OFF
        -DWITH_JSON_DISABLED=ON
        -DWITH_KRB5=OFF
        -DWITH_MANPAGES=OFF
        -DWITH_MEDIA_FOUNDATION=OFF
        -DWITH_OSS=OFF
        -DWITH_PCSC=OFF
        -DWITH_PKCS11=OFF
        -DWITH_PROXY=OFF
        -DWITH_PULSE=OFF
        -DWITH_SAMPLE=OFF
        -DWITH_SERVER=OFF
        -DWITH_SERVER_INTERFACE=OFF
        -DWITH_SHADOW=OFF
        -DWITH_SIMD=OFF
        -DWITH_SMARTCARD_EMULATE=OFF
        -DWITH_SMARTCARD_PCSC=OFF
        -DWITH_SWSCALE=OFF
        -DWITH_UNICODE_BUILTIN=ON
        -DWITH_VERBOSE_WINPR_ASSERT=OFF
        -DWITH_WAYLAND=OFF
        -DWITH_WEBVIEW=OFF
        -DWITH_WINMM=OFF
        -DWITH_WINPR_TOOLS=OFF
        -DWITH_WINPR_TOOLS_CLI=OFF
        -DWITH_X11=OFF
    )
}

cmake_ninja() {
    local size_cflags="${RDP_MCP_CMAKE_SIZE_CFLAGS:-$COMMON_SIZE_CFLAGS}"
    "$CMAKE_BIN" "$@" \
        -G Ninja \
        -DCMAKE_MAKE_PROGRAM="$NINJA_BIN" \
        -DCMAKE_BUILD_TYPE=MinSizeRel \
        -DCMAKE_SKIP_RPATH=ON \
        -DCMAKE_C_FLAGS_MINSIZEREL="$size_cflags -DNDEBUG" \
        -DCMAKE_CXX_FLAGS_MINSIZEREL="$size_cflags -DNDEBUG"
}

sanitize_windows_archives() {
    local triplet="$1"
    local prefix="$2"
    local archive archive_sections temporary
    local count=0

    # Static FreeRDP/WinPR archives carry PE dllexport directives even though
    # they are linked into an executable. Those directives create a large,
    # unused EXE export table and matching import-library entries.
    while IFS= read -r -d '' archive; do
        case "$archive" in
            "$prefix"/*) ;;
            *) fail "refusing to rewrite archive outside dependency prefix: $archive" ;;
        esac
        # Capture the complete section listing instead of piping objdump into
        # grep -q. That avoids SIGPIPE under pipefail and keeps this rewrite
        # idempotent, so cached archives do not force a different relink.
        archive_sections="$("${triplet}-objdump" -h "$archive" 2>/dev/null || true)"
        if [[ "$archive_sections" != *'.drectve'* ]]; then
            continue
        fi
        temporary="${archive}.rdp-mcp-noexports"
        "${triplet}-objcopy" --remove-section=.drectve "$archive" "$temporary"
        mv -f -- "$temporary" "$archive"
        count=$((count + 1))
    done < <(find "$prefix" -type f -name '*.a' -print0)
    printf '[deps] Sanitized PE export directives in %d static archives\n' "$count"
}

windows_external_prefix() {
    case "$1" in
        windows-x86_64) printf '%s' "${RDP_MCP_WINDOWS_X86_64_PREFIX:-}" ;;
        windows-i686) printf '%s' "${RDP_MCP_WINDOWS_I686_PREFIX:-}" ;;
    esac
}

build_windows_dependencies() {
    local target="$1"
    local triplet="$2"
    local openssl_target="$3"
    local external_prefix
    external_prefix="$(windows_external_prefix "$target")"

    if [ -n "$external_prefix" ]; then
        DEPS_PREFIX="$(cd "$external_prefix" && pwd)"
        [ -f "$DEPS_PREFIX/lib/libfreerdp3.a" ] || \
            fail "prebuilt prefix has no libfreerdp3.a: $DEPS_PREFIX"
        return
    fi

    local deps="$BUILD_ROOT/deps/$target-$BUILD_CACHE_REVISION"
    local openssl_source="$deps/openssl-src"
    local openssl_stage="$deps/openssl-stage"
    local openssl_install_prefix=/rdp-mcp
    local zlib_source="$deps/zlib-src"
    local zlib_build="$deps/zlib-build"
    local freerdp_build="$deps/freerdp-build"
    local -a freerdp_compat_args=()
    DEPS_PREFIX="$deps/install"
    mkdir -p "$deps" "$DEPS_PREFIX"

    # FreeRDP 3.15.0's type probe misdetects the SSIZE_T already supplied by
    # Debian's 32-bit MinGW headers and otherwise emits a conflicting typedef.
    if [ "$target" = "windows-i686" ]; then
        freerdp_compat_args=(
            -DHAVE_SSIZE_T=FALSE
            -DHAVE_WIN_SSIZE_T=TRUE
        )
    fi

    for tool in "${triplet}-gcc" "${triplet}-g++" "${triplet}-windres" \
        "${triplet}-objcopy" "${triplet}-objdump"; do
        require_tool "$tool"
    done

    if [ ! -f "$DEPS_PREFIX/lib/libssl.a" ]; then
        printf '[%s] Building static OpenSSL %s\n' "$target" "$OPENSSL_VERSION"
        download_extract \
            "https://github.com/openssl/openssl/releases/download/openssl-${OPENSSL_VERSION}/openssl-${OPENSSL_VERSION}.tar.gz" \
            "$openssl_source"
        (
            cd "$openssl_source"
            perl Configure "$openssl_target" \
                "--cross-compile-prefix=${triplet}-" \
                "--prefix=$openssl_install_prefix" \
                "--openssldir=$openssl_install_prefix/ssl" \
                --libdir=lib \
                no-apps no-asm no-docs no-legacy no-module no-shared no-tests \
                $COMMON_SIZE_CFLAGS
            make -j"$NCPU"
            make install_sw DESTDIR="$openssl_stage"
            cp -a "$openssl_stage$openssl_install_prefix/." "$DEPS_PREFIX/"
        )
    fi

    if [ ! -f "$DEPS_PREFIX/lib/libzlibstatic.a" ]; then
        printf '[%s] Building static zlib %s\n' "$target" "$ZLIB_VERSION"
        download_extract \
            "https://github.com/madler/zlib/releases/download/v${ZLIB_VERSION}/zlib-${ZLIB_VERSION}.tar.gz" \
            "$zlib_source"
        cmake_ninja -S "$zlib_source" -B "$zlib_build" \
            -DCMAKE_TOOLCHAIN_FILE="$MINGW_TOOLCHAIN" \
            -DMINGW_TRIPLET="$triplet" \
            -DCMAKE_INSTALL_PREFIX="$DEPS_PREFIX" \
            -DCMAKE_INSTALL_LIBDIR=lib \
            -DBUILD_SHARED_LIBS=OFF \
            -DZLIB_BUILD_EXAMPLES=OFF
        "$CMAKE_BIN" --build "$zlib_build" --parallel "$NCPU"
        "$CMAKE_BIN" --install "$zlib_build"
    fi

    if [ ! -f "$DEPS_PREFIX/lib/cmake/FreeRDP3/FreeRDPConfig.cmake" ]; then
        printf '[%s] Building static FreeRDP %s\n' "$target" "$FREERDP_VERSION"
        freerdp_source
        freerdp_flags
        cmake_ninja -S "$FREERDP_SOURCE" -B "$freerdp_build" \
            "${freerdp_compat_args[@]}" \
            -DCMAKE_TOOLCHAIN_FILE="$MINGW_TOOLCHAIN" \
            -DMINGW_TRIPLET="$triplet" \
            -DCMAKE_INSTALL_PREFIX="$DEPS_PREFIX" \
            -DCMAKE_INSTALL_LIBDIR=lib \
            -DCMAKE_PREFIX_PATH="$DEPS_PREFIX" \
            -DOPENSSL_ROOT_DIR="$DEPS_PREFIX" \
            -DOPENSSL_USE_STATIC_LIBS=TRUE \
            -DCMAKE_INTERPROCEDURAL_OPTIMIZATION=OFF \
            -DZLIB_LIBRARY="$DEPS_PREFIX/lib/libzlibstatic.a" \
            -DZLIB_ROOT="$DEPS_PREFIX" \
            "${FREERDP_FLAGS[@]}"
        "$CMAKE_BIN" --build "$freerdp_build" --parallel "$NCPU"
        "$CMAKE_BIN" --install "$freerdp_build"
    fi

    sanitize_windows_archives "$triplet" "$DEPS_PREFIX"
}

build_linux_dependencies() {
    local target="$1"
    local architecture="$2"
    local deps="$BUILD_ROOT/deps/$target-$BUILD_CACHE_REVISION-canonical"
    local freerdp_build="$deps/freerdp-build"
    local canonical_prefix=/rdp-mcp
    DEPS_SYSROOT="$deps/sysroot"
    DEPS_PREFIX="$DEPS_SYSROOT$canonical_prefix"
    mkdir -p "$DEPS_PREFIX"

    if [ "$architecture" = "i686" ]; then
        [ -f /usr/lib/i386-linux-gnu/libssl.so ] || \
            fail "install gcc-multilib g++-multilib libssl-dev:i386 and zlib1g-dev:i386"
        LINUX_OPENSSL_DIR=/usr/lib/i386-linux-gnu
        LINUX_ZLIB=/usr/lib/i386-linux-gnu/libz.so
        LINUX_TOOLCHAIN_ARGS=(-DCMAKE_TOOLCHAIN_FILE="$LINUX_I686_TOOLCHAIN")
    else
        [ -f /usr/lib/x86_64-linux-gnu/libssl.so ] || fail "install libssl-dev"
        LINUX_OPENSSL_DIR=/usr/lib/x86_64-linux-gnu
        LINUX_ZLIB=/usr/lib/x86_64-linux-gnu/libz.so
        LINUX_TOOLCHAIN_ARGS=()
    fi

    if [ ! -f "$DEPS_PREFIX/lib/pkgconfig/freerdp3.pc" ]; then
        printf '[%s] Building static FreeRDP %s\n' "$target" "$FREERDP_VERSION"
        freerdp_source
        freerdp_flags
        cmake_ninja -S "$FREERDP_SOURCE" -B "$freerdp_build" \
            "${LINUX_TOOLCHAIN_ARGS[@]}" \
            -DCMAKE_INSTALL_PREFIX="$canonical_prefix" \
            -DCMAKE_INSTALL_LIBDIR=lib \
            -DOPENSSL_CRYPTO_LIBRARY="$LINUX_OPENSSL_DIR/libcrypto.so" \
            -DOPENSSL_INCLUDE_DIR=/usr/include \
            -DOPENSSL_SSL_LIBRARY="$LINUX_OPENSSL_DIR/libssl.so" \
            -DZLIB_INCLUDE_DIR=/usr/include \
            -DZLIB_LIBRARY="$LINUX_ZLIB" \
            "${FREERDP_FLAGS[@]}"
        "$CMAKE_BIN" --build "$freerdp_build" --parallel "$NCPU"
        DESTDIR="$DEPS_SYSROOT" "$CMAKE_BIN" --install "$freerdp_build"
    fi
}

build_rust() {
    local rust_target="$1"
    local rust_flags="${RUSTFLAGS:-}"
    rust_flags="${rust_flags:+$rust_flags }--remap-path-prefix=$BUILD_USER_HOME=/build"
    rust_flags="$rust_flags --remap-path-prefix=$PROJECT_DIR=/src/rdp-mcp"
    printf '[rust] Building %s static library\n' "$rust_target"
    RUSTFLAGS="$rust_flags" CARGO_PROFILE_RELEASE_OPT_LEVEL="$RUST_OPT_LEVEL" \
        cargo build --locked --release --target "$rust_target" \
        --manifest-path "$PROJECT_DIR/Cargo.toml"
    RUST_STATICLIB="$PROJECT_DIR/target/$rust_target/release/librdp_mcp.a"
    [ -f "$RUST_STATICLIB" ] || fail "Rust static library was not produced: $RUST_STATICLIB"
}

validate_single_output() {
    local output_dir="$1"
    local count
    count="$(find "$output_dir" -maxdepth 1 -type f | wc -l)"
    [ "$count" -eq 1 ] || fail "distribution must contain exactly one file: $output_dir"
}

validate_no_private_paths() {
    local output="$1"
    local marker
    for marker in "$BUILD_USER_HOME" "$PROJECT_DIR" '/mnt/' 'C:\Users\' 'D:\Projects\'; do
        # grep deliberately reads the complete stream. grep -q can close the
        # pipe early and turn a real match into SIGPIPE under set -o pipefail.
        if strings -a "$output" | grep -F "$marker" >/dev/null || \
            strings -a -el "$output" | grep -F "$marker" >/dev/null; then
            fail "private build path remains in output: $marker"
        fi
    done
}

build_windows() {
    local target="$1"
    local triplet rust_target openssl_target output_name
    case "$target" in
        windows-x86_64)
            triplet=x86_64-w64-mingw32
            rust_target=x86_64-pc-windows-gnu
            openssl_target=mingw64
            ;;
        windows-i686)
            triplet=i686-w64-mingw32
            rust_target=i686-pc-windows-gnu
            openssl_target=mingw
            ;;
    esac
    output_name=rdp-mcp.exe

    build_windows_dependencies "$target" "$triplet" "$openssl_target"
    build_rust "$rust_target"

    local native_build="$BUILD_ROOT/native/$target"
    local output_dir="$DIST_ROOT/$target"
    RDP_MCP_CMAKE_SIZE_CFLAGS="$COMMON_SIZE_CFLAGS $PRIVATE_PATH_REMAP_CFLAGS" \
    cmake_ninja --fresh -S "$PROJECT_DIR/native/freerdp-helper" -B "$native_build" \
        -DCMAKE_TOOLCHAIN_FILE="$MINGW_TOOLCHAIN" \
        -DMINGW_TRIPLET="$triplet" \
        -DCMAKE_PREFIX_PATH="$DEPS_PREFIX" \
        -DFreeRDP_DIR="$DEPS_PREFIX/lib/cmake/FreeRDP3" \
        -DFreeRDP-Client_DIR="$DEPS_PREFIX/lib/cmake/FreeRDP-Client3" \
        -DWinPR_DIR="$DEPS_PREFIX/lib/cmake/WinPR3" \
        -DOPENSSL_ROOT_DIR="$DEPS_PREFIX" \
        -DOPENSSL_USE_STATIC_LIBS=TRUE \
        -DRDP_MCP_RUST_STATICLIB="$RUST_STATICLIB" \
        -DRDP_MCP_STATIC_DEPS=ON \
        -DZLIB_INCLUDE_DIR="$DEPS_PREFIX/include" \
        -DZLIB_LIBRARY="$DEPS_PREFIX/lib/libzlibstatic.a"
    "$CMAKE_BIN" --build "$native_build" --parallel "$NCPU"

    rm -rf -- "$output_dir"
    mkdir -p "$output_dir"
    install -m 0755 "$native_build/$output_name" "$output_dir/$output_name"

    local imports
    imports="$("${triplet}-objdump" -p "$output_dir/$output_name" | \
        sed -n 's/^[[:space:]]*DLL Name:[[:space:]]*//p')"
    if printf '%s\n' "$imports" | \
        grep -Eiq 'freerdp|winpr|libssl|libcrypto|zlib|libgcc|libstdc|libwinpthread'; then
        printf '%s\n' "$imports" >&2
        fail "$target imports a non-system runtime DLL"
    fi
    local pe_sections
    pe_sections="$("${triplet}-objdump" -h "$output_dir/$output_name")"
    if [[ "$pe_sections" == *'.edata'* ]]; then
        fail "$target still exports symbols from statically linked dependencies"
    fi
    validate_no_private_paths "$output_dir/$output_name"
    validate_single_output "$output_dir"
    sha256sum "$output_dir/$output_name"
}

build_linux() {
    local target="$1"
    local architecture rust_target pkg_system_dir
    case "$target" in
        linux-x86_64)
            architecture=x86_64
            rust_target=x86_64-unknown-linux-gnu
            pkg_system_dir=/usr/lib/x86_64-linux-gnu/pkgconfig
            ;;
        linux-i686)
            architecture=i686
            rust_target=i686-unknown-linux-gnu
            pkg_system_dir=/usr/lib/i386-linux-gnu/pkgconfig
            ;;
    esac

    build_linux_dependencies "$target" "$architecture"
    build_rust "$rust_target"

    local native_build="$BUILD_ROOT/native/$target"
    local output_dir="$DIST_ROOT/$target"
    local -a native_toolchain_args=()
    if [ "$architecture" = "i686" ]; then
        native_toolchain_args=(-DCMAKE_TOOLCHAIN_FILE="$LINUX_I686_TOOLCHAIN")
    fi

    PKG_CONFIG_SYSROOT_DIR="$DEPS_SYSROOT" \
    PKG_CONFIG_PATH="$DEPS_PREFIX/lib/pkgconfig" \
    PKG_CONFIG_LIBDIR="$DEPS_PREFIX/lib/pkgconfig:$pkg_system_dir:/usr/share/pkgconfig" \
        RDP_MCP_CMAKE_SIZE_CFLAGS="$COMMON_SIZE_CFLAGS $PRIVATE_PATH_REMAP_CFLAGS" \
        cmake_ninja --fresh -S "$PROJECT_DIR/native/freerdp-helper" -B "$native_build" \
            "${native_toolchain_args[@]}" \
            -DRDP_MCP_RUST_STATICLIB="$RUST_STATICLIB" \
            -DRDP_MCP_STATIC_DEPS=ON
    PKG_CONFIG_SYSROOT_DIR="$DEPS_SYSROOT" \
    PKG_CONFIG_PATH="$DEPS_PREFIX/lib/pkgconfig" \
    PKG_CONFIG_LIBDIR="$DEPS_PREFIX/lib/pkgconfig:$pkg_system_dir:/usr/share/pkgconfig" \
        "$CMAKE_BIN" --build "$native_build" --parallel "$NCPU"

    rm -rf -- "$output_dir"
    mkdir -p "$output_dir"
    install -m 0755 "$native_build/rdp-mcp" "$output_dir/rdp-mcp"

    local needed
    needed="$(readelf -d "$output_dir/rdp-mcp" | sed -n 's/.*Shared library: \[\(.*\)\]/\1/p')"
    if printf '%s\n' "$needed" | grep -Eiq 'freerdp|winpr'; then
        printf '%s\n' "$needed" >&2
        fail "$target imports FreeRDP or WinPR shared libraries"
    fi
    validate_no_private_paths "$output_dir/rdp-mcp"
    validate_single_output "$output_dir"
    "$output_dir/rdp-mcp" --version
    sha256sum "$output_dir/rdp-mcp"
}

printf 'WSL2 native project: %s\n' "$PROJECT_DIR"
printf 'Build targets: %s\n' "${REQUESTED_TARGETS[*]}"
printf 'Rust opt-level: %s\n' "$RUST_OPT_LEVEL"
for target in "${REQUESTED_TARGETS[@]}"; do
    case "$target" in
        windows-*) build_windows "$target" ;;
        linux-*) build_linux "$target" ;;
    esac
done

printf 'Build complete: %s\n' "$DIST_ROOT"
