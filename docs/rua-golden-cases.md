# Rua Golden 用例清单

> 状态：施工清单。
> 目的：补足现有 `tests/fixtures/examples/*.rua` 覆盖不足的问题，为 `ruac` oracle、双 parser conformance、IDE parity 提供系统化 fixture。

## 1. 目录约定

迁到 `/Users/bruce/GitProjects/rua` 后建议落地为：

```text
tests/golden/
  compile-pass/
  compile-fail/
  parser/accept/
  parser/reject/
  parser/ranges/
  modules/
  ruai/
  ide/
  COVERAGE.md
```

命名规则：

- 一个 golden 文件只覆盖一个主行为。
- 可编译用例：`compile-pass/<case>.rua` + `compile-pass/<case>.lua.golden`。
- 拒绝用例：`compile-fail/<case>.rua` + `compile-fail/<case>.diag.golden`。
- 多文件模块用例放在独立目录：`modules/<case>/main.rua`。
- `.ruai` 用例放在独立目录：`ruai/<case>/workspace/`、`ruai/<case>/library/`、`ruai/<case>/std/`。

## 2. Compile-Pass Golden

最低目标：30 个。若闭包/iterator 进入实现范围，建议先生成以下 58 个。

| Case | Path | 覆盖点 |
| --- | --- | --- |
| CP001 | `compile-pass/expr_precedence.rua` | arithmetic/comparison/bool precedence |
| CP002 | `compile-pass/expr_unary_bool.rua` | unary `-` / `!`, bool lowering |
| CP003 | `compile-pass/expr_call_field_index.rua` | call, field access, index access |
| CP004 | `compile-pass/expr_struct_literal_ambiguity.rua` | `Ident { .. }` expression context |
| CP005 | `compile-pass/stmt_let_mut_assign.rua` | `let`, `let mut`, assignment |
| CP006 | `compile-pass/stmt_block_tail.rua` | block tail expression |
| CP007 | `compile-pass/stmt_return_explicit.rua` | explicit `return` |
| CP008 | `compile-pass/control_if_expr_temp.rua` | `if` expression assigned to temp |
| CP009 | `compile-pass/control_if_tail_return.rua` | `if` as function tail |
| CP010 | `compile-pass/control_while_break_continue.rua` | `while`, `break`, `continue` |
| CP011 | `compile-pass/function_zero_arg.rua` | zero-arg function and `main` call |
| CP012 | `compile-pass/function_typed_params.rua` | typed params and return type |
| CP013 | `compile-pass/function_recursion.rua` | recursion |
| CP014 | `compile-pass/function_mutual_recursion.rua` | mutual recursion predeclare |
| CP015 | `compile-pass/struct_literal_fields.rua` | struct declaration/literal/field access |
| CP016 | `compile-pass/struct_assoc_fn.rua` | associated function lowering |
| CP017 | `compile-pass/struct_method_self.rua` | `&self` method colon call |
| CP018 | `compile-pass/struct_method_mut_self.rua` | `&mut self` method if supported |
| CP019 | `compile-pass/enum_unit_tuple_struct.rua` | unit/tuple/struct variants |
| CP020 | `compile-pass/enum_match_bindings.rua` | enum match and bindings |
| CP021 | `compile-pass/match_literals_or_wildcard.rua` | literal/or-pattern/wildcard |
| CP022 | `compile-pass/option_some_none.rua` | `Some` / `None` representation |
| CP023 | `compile-pass/result_ok_err.rua` | `Ok` / `Err` representation |
| CP024 | `compile-pass/result_try_operator.rua` | `?` early return lowering |
| CP025 | `compile-pass/container_vec_basic.rua` | `Vec<T>`, `vec!`, `len`, `get` |
| CP026 | `compile-pass/container_hashmap_basic.rua` | `HashMap<K,V>` if supported |
| CP027 | `compile-pass/module_inline_basic.rua` | inline `mod` |
| CP028 | `compile-pass/module_inline_nested.rua` | nested inline `mod` |
| CP029 | `compile-pass/module_use_alias.rua` | `use`, `use as` |
| CP030 | `compile-pass/module_use_grouped.rua` | grouped `use a::{b, c as d}` |
| CP031 | `compile-pass/visibility_pub_access.rua` | public item access |
| CP032 | `compile-pass/visibility_private_same_module.rua` | private same-module access |
| CP033 | `compile-pass/extern_lua_basic.rua` | `extern "lua"` function |
| CP034 | `compile-pass/extern_lua_variadic.rua` | variadic extern if supported |
| CP035 | `compile-pass/std_println_format.rua` | `println!`, `format!` |
| CP036 | `compile-pass/generic_function_identity.rua` | generic function |
| CP037 | `compile-pass/generic_struct_enum.rua` | generic ADT |
| CP038 | `compile-pass/trait_impl_method.rua` | trait + impl method |
| CP039 | `compile-pass/trait_bound_generic.rua` | bounded generic |
| CP040 | `compile-pass/trait_where_clause.rua` | `where` clause |
| CP041 | `compile-pass/trait_method_generic.rua` | method-level generic |
| CP042 | `compile-pass/comments_whitespace_stability.rua` | comments/spacing should not change semantics |
| CP043 | `compile-pass/closure_expr_inferred.rua` | `|x| x + 1` inferred closure |
| CP044 | `compile-pass/closure_block_typed.rua` | typed block closure `|x: T| -> U { ... }` |
| CP045 | `compile-pass/closure_capture_read.rua` | read-only capture through Lua upvalue |
| CP046 | `compile-pass/closure_capture_mut_immediate.rua` | immediate mutable capture if supported |
| CP047 | `compile-pass/iterator_range_for_exclusive.rua` | `for x in a..b` numeric loop |
| CP048 | `compile-pass/iterator_range_for_inclusive.rua` | `for x in a..=b` numeric loop |
| CP049 | `compile-pass/iterator_vec_for.rua` | `for x in xs.iter()` over `Vec.n` |
| CP050 | `compile-pass/iterator_map_collect_vec.rua` | `.map(|x| ..).collect::<Vec<_>>()` |
| CP051 | `compile-pass/iterator_filter_collect_vec.rua` | `.filter(|x| ..).collect::<Vec<_>>()` |
| CP052 | `compile-pass/iterator_map_filter_fused_for.rua` | map/filter fused into one `for` loop |
| CP053 | `compile-pass/iterator_enumerate.rua` | `.enumerate()` pair/index lowering |
| CP054 | `compile-pass/iterator_take_skip.rua` | `.take()` / `.skip()` loop bounds |
| CP055 | `compile-pass/iterator_fold_sum.rua` | `.fold(init, |acc, x| ..)` accumulator |
| CP056 | `compile-pass/iterator_any_all_find.rua` | `any` / `all` / `find` early break |
| CP057 | `compile-pass/iterator_filter_map_option.rua` | `filter_map` with `Option` |
| CP058 | `compile-pass/iterator_chain_no_materialize.rua` | no intermediate Vec/coroutine for simple chains |

