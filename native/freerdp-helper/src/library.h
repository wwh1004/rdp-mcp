/**
 * rdp-mcp native C ABI.
 *
 * This file adapts conduit-desktop's freerdp-helper into an in-process static
 * library. The current upstream helper is process-global, so one active RDP
 * connection is supported per rdp-mcp process.
 */

#ifndef RDP_MCP_NATIVE_LIBRARY_H
#define RDP_MCP_NATIVE_LIBRARY_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef void (*rdp_mcp_native_output_fn)(uint32_t type,
                                         const void *part_a,
                                         uint32_t part_a_length,
                                         const void *part_b,
                                         uint32_t part_b_length,
                                         void *user_data);

/** Initialize the native runtime and register its output callback. */
int rdp_mcp_native_initialize(rdp_mcp_native_output_fn callback, void *user_data);

/** Parse and execute one NUL-terminated UTF-8 helper protocol command. */
int rdp_mcp_native_command(const char *json_command);

/** Disconnect and release all native runtime resources. */
void rdp_mcp_native_shutdown(void);

/** Return non-zero while the RDP transport is connected. */
int rdp_mcp_native_is_connected(void);

/** Return the stable ABI version implemented by this library. */
uint32_t rdp_mcp_native_abi_version(void);

#ifdef __cplusplus
}
#endif

#endif /* RDP_MCP_NATIVE_LIBRARY_H */
