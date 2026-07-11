# Rua Closures and Fused Iterators

Status: implemented in Phase 4A.

This RFC fixes the syntax, semantic boundary, and Lua performance contract for
the Phase 4A closure and iterator work. It does not extend Rua with Rust
ownership, borrowing, or the full standard-library iterator trait hierarchy.

## 1. Closure Syntax

Phase 4A accepts:

~~~rust
|x| x + 1
|x, y| x + y
|x: i64| -> i64 { x + 1 }
|| 42
~~~

The parameter list is enclosed by two pipe tokens. Each parameter may have a
type annotation. The return type is optional and follows the closing pipe.
The body is either one expression or a block.

The pipe token starts a closure only in expression-prefix position. Existing
logical-or (||) and pattern-or (|) parsing keeps its current meaning in their
respective grammar positions.

Phase 4A does not accept:

- move closures;
- explicit Fn, FnMut, or FnOnce bounds;
- async closures;
- closure parameter patterns beyond identifiers;
- closure values that escape the supported immediate-use contexts.

## 2. Closure Types and Capture

Parameter types are determined in this order:

1. an explicit parameter annotation;
2. an expected callable type from the call site;
3. the iterator adapter's current item type;
4. Unknown when none of the above proves a type.

Return types combine all reachable expression/block exits. An explicit return
annotation is checked against the inferred result. Analysis must return
Unknown rather than inventing a precise type when inference is incomplete.

Read-only capture of an enclosing local is supported and lowers to a Lua
upvalue. Mutation through a captured mutable binding is supported only while
the closure remains inside an immediately consumed, statically fused iterator
plan. Other mutable or escaping captures produce a diagnostic.

## 3. Iterator Surface

Sources:

- exclusive and inclusive integer ranges;
- Vec<T>;
- value.iter();
- value.into_iter().

Adapters:

- map;
- filter;
- filter_map;
- enumerate;
- take;
- skip.

Consumers:

- for;
- collect::<Vec<_>>();
- fold;
- count;
- any;
- all;
- find.

Adapters do not execute or allocate by themselves. They extend an internal
IterPlan containing the source, current item type, ordered adapters, closure
bodies, and final consumer.

## 4. Type Rules

- Range items are i64; both bounds must be integer-compatible.
- Vec<T>::iter() and Vec<T>::into_iter() yield T in Phase 4A.
- map(F) changes the item type to F's return type.
- filter(F) preserves the item type and requires a bool return.
- filter_map(F) requires Option<U> and changes the item type to U.
- enumerate() yields a zero-based (i64, T) pair.
- take and skip require a non-negative integer-compatible count.
- collect::<Vec<U>>() requires the final item type to be compatible with U.
- fold(init, F) requires F(acc, item) to return the accumulator type.
- any and all require a boolean predicate.
- find returns Option<T>.

## 5. Lua Lowering Contract

Known plans are fused at the consumer:

- ranges use one Lua numeric for;
- vectors use 'for __i = 0, vec.n - 1 do', never '#vec';
- adapters become locals, conditions, and counters inside that same loop;
- collect allocates exactly one result rt.vec();
- fold and count use local accumulators;
- any, all, and find use early exit.

Fused output must not contain:

- an intermediate Vec between adapters;
- a coroutine;
- one runtime iterator object per adapter;
- one Lua closure call per item per adapter;
- a generic per-item dispatcher.

## 6. Escape Policy

An iterator plan escapes when it is stored without an immediate consumer,
passed as an ordinary argument, returned, or captured by another value. Phase
4A emits 'iterator escape is not supported yet'. It does not silently
materialize a Vec or introduce a pull-iterator runtime. A future RFC may add a
fallback protocol without changing the fused fast path.

Standalone closures may be invoked immediately or used in the compiler-known
contexts introduced by this phase. General first-class closure escape remains
out of scope for the same reason.

## 7. Golden Contract

The active Phase 4A fixtures are under
tests/golden/phase4a/{compile-pass,compile-fail}. The normal golden test checks
that every case remains registered and runs all pass/fail fixtures by default.

Compile-pass cases have exact .lua.golden output and compile-fail cases have
exact .diag.golden diagnostics. Iterator codegen goldens additionally assert
the forbidden Lua shapes from section 5, a single fused loop, and one result
Vec allocation for collect.
