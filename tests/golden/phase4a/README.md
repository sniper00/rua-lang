# Phase 4A Goldens

These active fixtures cover closures and fused iterator plans. Their inventory
is enforced by `phase4a_goldens_are_registered`, and the compile-pass/fail
runners execute in the default `ruac` golden test suite.

The 13 compile-pass cases cover closure inference and capture, range/Vec
sources, every Phase 4A adapter, and every consumer. The 8 compile-fail cases
cover inference, capture, escape, source, predicate, argument, and collect
boundaries. Iterator pass cases also assert one direct fused loop without
coroutine dispatch or per-item Lua closures.

The current language contract is defined in
[`docs/rua-design.md`](../../../docs/rua-design.md).
