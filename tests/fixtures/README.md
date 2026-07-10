# Shared Test Fixtures

`examples/` contains the legacy Rua source corpus imported from `moon_rs`. The
compiler and syntax tests reuse these files as smoke fixtures; the systematic
oracle corpus lives separately under `tests/golden/`.

Generated Lua output is intentionally not committed. The `.ruai` declaration
and Lua host harness under `examples/rua_moon/` are source fixtures and remain
versioned.
