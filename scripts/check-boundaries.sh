#!/usr/bin/env bash
# Boundary enforcement for the Rua workspace dependency graph.
#
# Ensures:
#   - rua-analysis production deps are free of ruac and LSP types
#   - rua-syntax default production deps are free of ruac
#   - rua-lsp production deps are free of ruac
#   - No legacy semantic facade or transition module remains
#
# Usage: bash scripts/check-boundaries.sh
# Exit 0 = clean, exit 1 = violation found.

set -euo pipefail

RED='\033[0;31m'
GREEN='\033[0;32m'
NC='\033[0m' # No Color

pass() { echo -e "${GREEN}PASS${NC} ${1:-}"; }
fail() { echo -e "${RED}FAIL${NC} ${1:-}"; exit 1; }

dependency_tree() {
    local label=$1
    shift
    local output
    if ! output=$(cargo tree "$@" 2>&1); then
        echo "$output" >&2
        fail "could not inspect ${label} dependency tree"
    fi
    printf '%s\n' "$output"
}

echo "=== Checking dependency boundaries ==="

# 1. rua-analysis must not depend on ruac in production
echo -n "  rua-analysis production deps include ruac ... "
analysis_tree=$(dependency_tree "rua-analysis" -p rua-analysis -e normal --depth 1)
if rg -q '(^|[[:space:]])ruac v' <<<"$analysis_tree"; then
    fail "rua-analysis has ruac in production deps"
else
    pass
fi

# 2. rua-syntax default features must not depend on ruac
echo -n "  rua-syntax default production deps include ruac ... "
syntax_tree=$(dependency_tree "rua-syntax" -p rua-syntax -e normal --depth 1)
if rg -q '(^|[[:space:]])ruac v' <<<"$syntax_tree"; then
    fail "rua-syntax has ruac in production deps"
else
    pass
fi

# 3. rua-lsp must not depend on ruac in production
echo -n "  rua-lsp (lsp feature) production deps include ruac ... "
lsp_tree=$(dependency_tree "rua-lsp" -p rua-lsp --features lsp -e normal --depth 2)
if rg -q '(^|[[:space:]])ruac v' <<<"$lsp_tree"; then
    fail "rua-lsp has ruac in production deps"
else
    pass
fi

# 4. rua-analysis must not depend on LSP types in production
echo -n "  rua-analysis production deps include lsp-types or lsp-server ... "
if rg -q '(^|[[:space:]])lsp-(types|server) v' <<<"$analysis_tree"; then
    fail "rua-analysis has LSP types in production deps"
else
    pass
fi

# 5. No transition or legacy facade remains in rua-syntax
echo -n "  rua-syntax legacy facade has been deleted ... "
if test -e crates/rua-syntax/src/transition.rs || \
   rg -q 'mod (transition|analysis|workspace|nameres|completion);|feature = "legacy"' \
      crates/rua-syntax/src/lib.rs crates/rua-syntax/Cargo.toml; then
    fail "rua-syntax still exposes a legacy facade"
else
    pass
fi

# 6. No legacy semantic facade imported in rua-lsp or rua-analysis production
echo -n "  legacy facade imports in production code ... "
if rg -n '^[[:space:]]*(pub[[:space:]]+)?use[[:space:]]+rua_syntax::(analysis|workspace|nameres)' \
    crates/rua-lsp/src/ crates/rua-analysis/src/; then
    fail "legacy semantic facade imported in production code"
else
    pass
fi

# 7. ruac must not depend on rowan, analysis, or LSP crates
echo -n "  ruac production deps are isolated ... "
ruac_tree=$(dependency_tree "ruac" -p ruac -e normal --depth 1)
if rg -q '(^|[[:space:]])(rowan|rua-syntax|rua-analysis|lsp-types|lsp-server) v' <<<"$ruac_tree"; then
    fail "ruac depends on IDE/LSP crates"
else
    pass
fi

echo ""
echo -e "${GREEN}All boundary checks passed.${NC}"