## 3. Multi-File Module Golden

| Case | Path | 覆盖点 |
| --- | --- | --- |
| CM001 | `modules/file_mod_basic/main.rua` | `mod foo;` -> `foo.rua` |
| CM002 | `modules/file_mod_dir/main.rua` | `mod foo;` -> `foo/mod.rua` |
| CM003 | `modules/file_mod_nested/main.rua` | nested file modules |
| CM004 | `modules/file_mod_sibling_use/main.rua` | sibling module call/import |
| CM005 | `modules/file_mod_visibility/main.rua` | public/private across files |
| CM006 | `modules/file_mod_shadowing/main.rua` | local/import/module name shadowing |
| CM007 | `modules/file_mod_struct_enum/main.rua` | cross-file ADT construction |
| CM008 | `modules/file_mod_trait_impl/main.rua` | cross-file trait + impl |

## 4. Compile-Fail Diagnostic Golden

最低目标：30 个。若闭包/iterator 进入实现范围，建议先生成以下 51 个。

| Case | Path | 覆盖点 |
| --- | --- | --- |
| CF001 | `compile-fail/parse_missing_brace.rua` | missing `}` |
| CF002 | `compile-fail/parse_bad_item_start.rua` | invalid item start |
| CF003 | `compile-fail/parse_bad_generic_list.rua` | malformed `<T,>` / nested generic |
| CF004 | `compile-fail/parse_bad_where_clause.rua` | malformed `where` |
| CF005 | `compile-fail/parse_bad_pattern.rua` | malformed pattern |
| CF006 | `compile-fail/name_unresolved_local.rua` | unresolved name |
| CF007 | `compile-fail/name_duplicate_fn.rua` | duplicate function |
| CF008 | `compile-fail/name_duplicate_struct_field.rua` | duplicate field |
| CF009 | `compile-fail/name_duplicate_enum_variant.rua` | duplicate variant |
| CF010 | `compile-fail/name_ambiguous_variant.rua` | ambiguous variant |
| CF011 | `compile-fail/call_wrong_arity_fn.rua` | function arity mismatch |
| CF012 | `compile-fail/call_non_callable.rua` | non-callable callee |
| CF013 | `compile-fail/call_method_not_found.rua` | method not found |
| CF014 | `compile-fail/call_assoc_as_method.rua` | invalid associated fn/method form |
| CF015 | `compile-fail/type_assignment_mismatch.rua` | assignment mismatch |
| CF016 | `compile-fail/type_return_mismatch.rua` | return mismatch |
| CF017 | `compile-fail/type_if_branch_mismatch.rua` | branch type mismatch |
| CF018 | `compile-fail/type_binary_invalid.rua` | invalid binary op |
| CF019 | `compile-fail/type_field_mismatch.rua` | field type mismatch |
| CF020 | `compile-fail/struct_missing_field.rua` | missing field |
| CF021 | `compile-fail/struct_extra_field.rua` | extra field |
| CF022 | `compile-fail/enum_unknown_variant.rua` | unknown variant |
| CF023 | `compile-fail/enum_wrong_tuple_form.rua` | tuple variant built as struct/unit |
| CF024 | `compile-fail/enum_wrong_struct_form.rua` | struct variant built as tuple/unit |
| CF025 | `compile-fail/pattern_variant_arity.rua` | pattern arity mismatch |
| CF026 | `compile-fail/module_missing_file.rua` | missing file module |
| CF027 | `compile-fail/module_private_item_access.rua` | private item access |
| CF028 | `compile-fail/module_invalid_use.rua` | invalid import |
| CF029 | `compile-fail/module_import_private.rua` | importing private item |
| CF030 | `compile-fail/trait_unknown_bound.rua` | unknown trait bound |
| CF031 | `compile-fail/trait_impl_missing_method.rua` | missing trait method |
| CF032 | `compile-fail/trait_bound_unsatisfied.rua` | call-site bound not satisfied |
| CF033 | `compile-fail/trait_method_generic_mismatch.rua` | method-level generic mismatch |
| CF034 | `compile-fail/generic_inference_conflict.rua` | generic type conflict |
| CF035 | `compile-fail/result_try_on_non_result.rua` | `?` on non-Result |
| CF036 | `compile-fail/result_try_return_mismatch.rua` | `?` error type mismatch |
| CF037 | `compile-fail/option_some_wrong_arity.rua` | `Some` arity |
| CF038 | `compile-fail/result_ok_err_wrong_arity.rua` | `Ok`/`Err` arity |
| CF039 | `compile-fail/extern_wrong_abi.rua` | invalid extern ABI if checked |
| CF040 | `compile-fail/std_builtin_wrong_usage.rua` | builtin misuse |
| CF041 | `compile-fail/closure_param_cannot_infer.rua` | closure parameter type cannot be inferred |
| CF042 | `compile-fail/closure_return_mismatch.rua` | closure return type mismatch |
| CF043 | `compile-fail/closure_mut_capture_invalid.rua` | unsupported mutable capture |
| CF044 | `compile-fail/closure_escape_unsupported.rua` | unsupported escaping closure |
| CF045 | `compile-fail/iterator_non_iterable_source.rua` | `for x in value` where value is not iterable |
| CF046 | `compile-fail/iterator_map_arg_not_closure.rua` | `.map()` argument is not closure/function |
| CF047 | `compile-fail/iterator_filter_not_bool.rua` | filter predicate does not return bool |
| CF048 | `compile-fail/iterator_collect_type_mismatch.rua` | collect target type mismatch |
| CF049 | `compile-fail/range_bound_type_mismatch.rua` | range bounds are not compatible integers |
| CF050 | `compile-fail/for_pattern_mismatch.rua` | `for` pattern does not match item type |
| CF051 | `compile-fail/iterator_escape_unsupported.rua` | iterator chain escapes when fallback unsupported |

