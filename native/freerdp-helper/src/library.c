/**
 * In-process library adapter for conduit-desktop's freerdp-helper.
 *
 * Modified for rdp-mcp: commands arrive through a C ABI instead of stdin and
 * output is delivered through a callback instead of a binary stdout stream.
 */

#include "library.h"

#include "cliprdr.h"
#include "cliprdr_file.h"
#ifdef _WIN32
#include "cliprdr_win32.h"
#endif
#include "connection.h"
#include "disp.h"
#include "input.h"
#include "output.h"
#include "protocol.h"

#include <stdbool.h>
#include <stdio.h>

#include <winpr/synch.h>
#include <winpr/thread.h>

#ifdef _WIN32
#include <winsock2.h>
#include <ws2tcpip.h>
#endif

#define RDP_MCP_NATIVE_ABI_VERSION 1U

static freerdp *g_instance = NULL;
static volatile bool g_initialized = false;
static volatile bool g_running = false;
static volatile bool g_connected = false;
static HANDLE g_event_thread = NULL;

static DWORD WINAPI event_loop_thread(LPVOID arg) {
    freerdp *instance = (freerdp *)arg;
    int last_diag_gen = 0;
    int diag_check_countdown = 0;

    while (g_running && g_connected) {
        if (!connection_check_events(instance)) {
            g_connected = false;
            break;
        }
        disp_check_pending();

        {
            int gen = 0;
            int frames = 0;
            connection_get_resize_diag(&gen, &frames);
            if (gen != last_diag_gen) {
                last_diag_gen = gen;
                diag_check_countdown = 20;
            }
            if (diag_check_countdown > 0) {
                diag_check_countdown--;
                if (diag_check_countdown == 0 && frames == 0) {
                    fprintf(stderr,
                            "[rdp-mcp-native] no frames received after resize #%d\n",
                            gen);
                }
            }
        }
    }

    if (g_running) {
        output_send_disconnected(NULL);
    }
    return 0;
}

static void stop_connection(void) {
    g_connected = false;

    if (g_event_thread) {
        WaitForSingleObject(g_event_thread, INFINITE);
        CloseHandle(g_event_thread);
        g_event_thread = NULL;
    }

    if (g_instance) {
        connection_free(g_instance);
        g_instance = NULL;
    }
}

static int start_connection(const ConnectConfig *config) {
    if (g_instance || g_event_thread) {
        stop_connection();
    }

    disp_init();
    cliprdr_init();
    cliprdr_file_init();

    g_instance = connection_init(config);
    if (!g_instance) {
        output_send_error("Failed to initialize FreeRDP");
        return -3;
    }

    if (!connection_connect(g_instance)) {
        connection_free(g_instance);
        g_instance = NULL;
        return -4;
    }

    disp_set_initial_layout(config->width,
                            config->height,
                            config->desktop_scale_factor,
                            config->device_scale_factor);

    g_connected = true;
    g_event_thread = CreateThread(NULL, 0, event_loop_thread, g_instance, 0, NULL);
    if (!g_event_thread) {
        output_send_error("Failed to create FreeRDP event thread");
        stop_connection();
        return -5;
    }
    return 0;
}

int rdp_mcp_native_initialize(rdp_mcp_native_output_fn callback, void *user_data) {
    if (!callback) return -1;
    if (g_initialized) return -2;

#ifdef _WIN32
    {
        WSADATA wsa_data;
        if (WSAStartup(MAKEWORD(2, 2), &wsa_data) != 0) return -3;
    }
#endif

    output_init_callback((output_callback_fn)callback, user_data);
    input_init();

#ifdef _WIN32
    cliprdr_win32_init();
#endif

    g_running = true;
    g_connected = false;
    g_initialized = true;
    return 0;
}

int rdp_mcp_native_command(const char *json_command) {
    Command cmd;
    int result = 0;

    if (!g_initialized || !g_running || !json_command) return -1;
    if (!protocol_parse_command(json_command, &cmd)) return -2;

    switch (cmd.type) {
        case CMD_CONNECT:
            if (g_connected) {
                output_send_error("Already connected");
                result = -3;
            } else {
                result = start_connection(&cmd.data.connect);
            }
            break;

        case CMD_DISCONNECT:
            stop_connection();
            break;

        case CMD_MOUSE_MOVE:
            if (g_connected && g_instance) {
                input_mouse_move(g_instance, cmd.data.mouse_move.x, cmd.data.mouse_move.y);
            }
            break;

        case CMD_MOUSE_BUTTON_DOWN:
            if (g_connected && g_instance) {
                input_mouse_button_down(g_instance,
                                        cmd.data.mouse_button.x,
                                        cmd.data.mouse_button.y,
                                        cmd.data.mouse_button.button);
            }
            break;

        case CMD_MOUSE_BUTTON_UP:
            if (g_connected && g_instance) {
                input_mouse_button_up(g_instance,
                                      cmd.data.mouse_button.x,
                                      cmd.data.mouse_button.y,
                                      cmd.data.mouse_button.button);
            }
            break;

        case CMD_MOUSE_SCROLL:
            if (g_connected && g_instance) {
                input_mouse_scroll(g_instance,
                                   cmd.data.mouse_scroll.x,
                                   cmd.data.mouse_scroll.y,
                                   cmd.data.mouse_scroll.delta,
                                   cmd.data.mouse_scroll.vertical);
            }
            break;

        case CMD_KEY_DOWN:
            if (g_connected && g_instance) {
                input_key_down(g_instance, cmd.data.key.scancode, cmd.data.key.extended);
            }
            break;

        case CMD_KEY_UP:
            if (g_connected && g_instance) {
                input_key_up(g_instance, cmd.data.key.scancode, cmd.data.key.extended);
            }
            break;

        case CMD_RESIZE:
            if (g_connected && g_instance) {
                disp_request_resize(cmd.data.resize.width,
                                    cmd.data.resize.height,
                                    cmd.data.resize.desktop_scale_factor,
                                    cmd.data.resize.device_scale_factor);
            }
            break;

        case CMD_CLIPBOARD_SET:
            if (g_connected && g_instance) {
#ifdef _WIN32
                if (!cliprdr_win32_is_active())
#endif
                {
                    cliprdr_set_text(cmd.data.clipboard_set.text,
                                     cmd.data.clipboard_set.length);
                }
            }
            break;

        case CMD_CLIPBOARD_SET_FILES:
            if (g_connected && g_instance) {
#ifdef _WIN32
                if (!cliprdr_win32_is_active())
#endif
                {
                    cliprdr_file_set_local_files(cmd.data.clipboard_set_files.json);
                    cliprdr_announce_files();
                }
            }
            break;

        case CMD_CLIPBOARD_REQUEST_FILES:
            if (g_connected && g_instance) {
                cliprdr_file_request_download(cmd.data.clipboard_request_files.temp_dir);
            }
            break;

        case CMD_UNKNOWN:
        default:
            output_send_error("Unknown native command");
            result = -6;
            break;
    }

    protocol_free_command(&cmd);
    return result;
}

void rdp_mcp_native_shutdown(void) {
    if (!g_initialized) return;

    g_running = false;
    stop_connection();
    cliprdr_file_cleanup();

#ifdef _WIN32
    cliprdr_win32_cleanup();
    WSACleanup();
#endif

    output_cleanup();
    g_initialized = false;
}

int rdp_mcp_native_is_connected(void) {
    return g_connected ? 1 : 0;
}

uint32_t rdp_mcp_native_abi_version(void) {
    return RDP_MCP_NATIVE_ABI_VERSION;
}
