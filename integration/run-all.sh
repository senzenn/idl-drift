#!/usr/bin/env bash
# End-to-end checks: run each tool on real inputs and assert its exit code.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

OUT="$(mktemp)"
trap 'rm -f "$OUT"' EXIT
PASS=0
TOTAL=0

# expect <label> <exit code> <command...>
expect() {
  local label="$1" want="$2"
  shift 2
  TOTAL=$((TOTAL + 1))
  set +e
  "$@" >"$OUT" 2>&1
  local got=$?
  set -e
  if [[ "$got" -eq "$want" ]]; then
    echo "${label}: exit=${got} PASS"
    PASS=$((PASS + 1))
  else
    echo "${label}: exit=${got} FAIL (expected ${want})"
    sed 's/^/    /' "$OUT"
  fi
}

cargo build --quiet --workspace
DRIFT="$ROOT/target/debug/idl-drift"
VERIFY="$ROOT/target/debug/idl-verify"

echo "== idl-drift diff: integration/scenarios =="
for entry in \
  "anchor_029_to_030|1" \
  "account_removed_collision|1" \
  "mid_optional_account_removed|1" \
  "cpi_event_anchor_to_pinocchio|1" \
  "map_value_widen|1" \
  "truncated_idl|2"; do
  IFS='|' read -r name want <<<"$entry"
  dir="integration/scenarios/$name"
  expect "$name" "$want" "$DRIFT" diff "$dir/old.json" "$dir/new.json"
done

echo
echo "== idl-drift diff: fixtures/manual =="
for entry in \
  "00_identical|0" \
  "01_default_omission|0" \
  "02_new_instruction|0" \
  "03_enum_append|0" \
  "10_field_widen|1" \
  "11_enum_mid_insert|1" \
  "12_arg_added|1" \
  "13_account_reorder|1" \
  "14_disc_len_change|1" \
  "15_alias_retarget|1" \
  "16_collision_introduced|1" \
  "17_disc_reencoded|0"; do
  IFS='|' read -r name want <<<"$entry"
  expect "$name" "$want" "$DRIFT" diff "fixtures/manual/${name}_old.json" "fixtures/manual/${name}_new.json"
done
# A stricter gate: enum append is Dangerous, so it fails only under --fail-on dangerous.
expect "03_enum_append --fail-on dangerous" 1 "$DRIFT" diff --fail-on dangerous fixtures/manual/03_enum_append_old.json fixtures/manual/03_enum_append_new.json
# Malformed input must exit 2, not panic.
for name in 90_broken_json 91_missing_field; do
  expect "$name" 2 "$DRIFT" diff "fixtures/manual/${name}_old.json" fixtures/manual/00_identical_new.json
done

# A real upgrade: the legacy pump.fun IDL against the on-chain 0.30 one.
expect "real pair pump_fun legacy -> 0.30" 1 "$DRIFT" diff fixtures/shipstern/pump_fun.json fixtures/real/pump_fun_onchain.json
# The same program as legacy Anchor and as Codama: names and discriminators must line up.
expect "cross-format limit_order_v1" 0 "$DRIFT" diff fixtures/real/limit_order_v1_onchain.json fixtures/shipstern/limit_order_v1.json

echo
echo "== idl-drift inspect: fixtures/real =="
for f in fixtures/real/*.json; do
  expect "$(basename "$f")" 0 "$DRIFT" inspect "$f"
done

expect "voltr idl.json --layout" 0 "$DRIFT" inspect --layout fixtures/live/voltr_vault/idl.json

echo
echo "== idl-verify: fixtures/live =="
for entry in \
  "voltr_vault|idl.json|0" \
  "voltr_vault|idl.js-codama.json|1" \
  "jupiter_vaults|idl.json|0" \
  "pump_fun|idl.json|1"; do
  IFS='|' read -r program idl want <<<"$entry"
  dir="fixtures/live/$program"
  expect "$program/$idl" "$want" "$VERIFY" --idl "$dir/$idl" --samples "$dir/samples.json"
done
# An IDL is its own upgrade: every account is ok. A samples file with no account exits 2.
expect "voltr_vault --upgrade-impact" 0 "$VERIFY" --idl fixtures/live/voltr_vault/idl.json --samples fixtures/live/voltr_vault/samples.json --upgrade-impact
expect "pump_fun --upgrade-impact (no accounts)" 2 "$VERIFY" --idl fixtures/live/pump_fun/idl.json --samples fixtures/live/pump_fun/samples.json --upgrade-impact

echo
echo "${PASS}/${TOTAL} behaved as expected"
[[ "$PASS" -eq "$TOTAL" ]]