## 5. Parser / Range Golden

最低目标：20 个。建议先生成以下 24 个。

| Case | Path | 覆盖点 |
| --- | --- | --- |
| PR001 | `parser/accept/comments.rua` | line/block comments if supported |
| PR002 | `parser/accept/string_escapes.rua` | string escapes |
| PR003 | `parser/accept/numeric_literals.rua` | int/float literals |
| PR004 | `parser/accept/keyword_vs_ident.rua` | keyword boundary |
| PR005 | `parser/accept/generic_nested_types.rua` | nested generic types |
| PR006 | `parser/accept/where_clause.rua` | `where` clause |
| PR007 | `parser/accept/receiver_forms.rua` | `self`, `&self`, `&mut self` |
| PR008 | `parser/accept/extern_block.rua` | extern block |
| PR009 | `parser/accept/grouped_use.rua` | grouped use |
| PR010 | `parser/accept/struct_literal_vs_block.rua` | `Ident {}` ambiguity |
| PR011 | `parser/accept/closure_expr.rua` | `|x| x + 1` |
| PR012 | `parser/accept/closure_typed_block.rua` | typed closure with block body |
| PR013 | `parser/accept/range_expr.rua` | `a..b` / `a..=b` |
| PR014 | `parser/accept/iterator_chain.rua` | `.map(|x| ..).filter(|x| ..)` |
| PR015 | `parser/reject/incomplete_fn.rua` | recovery: incomplete fn |
| PR016 | `parser/reject/incomplete_call.rua` | recovery: incomplete call |
| PR017 | `parser/reject/incomplete_closure.rua` | recovery: incomplete closure |
| PR018 | `parser/reject/missing_comma.rua` | recovery: missing comma |
| PR019 | `parser/reject/missing_brace.rua` | recovery: missing brace |
| PR020 | `parser/ranges/fn_item.range` | fn item name/body range |
| PR021 | `parser/ranges/struct_item.range` | struct/field range |
| PR022 | `parser/ranges/enum_variant.range` | enum/variant range |
| PR023 | `parser/ranges/trait_impl.range` | trait/impl method range |
| PR024 | `parser/ranges/path_qualified.range` | qualified path range |
| PR025 | `parser/ranges/call_method.range` | call/member/method range |
| PR026 | `parser/ranges/field_access.range` | field access range |
| PR027 | `parser/ranges/index.range` | index range |
| PR028 | `parser/ranges/closure.range` | closure params/body range |
| PR029 | `parser/ranges/range_expr.range` | range operator range |
| PR030 | `parser/ranges/iterator_adapter.range` | adapter call and closure arg range |
| PR031 | `parser/ranges/match_arm.range` | match arm/pattern range |
| PR032 | `parser/ranges/use_alias.range` | use alias range |

