#!/usr/bin/env bash
# Boundary enforcement for the Rua workspace dependency graph.
#
# Ensures:
#   - ruac stays lightweight (no rowan, rua-ide, or LSP crates)
#   - rua-common stays lightweight (no ruac, rua-ide, or IDE crates)
#   - rua-ide default features stay free of LSP types
#   - No old crate directories remain
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

# 1. ruac must not depend on rowan, rua-ide, or LSP crates
echo -n "  ruac production deps include rowan/IDE/LSP ... "
ruac_tree=$(dependency_tree "ruac" -p ruac -e normal --depth 1)
if echo "$ruac_tree" | rg -q '(^|[[:space:]])(rowan|rua-ide|lsp-types|lsp-server) v'; then
    fail "ruac depends on IDE/LSP crates"
else
    pass
fi

# 2. rua-common must not depend on ruac, rua-ide, rowan, or IDE crates
echo -n "  rua-common production deps are clean ... "
common_tree=$(dependency_tree "rua-common" -p rua-common -e normal --depth 1)
if echo "$common_tree" | rg -q '(^|[[:space:]])(ruac|rua-ide|rowan) v'; then
    fail "rua-common depends on compiler/IDE crates"
else
    pass
fi

# 3. rua-ide default features must not pull in LSP types
echo -n "  rua-ide default features exclude LSP types ... "
ide_tree=$(dependency_tree "rua-ide" -p rua-ide -e normal --depth 1)
if echo "$ide_tree" | rg -q '(^|[[:space:]])(lsp-types|lsp-server) v'; then
    fail "rua-ide default features include LSP types"
else
    pass
fi

# 4. No old crate directories remain
echo -n "  old crate directories are removed ... "
for old_crate in rua-core rua-lex rua-project rua-resources rua-syntax rua-analysis rua-lsp; do
    if test -d "crates/$old_crate"; then
        fail "old crate directory crates/$old_crate still exists"
    fi
done
pass

echo ""
echo -e "${GREEN}All boundary checks passed.${NC}"
