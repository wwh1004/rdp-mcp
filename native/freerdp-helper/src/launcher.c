/** Minimal C entry point for the Rust static library. */

#ifdef __cplusplus
extern "C" {
#endif
int rdp_mcp_main(int argc, const char *const *argv);
#ifdef __cplusplus
}
#endif

int main(int argc, char **argv) {
    return rdp_mcp_main(argc, (const char *const *)argv);
}
