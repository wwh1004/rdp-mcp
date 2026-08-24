cmake_minimum_required(VERSION 3.24)
project(rdp-mcp-native C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_POSITION_INDEPENDENT_CODE ON)

option(RDP_MCP_STATIC_DEPS "Link FreeRDP and its dependencies statically" ON)
set(RDP_MCP_RUST_STATICLIB "" CACHE FILEPATH "Path to the Rust rdp_mcp static library")

set(RDP_MCP_NATIVE_SOURCES
    src/library.c
    src/cliprdr.c
    src/cliprdr_file.c
    src/connection.c
    src/disp.c
    src/input.c
    src/output.c
    src/pointer.c
    src/protocol.c
    src/cjson/cJSON.c
)

add_library(rdp_mcp_native STATIC ${RDP_MCP_NATIVE_SOURCES})
add_library(rdp-mcp::native ALIAS rdp_mcp_native)

target_include_directories(rdp_mcp_native
    PUBLIC src
    PRIVATE src/cjson
)

if(WIN32)
    find_package(FreeRDP 3 REQUIRED)
    find_package(WinPR 3 REQUIRED)
    find_package(FreeRDP-Client 3 REQUIRED)

    target_sources(rdp_mcp_native PRIVATE src/cliprdr_win32.c)
    target_include_directories(rdp_mcp_native PRIVATE
        ${FreeRDP_INCLUDE_DIR}
        ${FreeRDP-Client_INCLUDE_DIR}
        ${WinPR_INCLUDE_DIR}
    )
    target_link_libraries(rdp_mcp_native PUBLIC
        freerdp
        freerdp-client
        winpr
        ws2_32
        crypt32
        ole32
        shell32
    )
    target_compile_definitions(rdp_mcp_native PRIVATE
        _CRT_SECURE_NO_WARNINGS
        _WIN32_WINNT=0x0601
    )

    if(MINGW AND RDP_MCP_STATIC_DEPS)
        # FreeRDP's installed API header otherwise annotates static calls as dllimport.
        target_compile_definitions(rdp_mcp_native PRIVATE FREERDP_EXPORTS)
    endif()
else()
    find_package(PkgConfig REQUIRED)
    pkg_check_modules(FREERDP freerdp3)
    if(NOT FREERDP_FOUND)
        pkg_check_modules(FREERDP REQUIRED freerdp0)
    endif()
    pkg_check_modules(FREERDP_CLIENT freerdp-client3)
    if(NOT FREERDP_CLIENT_FOUND)
        pkg_check_modules(FREERDP_CLIENT REQUIRED freerdp-client0)
    endif()
    pkg_check_modules(WINPR winpr3)
    if(NOT WINPR_FOUND)
        pkg_check_modules(WINPR REQUIRED winpr0)
    endif()

    target_include_directories(rdp_mcp_native PRIVATE
        ${FREERDP_INCLUDE_DIRS}
        ${FREERDP_CLIENT_INCLUDE_DIRS}
        ${WINPR_INCLUDE_DIRS}
    )
    target_link_directories(rdp_mcp_native PUBLIC
        ${FREERDP_LIBRARY_DIRS}
        ${FREERDP_CLIENT_LIBRARY_DIRS}
        ${WINPR_LIBRARY_DIRS}
    )
    target_link_libraries(rdp_mcp_native PUBLIC
        ${FREERDP_CLIENT_LIBRARIES}
        ${FREERDP_LIBRARIES}
        ${WINPR_LIBRARIES}
    )
    target_compile_options(rdp_mcp_native PRIVATE
        ${FREERDP_CFLAGS_OTHER}
        ${FREERDP_CLIENT_CFLAGS_OTHER}
        ${WINPR_CFLAGS_OTHER}
    )
endif()

if(MSVC)
    target_compile_options(rdp_mcp_native PRIVATE /W3 /O1)
else()
    target_compile_options(rdp_mcp_native PRIVATE
        -Os
        -ffunction-sections
        -fdata-sections
        -Wall
        -Wextra
        -Wno-unused-parameter
        -Wno-missing-field-initializers
    )
endif()

install(TARGETS rdp_mcp_native ARCHIVE DESTINATION lib)
install(FILES src/library.h DESTINATION include/rdp-mcp)

if(RDP_MCP_RUST_STATICLIB)
    if(NOT EXISTS "${RDP_MCP_RUST_STATICLIB}")
        message(FATAL_ERROR "Rust static library not found: ${RDP_MCP_RUST_STATICLIB}")
    endif()

    add_library(rdp_mcp_rust STATIC IMPORTED GLOBAL)
    set_target_properties(rdp_mcp_rust PROPERTIES
        IMPORTED_LOCATION "${RDP_MCP_RUST_STATICLIB}"
    )

    add_executable(rdp-mcp src/launcher.c)
    target_link_libraries(rdp-mcp PRIVATE rdp_mcp_rust rdp_mcp_native)

    if(WIN32)
        target_link_libraries(rdp-mcp PRIVATE
            bcrypt
            userenv
            advapi32
            ntdll
            ws2_32
        )
    else()
        target_link_libraries(rdp-mcp PRIVATE dl m pthread rt util)
    endif()

    if(MINGW)
        target_link_options(rdp-mcp PRIVATE
            -static
            -static-libgcc
            -Wl,--gc-sections
            -Wl,--strip-all
        )
    elseif(NOT MSVC)
        target_link_options(rdp-mcp PRIVATE
            -Wl,--gc-sections
            -Wl,--strip-all
        )
    endif()

    install(TARGETS rdp-mcp RUNTIME DESTINATION bin)
endif()
