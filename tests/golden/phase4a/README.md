# Phase 4A Goldens

These active fixtures cover closures and fused iterator plans. Their inventory
is enforced by `phase4a_goldens_are_registered`, and the compile-pass/fail
runners execute in the default `ruac` golden test suite.

The 12 compile-pass cases cover closure inference and capture, range/Vec
sources, every Phase 4A adapter, and every consumer. The 9 compile-fail cases
cover inference, capture, escape, source, predicate, argument, and collect
boundaries. Iterator pass cases also assert one fused loop, no coroutine or
legacy iterator calls, and no per-item Lua closure.

The accepted behavior and unsupported boundary are defined in
`docs/rua-closure-iterator-rfc.md`.