## 6. `.ruai` / External Library Golden

| Case | Path | 覆盖点 |
| --- | --- | --- |
| RI001 | `ruai/library_decl_basic/` | `.ruai` function/type visible to `.rua` |
| RI002 | `ruai/library_decl_module_dir/` | directory root exposes `name/mod.ruai` |
| RI003 | `ruai/library_mount_single_file/` | explicit mount to module name |
| RI004 | `ruai/workspace_shadows_library/` | workspace `.rua` wins over library `.ruai` |
| RI005 | `ruai/library_shadows_std/` | library root wins over std/prelude |
| RI006 | `ruai/goto_hover_signature/` | hover/goto/signature from declaration |
| RI007 | `ruai/completion_members/` | member completion from declaration type |
| RI008 | `ruai/rename_readonly_rejected/` | rename/code action does not edit library root |
| RI009 | `ruai/declaration_codegen_skip/` | declaration file skipped by codegen |
| RI010 | `ruai/declaration_invalid_body/` | body in declaration rejected if forbidden |

## 7. IDE Snapshot Golden

| Case | Path | 覆盖点 |
| --- | --- | --- |
| IDE001 | `ide/completion_local.snap` | local completion |
| IDE002 | `ide/completion_member_struct.snap` | struct member completion |
| IDE003 | `ide/completion_member_trait.snap` | trait method completion |
| IDE004 | `ide/completion_module_path.snap` | module path completion |
| IDE005 | `ide/hover_local_type.snap` | local type hover |
| IDE006 | `ide/hover_function_signature.snap` | function signature hover |
| IDE007 | `ide/hover_ruai_doc.snap` | declaration doc hover |
| IDE008 | `ide/goto_local.snap` | local goto |
| IDE009 | `ide/goto_cross_file.snap` | cross-file goto |
| IDE010 | `ide/goto_ruai.snap` | goto declaration file |
| IDE011 | `ide/references_local.snap` | local references |
| IDE012 | `ide/references_cross_file.snap` | cross-file references |
| IDE013 | `ide/rename_local.snap` | local rename edits |
| IDE014 | `ide/rename_cross_file.snap` | cross-file rename edits |
| IDE015 | `ide/rename_ruai_readonly.snap` | readonly declaration rename rejection |
| IDE016 | `ide/diagnostics_fast.snap` | analysis diagnostics |
| IDE017 | `ide/semantic_tokens.snap` | semantic token output |
| IDE018 | `ide/inlay_hints.snap` | inlay hints |

