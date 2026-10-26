#!/usr/bin/env bash
# Run `scripts/action.sh run` as the runner does, and check the job log and GITHUB_OUTPUT.
# The script does not build. IDL_DRIFT_BIN is a directory with idl-drift and idl-verify.
# Usage: IDL_DRIFT_BIN=target/debug bash scripts/test-action.sh
set -euo pipefail

bin=$(realpath -- "${IDL_DRIFT_BIN:?set IDL_DRIFT_BIN to a directory with idl-drift and idl-verify}")
if [[ ! -x $bin/idl-drift || ! -x $bin/idl-verify ]]; then
  echo "$bin does not hold idl-drift and idl-verify" >&2
  exit 2
fi
# Without jq, action.sh prints no findings to the log, so the log checks mean nothing.
if ! command -v jq >/dev/null; then
  echo "test-action.sh needs jq" >&2
  exit 2
fi

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

TMP=$(realpath -- "$(mktemp -d)")
trap 'rm -rf "$TMP"' EXIT
PASS=0
TOTAL=0
IDL=fixtures/manual/00_identical_new.json

# act <NAME=value...>: run the step with only these variables. Sets `code`; the log is $TMP/log.
act() {
  : >"$TMP/output"
  : >"$TMP/summary"
  code=0
  env -i PATH="$PATH" IDL_DRIFT_BIN="$bin" GITHUB_OUTPUT="$TMP/output" \
    GITHUB_STEP_SUMMARY="$TMP/summary" RUNNER_TEMP="$TMP" "$@" \
    bash scripts/action.sh run >"$TMP/log" 2>&1 || code=$?
}

# check <label> <function>: PASS when the function returns 0.
check() {
  TOTAL=$((TOTAL + 1))
  if "$2"; then
    echo "PASS $1"
    PASS=$((PASS + 1))
  else
    echo "FAIL $1 (exit code $code)"
    sed 's/^/    /' "$TMP/log"
  fi
}

stopped_outside() {
  [[ $code == 2 ]] && grep -q 'resolves to a path outside the workspace' "$TMP/log"
}

control_characters() {
  local ws=$TMP/control
  mkdir -p "$ws"
  # TOML escapes for ESC (C0) and U+009B (C1, the 8-bit CSI).
  printf '%s\n' '[programs."x\u001b[31mA\u009b2KB"]' 'amm = "bad"' >"$ws/Anchor.toml"
  act INPUT_COMMAND=source "INPUT_ARGS=$ws"
  [[ $code == 1 ]] && grep -qF 'x?[31mA?2KB' "$TMP/log" &&
    ! LC_ALL=C grep -q -e $'\x1b' -e $'\xc2\x9b' "$TMP/log"
}

idl_symlink() {
  local ws=$TMP/idl-link
  mkdir -p "$ws" "$TMP/outside"
  cp "$IDL" "$TMP/outside/idl.json"
  ln -s "$TMP/outside/idl.json" "$ws/idl.json"
  act GITHUB_WORKSPACE="$ws" INPUT_COMMAND=diff INPUT_IDL="$ws/idl.json" INPUT_OLD="$IDL"
  stopped_outside
}

allow_symlink() {
  local ws=$TMP/allow-link
  mkdir -p "$ws" "$TMP/outside"
  cp "$IDL" "$ws/idl.json"
  : >"$TMP/outside/allow.txt"
  ln -s "$TMP/outside/allow.txt" "$ws/allow.txt"
  act GITHUB_WORKSPACE="$ws" INPUT_COMMAND=diff INPUT_IDL="$ws/idl.json" INPUT_OLD="$IDL" \
    INPUT_ALLOW="$ws/allow.txt"
  stopped_outside
}

sibling_prefix() {
  # The sibling directory name starts with the workspace name.
  local ws=$TMP/ws
  mkdir -p "$ws" "$ws-evil"
  cp "$IDL" "$ws-evil/x.json"
  act GITHUB_WORKSPACE="$ws" INPUT_COMMAND=diff INPUT_IDL="$ws-evil/x.json" INPUT_OLD="$IDL"
  stopped_outside
}

source_drift() {
  act GITHUB_WORKSPACE="$ROOT" INPUT_COMMAND=source INPUT_ARGS=fixtures/source/drift
  [[ $code == 1 ]] && grep -qx 'exit-code=1' "$TMP/output"
}

check "a C0 and a C1 control in a cluster name reach the log as ?" control_characters
check "an idl input that links out of the workspace exits 2" idl_symlink
check "an allow input that links out of the workspace exits 2" allow_symlink
check "a file in <workspace>-evil/ is outside the workspace" sibling_prefix
check "source on fixtures/source/drift exits 1 and sets exit-code=1" source_drift

echo
echo "${PASS}/${TOTAL} passed"
[[ "$PASS" -eq "$TOTAL" ]]
