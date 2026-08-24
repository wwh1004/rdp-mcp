# Upstream source

The original source is `freerdp-helper` from
<https://github.com/advenimus/conduit-desktop>. The exact upstream baseline is
commit `9aa6ef0505dae22912b454bf22b862d6ddd505cf` (v0.16.2).

The files were initially imported through a native WSL2 checkout at local
commit `b31c267`, which contains two additional Windows build commits. This
project adapts the helper from a standalone executable into a static C library
for `rdp-mcp`.

`../freerdp-helper-rdp-mcp.patch` contains the complete difference from the
exact upstream baseline above to this directory.
