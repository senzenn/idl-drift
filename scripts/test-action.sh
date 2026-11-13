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

# watch: a git workspace with a local bare origin, a fake `gh` that records each call, and a
# fake `idl-drift watch` that prints two change events. Other idl-drift commands are real.
ADDITIVE=6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P
BREAKING=vVoLTRjQmtFpiYoegx285Ze4gsLJ8ZxgFKVcuvmG1a8
WS=$TMP/watch-ws
FAKE=$TMP/fake
SECRET=ghs_secret

watch_setup() {
  rm -rf "$WS" "$TMP/origin.git" "$FAKE" "$TMP"/gh-*
  mkdir -p "$WS/idls" "$FAKE"
  git init -q --bare "$TMP/origin.git"
  git init -q -b main "$WS"
  cp fixtures/manual/02_new_instruction_old.json "$WS/idls/additive.json"
  cp fixtures/manual/10_field_widen_old.json "$WS/idls/breaking.json"
  printf '%s idls/additive.json\n%s idls/breaking.json\n' "$ADDITIVE" "$BREAKING" >"$WS/programs.txt"
  git -C "$WS" add -A
  git -C "$WS" -c user.name=t -c user.email=t@example.com commit -qm init
  git -C "$WS" remote add origin "$TMP/origin.git"

  # The fake gh keeps the pull requests and issues that it opens, so a later run finds them.
  cat >"$FAKE/gh" <<EOF
#!/usr/bin/env bash
n=\$(cat "$TMP/gh-log" 2>/dev/null | wc -l | tr -d ' ')
printf '%q ' "\$@" >>"$TMP/gh-log"
echo >>"$TMP/gh-log"
echo "\${GH_TOKEN:-}" >>"$TMP/gh-token"
for ((i = 1; i < \$#; i++)); do
  j=\$((i + 1))
  case \${!i} in
    --body-file) cp "\${!j}" "$TMP/gh-body-\$n.md" ;;
    --head) head=\${!j} ;;
    --title) title=\${!j} ;;
  esac
done
case "\$1 \$2" in
  "pr list") k=\$(grep -cxF -- "\$head" "$TMP/gh-heads" 2>/dev/null); echo "\${k:-0}" ;;
  "issue list") jq -R '{title: .}' "$TMP/gh-titles" 2>/dev/null | jq -s . ;;
  "pr create") echo "\$head" >>"$TMP/gh-heads"; echo "https://github.com/owner/repo/pull/1" ;;
  "issue create") echo "\$title" >>"$TMP/gh-titles"; echo "https://github.com/owner/repo/issues/2" ;;
esac
EOF
  # The fake watch writes the temp state that --state names, and the slot of FAKE_SLOT. Each
  # run of the tool keeps the token that it gets.
  cat >"$FAKE/idl-drift" <<EOF
#!/usr/bin/env bash
echo "\${INPUT_TOKEN:-}\${GH_TOKEN:-}" >>"$TMP/tool-token"
if [[ \$1 != watch ]]; then exec "$bin/idl-drift" "\$@"; fi
while [[ \$# -gt 0 ]]; do
  case \$1 in
    --save-onchain) dir=\$2 ;;
    --state) state=\$2 ;;
  esac
  shift
done
mkdir -p "\$dir"
cp "$ROOT/fixtures/manual/02_new_instruction_new.json" "\$dir/$ADDITIVE-anchor.json"
cp "$ROOT/fixtures/manual/10_field_widen_new.json" "\$dir/$BREAKING-anchor.json"
slot=\${FAKE_SLOT:-101}
mkdir -p "\$(dirname "\$state")"
printf '{"version":1,"programs":[{"program":"%s","idl":"idls/additive.json","deploy_slot":100,"idls":[]},{"program":"%s","idl":"idls/breaking.json","deploy_slot":60,"idls":[]}]}\n' $ADDITIVE $BREAKING >"\$state"
event() {
  printf '{"event":"change","program":"%s","idl":"idls/%s.json","idl_path":"$WS/idls/%s.json","previous":null,"deploy_slot":%s,"status":"%s","idls":[{"source":"anchor","account":"Acc","slot":%s,"status":"%s"}],"onchain":{"source":"anchor","account":"Acc","saved":"%s/%s-anchor.json"},"diff":{"changes":[],"counts":{"breaking":%s,"dangerous":0,"additive":%s,"cosmetic":0},"highest":"%s"}}\n' "\$@"
}
event $ADDITIVE additive additive 100 fresh "\$slot" fresh "\$dir" $ADDITIVE 0 1 additive
event $BREAKING breaking breaking 60 stale "\$slot" stale "\$dir" $BREAKING 1 0 breaking
EOF
  chmod +x "$FAKE/gh" "$FAKE/idl-drift"
}

