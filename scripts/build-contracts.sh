#!/usr/bin/env bash
# Builds every NME contract to contracts/target/near/<crate>/<crate>.wasm (non-reproducible; fine for dev/tests).
set -euo pipefail
cd "$(dirname "$0")/../contracts"
# Test-only arena variant (adds `test_buy_and_burn`): built first, copied aside, never deployed.
echo "== building nme-arena (test-hooks variant)"
(cd nme-arena && cargo near build non-reproducible-wasm --features test-hooks >/dev/null && cp ../target/near/nme_arena/nme_arena.wasm ../target/near/nme_arena/nme_arena_test.wasm)
for c in nme-crown-vault nme-coin nme-arena nme-crown nme-gateway nme-factory; do
  echo "== building $c"
  (cd "$c" && cargo near build non-reproducible-wasm >/dev/null)
done
ls -la target/near/*/*.wasm