## 8. Coverage Rules

新增语言能力时必须更新 `tests/golden/COVERAGE.md`：

```text
Feature | compile-pass | compile-fail | parser/range | IDE snapshot | Notes
```

合并门槛：

- 新语法：至少 parser accept/reject + compile-pass 或 compile-fail。
- 新 typeck 行为：至少 compile-pass + compile-fail + parity note。
- 新 codegen 行为：至少 compile-pass `.lua.golden`。
- 新 IDE 行为：至少 one cursor-marker snapshot。
- 新 `.ruai` 行为：至少 compiler + IDE 两侧 golden。

## 9. Iterator Codegen 性能断言

iterator golden 不能只验证运行结果，还要验证生成 Lua 的形态。对可静态融合的链，`.lua.golden` 必须满足：

- range source 使用 Lua numeric `for`。
- Vec source 使用 `for __i = 0, vec.n - 1 do`，不用 `#vec`。
- `map/filter/filter_map/enumerate/take/skip` 在同一个 loop 中完成。
- `collect::<Vec<_>>()` 只在最终 consumer 处分配 `rt.vec()`。
- `fold/count/any/all/find` 使用局部 accumulator/early-break，不 materialize 中间集合。
- 不出现 coroutine。
- 不出现每个 adapter 一个运行期 iterator object。
- 不出现每元素调用通用 dispatcher 的热路径。

允许 fallback 的场景：

- iterator chain 被存储到变量后多次使用。
- iterator chain 被作为参数传递或从函数返回。
- adapter 组合当前无法静态分析。

fallback 必须二选一：

- 明确 diagnostic：`iterator escape is not supported yet`。
- 或生成小型 runtime pull-iterator，并在 golden 中标记这是 fallback，不影响 fused fast path。