# act_watch <NAME=value...>: the watch step on the workspace, with the fakes.
act_watch() {
  act PATH="$FAKE:$PATH" IDL_DRIFT_BIN="$FAKE" GITHUB_WORKSPACE="$WS" INPUT_COMMAND=watch \
    INPUT_PROGRAMS="$WS/programs.txt" INPUT_STATE="$WS/.idl-watch/state.json" \
    INPUT_RPC_URL=http://127.0.0.1:1 GITHUB_REPOSITORY=owner/repo GITHUB_REF_NAME=main "$@"
}

# The first 12 hex digits of the sha256 of a file, which name the branch and the issue.
short_sha() {
  if command -v sha256sum >/dev/null; then sha256sum "$1"; else shasum -a 256 "$1"; fi | cut -c1-12
}
BRANCH="idl-drift/$ADDITIVE-$(short_sha fixtures/manual/02_new_instruction_new.json)"
ISSUE_TITLE="idl-drift: the on-chain IDL of $BREAKING has breaking changes (IDL $(short_sha fixtures/manual/10_field_widen_new.json))"

# The line numbers of the end of the stop-commands span and of the Stale warning.
warning_after_the_span() {
  local token end warning
  token=$(sed -n 's/^::stop-commands:://p' "$TMP/log")
  end=$(grep -nxF "::$token::" "$TMP/log" | cut -d: -f1)
  warning=$(grep -n "^::warning title=Stale on-chain IDL::Program $BREAKING" "$TMP/log" | cut -d: -f1)
  [[ -n $end && -n $warning && $warning -gt $end ]]
}

watch_opens_a_pr_and_an_issue() {
  watch_setup
  local head
  head=$(git -C "$WS" rev-parse HEAD)
  act_watch INPUT_TOKEN="$SECRET"
  [[ $code == 0 ]] || return 1
  grep -q "^pr list --repo owner/repo --head $BRANCH --state all" "$TMP/gh-log" &&
    grep -q "^pr create --repo owner/repo --base main --head $BRANCH --title Update\\\\ the\\\\ IDL" "$TMP/gh-log" &&
    grep -qF "issue create --repo owner/repo --title $(printf '%q' "$ISSUE_TITLE")" "$TMP/gh-log" || return 1
  # The branch holds the on-chain IDL at the local path, on top of HEAD; the checkout is untouched.
  git -C "$TMP/origin.git" show "$BRANCH:idls/additive.json" |
    cmp -s - <(jq . fixtures/manual/02_new_instruction_new.json) &&
    [[ $(git -C "$TMP/origin.git" rev-parse "$BRANCH^") == "$head" ]] &&
    [[ -z $(git -C "$WS" status --porcelain --untracked-files=no) ]] || return 1
  # gh gets the token, and the state keeps both programs after their pull request or issue.
  [[ $(sort -u "$TMP/gh-token") == "$SECRET" ]] &&
    [[ $(jq '.programs | length' "$WS/.idl-watch/state.json") == 2 ]] || return 1
  grep -q '| Breaking | 1 |' "$TMP"/gh-body-*.md && warning_after_the_span &&
    grep -q 'pull request opened from' "$TMP/summary" && grep -q 'issue opened' "$TMP/summary"
}

