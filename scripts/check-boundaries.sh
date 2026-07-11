#!/usr/bin/env bash
# Boundary enforcement for the Rua workspace dependency graph.
#
# Ensures:
#   - rua-analysis production deps are free of ruac and LSP types
#   - rua-syntax production deps are free of ruac (when built without legacy feature)
#   - rua-lsp production deps are free of ruac
#   - No legacy semantic facade is imported in production code
#   - No transition module is accessible in production rua-syntax
#
# Usage: bash scripts/check-boundaries.sh
# Exit 0 = clean, exit 1 = violation found.

set -euo pipefail

RED='\033[0;31m'
GREEN='\033[0;32m'
NC='\033[0m' # No Color

pass() { echo -e "${GREEN}PASS${NC} ${1:-}"; }
fail() { echo -e "${RED}FAIL${NC} ${1:-}"; exit 1; }

echo "=== Checking dependency boundaries ==="

# 1. rua-analysis must not depend on ruac in production
echo -n "  rua-analysis production deps include ruac ... "
if cargo tree -p rua-analysis -e normal --depth 1 2>/dev/null | grep -q "ruac"; then
    fail "rua-analysis has ruac in production deps"
else
    pass
fi

# 2. rua-syntax (without default features) must not depend on ruac
echo -n "  rua-syntax (--no-default-features) production deps include ruac ... "
if cargo tree -p rua-syntax --no-default-features -e normal --depth 1 2>/dev/null | grep -q "ruac"; then
    fail "rua-syntax has ruac in production deps"
else
    pass
fi

# 3. rua-lsp must not depend on ruac in production
echo -n "  rua-lsp (lsp feature) production deps include ruac ... "
if cargo tree -p rua-lsp --features lsp -e normal --depth 2 2>/dev/null | grep -q "ruac"; then
    fail "rua-lsp has ruac in production deps"
else
    pass
fi

# 4. rua-analysis must not depend on LSP types in production
echo -n "  rua-analysis production deps include lsp-types or lsp-server ... "
if cargo tree -p rua-analysis -e normal --depth 1 2>/dev/null | grep -qE "lsp-types|lsp-server"; then
    fail "rua-analysis has LSP types in production deps"
else
    pass
fi

# 5. No transition module importable from production rua-syntax
echo -n "  rua-syntax src/ references transition module (without legacy feature) ... "
# The transition module should be gated behind #[cfg(feature = "legacy")]
if grep -rn "mod transition;" crates/rua-syntax/src/lib.rs 2>/dev/null | grep -v "#[cfg(feature" | grep -q "mod transition"; then
    fail "rua-syntax lib.rs declares mod transition without cfg gate"
else
    pass
fi

# 6. No legacy semantic facade imported in rua-lsp or rua-analysis production
echo -n "  legacy facade imports in production code ... "
if rg -n "rua_syntax::(analysis|workspace|nameres)" crates/rua-lsp/src/ crates/rua-analysis/src/ 2>/dev/null | grep -qv "//\|/\*"; then
    fail "legacy semantic facade imported in production code"
else
    pass
fi

# 7. ruac must not depend on rowan, analysis, or LSP crates
echo -n "  ruac production deps are isolated ... "
if cargo tree -p ruac -e normal --depth 1 2>/dev/null | grep -qE "rowan|rua-syntax|rua-analysis|lsp-types|lsp-server"; then
    fail "ruac depends on IDE/LSP crates"
else
    pass
fi

echo ""
echo -e "${GREEN}All boundary checks passed.${NC}"