watch_keys_the_bot_on_the_idl_text() {
  # A transaction that only names the IDL account moves its slot, not its text.
  watch_setup
  act_watch INPUT_TOKEN=t FAKE_SLOT=101
  act_watch INPUT_TOKEN=t FAKE_SLOT=102
  [[ $code == 0 ]] && [[ $(grep -c '^pr create ' "$TMP/gh-log") == 1 ]] &&
    [[ $(grep -c '^issue create ' "$TMP/gh-log") == 1 ]]
}

watch_skips_an_existing_pr_and_issue() {
  watch_setup
  echo "$BRANCH" >"$TMP/gh-heads"
  echo "$ISSUE_TITLE" >"$TMP/gh-titles"
  act_watch INPUT_TOKEN=t
  [[ $code == 0 ]] && ! grep -q ' create ' "$TMP/gh-log" &&
    ! git -C "$TMP/origin.git" rev-parse -q --verify "$BRANCH" >/dev/null
}

watch_replaces_a_bot_branch_with_no_pr() {
  # A failed `gh pr create` leaves the branch with no pull request.
  watch_setup
  local other
  other=$(git -C "$WS" -c user.name=t -c user.email=t@example.com commit-tree "$(git -C "$WS" mktree </dev/null)" -m other)
  git -C "$WS" push -q origin "$other:refs/heads/$BRANCH"
  act_watch INPUT_TOKEN=t
  [[ $code == 0 ]] && grep -q "^pr create .*--head $BRANCH" "$TMP/gh-log" &&
    git -C "$TMP/origin.git" show "$BRANCH:idls/additive.json" | cmp -s - <(jq . fixtures/manual/02_new_instruction_new.json)
}

watch_without_a_token_exits_2() {
  # Nothing was opened, so the state keeps no program and the next run tries again.
  watch_setup
  act_watch
  [[ $code == 2 && ! -f $TMP/gh-log ]] && grep -qx 'exit-code=2' "$TMP/output" &&
    grep -q 'give the token input' "$TMP/summary" &&
    [[ ! -f $WS/.idl-watch/state.json || $(jq '.programs | length' "$WS/.idl-watch/state.json") == 0 ]]
}

non_watch_gets_no_token() {
  # action.yml gives the token input only to watch, and no tool gets it from the script.
  watch_setup
  grep -qF "INPUT_TOKEN: \${{ inputs.command == 'watch' && inputs.token || '' }}" action.yml || return 1
  rm -f "$TMP/tool-token"
  act IDL_DRIFT_BIN="$FAKE" GITHUB_WORKSPACE="$ROOT" INPUT_COMMAND=diff INPUT_IDL="$IDL" \
    INPUT_OLD="$IDL" INPUT_TOKEN="$SECRET"
  [[ $code == 0 && -f $TMP/tool-token && -z $(tr -d '\n' <"$TMP/tool-token") ]]
}

watch_programs_outside() {
  watch_setup
  act_watch INPUT_TOKEN=t INPUT_PROGRAMS="$TMP/control/Anchor.toml"
  stopped_outside
}

check "a C0 and a C1 control in a cluster name reach the log as ?" control_characters
check "an idl input that links out of the workspace exits 2" idl_symlink
check "an allow input that links out of the workspace exits 2" allow_symlink
check "a file in <workspace>-evil/ is outside the workspace" sibling_prefix
check "source on fixtures/source/drift exits 1 and sets exit-code=1" source_drift
check "watch opens a pull request for an additive change and an issue for a breaking one" watch_opens_a_pr_and_an_issue
check "watch opens one pull request and one issue for one IDL text, also when its slot moves" watch_keys_the_bot_on_the_idl_text
check "watch opens no pull request or issue that exists" watch_skips_an_existing_pr_and_issue
check "watch replaces a bot branch that has no pull request" watch_replaces_a_bot_branch_with_no_pr
check "watch without a token opens nothing, keeps no state and exits 2" watch_without_a_token_exits_2
check "a programs input outside the workspace exits 2" watch_programs_outside
check "a command other than watch gets no token" non_watch_gets_no_token

echo
echo "${PASS}/${TOTAL} passed"
[[ "$PASS" -eq "$TOTAL" ]]
