#!/usr/bin/env bash
# The steps of action.yml: `action.sh install` and `action.sh run`.
# action.yml gives each input in an INPUT_* environment variable, never in the script text.
set -euo pipefail

REPO=https://github.com/senzenn/idl-drift

# The runner reads %, CR and LF in the text of a workflow command as markup.
escape_data() {
  local s=${1//'%'/%25}
  s=${s//$'\r'/%0D}
  printf '%s' "${s//$'\n'/%0A}"
}

escape_property() {
  local s
  s=$(escape_data "$1")
  s=${s//:/%3A}
  printf '%s' "${s//,/%2C}"
}

# fail <title> <message>: print an error annotation and stop with exit 2.
fail() {
  echo "::error title=$(escape_property "$1")::$(escape_data "$2")"
  if [[ -n ${GITHUB_OUTPUT:-} ]]; then
    echo "exit-code=2" >>"$GITHUB_OUTPUT"
  fi
  exit 2
}

sha256() {
  if command -v sha256sum >/dev/null; then
    sha256sum "$1" | cut -d' ' -f1
  else
    shasum -a 256 "$1" | cut -d' ' -f1
  fi
}

# Sets `tool` from the command input. Not in a subshell, so that `fail` can stop the step.
set_tool() {
  case ${INPUT_COMMAND:-diff} in
    diff | source | watch) tool=idl-drift ;;
    verify) tool=idl-verify ;;
    *) fail "idl-drift action" "The command input must be diff, source, verify or watch, not \`$INPUT_COMMAND\`." ;;
  esac
}

install() {
  local tool version target dir name url want got
  set_tool
  version=${INPUT_VERSION:-${ACTION_REF:-}}
  if [[ -z $version ]]; then
    fail "idl-drift action" "Set the version input to an idl-drift tag or commit SHA."
  fi
  # The version goes into a URL path, so it must not climb out of the release path.
  if [[ ! $version =~ ^[A-Za-z0-9][A-Za-z0-9._/-]*$ || $version == *..* ]]; then
    fail "idl-drift action" "The version \`$version\` is not a tag, branch or commit SHA."
  fi
  case "${RUNNER_OS:-}/${RUNNER_ARCH:-}" in
    Linux/X64) target=x86_64-unknown-linux-gnu ;;
    Linux/ARM64) target=aarch64-unknown-linux-gnu ;;
    macOS/ARM64) target=aarch64-apple-darwin ;;
    macOS/X64) target=x86_64-apple-darwin ;;
    *) target="" ;;
  esac
  dir=$(mktemp -d "${RUNNER_TEMP:-/tmp}/idl-drift-bin.XXXXXX")

  # Releases are made from tags, so a commit SHA has no release binary.
  if [[ -n $target && ! $version =~ ^[0-9a-f]{40}$ ]]; then
    name="idl-drift-$version-$target"
    url="$REPO/releases/download/$version/$name.tar.gz"
    if curl -fsL --proto '=https' --retry 3 -o "$dir/$name.tar.gz" "$url"; then
      if ! curl -fsSL --proto '=https' --retry 3 -o "$dir/$name.tar.gz.sha256" "$url.sha256"; then
        fail "idl-drift action" "The release $version has $name.tar.gz, but no $name.tar.gz.sha256 to check it."
      fi
      read -r want _ <"$dir/$name.tar.gz.sha256" || true
      got=$(sha256 "$dir/$name.tar.gz")
      if [[ $want != "$got" ]]; then
        fail "idl-drift action" "The sha256 of $name.tar.gz is $got, but the release gives $want. The action does not run this binary."
      fi
      tar -xzf "$dir/$name.tar.gz" -C "$dir"
      echo "Installed the release binaries of idl-drift $version for $target."
      echo "bin=$dir/$name" >>"$GITHUB_OUTPUT"
      return
    fi
    echo "::notice title=idl-drift action::$(escape_data "No release binary of idl-drift $version for $target, so the action builds $tool from source.")"
  fi

  if ! command -v cargo >/dev/null; then
    fail "idl-drift action" "No release binary fits this runner, and cargo is not installed to build $tool. Install Rust before this step."
  fi
  local from
  if [[ $version =~ ^[0-9a-f]{40}$ ]]; then
    from=(--rev "$version")
  elif git ls-remote --exit-code --tags "$REPO" "refs/tags/$version" >/dev/null; then
    from=(--tag "$version")
  else
    from=(--branch "$version")
  fi
  cargo install --locked --git "$REPO" "${from[@]}" --root "$dir" "$tool"
  echo "bin=$dir/bin" >>"$GITHUB_OUTPUT"
}

# Sets `old` to a copy, under `$1`, of the file at the path of INPUT_IDL on the base branch
# of the pull request. Sets it empty when the base branch has no such file.
base_version() {
  local into=$1 dir file rel depth=()
  old=""
  if [[ -z ${BASE_REF:-} ]]; then
    fail "idl-drift could not run" "Set the old or the program input. The action finds the old IDL itself only on a pull_request event."
  fi
  dir=$(dirname "$INPUT_IDL")
  file=$(basename "$INPUT_IDL")
  # A fetch with --depth into a full clone makes it shallow, and later steps can need the history.
  if [[ $(git -C "$dir" rev-parse --is-shallow-repository) == true ]]; then
    depth=(--depth 1)
  fi
  if ! git -C "$dir" fetch -q --no-tags ${depth[@]+"${depth[@]}"} origin \
    "+refs/heads/$BASE_REF:refs/remotes/origin/$BASE_REF"; then
    fail "idl-drift could not run" "git could not fetch the base branch $BASE_REF. Run actions/checkout before this action."
  fi
  if ! git -C "$dir" cat-file -e "origin/$BASE_REF:./$file" 2>/dev/null; then
    return
  fi
  rel="$(git -C "$dir" rev-parse --show-prefix)$file"
  old="$into/$BASE_REF/$rel"
  mkdir -p "$(dirname "$old")"
  git -C "$dir" show "origin/$BASE_REF:./$file" >"$old"
}

# A path input must stay in the checkout. A symlink in a pull request could otherwise make the
# tool read a file on the runner and print a part of it.
in_workspace() {
  [[ -z ${GITHUB_WORKSPACE:-} ]] && return 0
  local real workspace
  real=$(realpath -- "$1" 2>/dev/null) || return 1
  workspace=$(realpath -- "$GITHUB_WORKSPACE") || return 1
  [[ $real == "$workspace" || $real == "$workspace"/* ]]
}

run() {
  local tool bin out report summary cmd extra old live=false samples="" code=0
  local command=${INPUT_COMMAND:-diff}
  # action.yml gives the token only to watch. Only the gh calls get it, in GH_TOKEN.
  local gh_token=${INPUT_TOKEN:-}
  unset INPUT_TOKEN
  set_tool
  bin=${IDL_DRIFT_BIN:?the install step sets IDL_DRIFT_BIN}
  : "${GITHUB_OUTPUT:?}" "${GITHUB_STEP_SUMMARY:?}"
  # source finds the IDLs in the workspace; the idl input only adds one. watch takes none.
  if [[ $command != watch && ($command != source || -n ${INPUT_IDL:-}) && ! -f ${INPUT_IDL:-} ]]; then
    fail "$tool could not run" "The idl input \`${INPUT_IDL:-}\` is not a file. Give the path of the new IDL."
  fi
  local input
  for input in "${INPUT_IDL:-}" "${INPUT_ALLOW:-}"; do
    if [[ -n $input ]] && ! in_workspace "$input"; then
      fail "$tool could not run" "The input \`$input\` resolves to a path outside the workspace."
    fi
  done
  if [[ -n ${INPUT_RPC_URL:-} ]]; then
    # The mask covers one line only, so a line break could print the rest of the URL.
    if [[ $INPUT_RPC_URL == *[$'\r\n']* ]]; then
      fail "$tool could not run" "The rpc-url input holds a line break."
    fi
    echo "::add-mask::$INPUT_RPC_URL"
  fi
  out=$(mktemp -d "${RUNNER_TEMP:-/tmp}/idl-drift.XXXXXX")
  report="$out/report.json"
  summary="$out/summary.md"
  # Split on whitespace only: no globs, quotes or variables.
  read -r -d '' -a extra <<<"${INPUT_ARGS:-}" || true

  if [[ $command == watch ]]; then
    watch_run
    return
  fi
  if [[ $command == source ]]; then
    if [[ -n ${INPUT_OLD:-}${INPUT_PROGRAM:-}${INPUT_ALLOW:-} ]]; then
      fail "idl-drift could not run" "The source command reads the program id from the workspace, and takes no old, program or allow input. Set rpc-url to also check the chain."
    fi
    cmd=(source)
    if [[ -n ${INPUT_IDL:-} ]]; then
      cmd+=(--idl "$INPUT_IDL")
    fi
    if [[ -n ${INPUT_RPC_URL:-} ]]; then
      export IDL_DRIFT_RPC_URL=$INPUT_RPC_URL
      cmd+=(--onchain)
    fi
  elif [[ $tool == idl-drift ]]; then
    cmd=(diff)
    if [[ -n ${INPUT_PROGRAM:-} ]]; then
      if [[ -n ${INPUT_OLD:-} ]]; then
        fail "idl-drift could not run" "Set the old or the program input, not both."
      fi
      export IDL_DRIFT_RPC_URL=${INPUT_RPC_URL:-}
      cmd+=(--onchain "$INPUT_PROGRAM" "$INPUT_IDL")
    else
      old=${INPUT_OLD:-}
      if [[ -z $old ]]; then
        base_version "$out"
        if [[ -z $old ]]; then
          echo "::notice file=$(escape_property "$INPUT_IDL"),title=New IDL::$(escape_data "The base branch $BASE_REF has no file at this path, so idl-drift has nothing to compare.")"
          printf '### idl-drift diff: %s\n\nThe base branch has no file at this path, so idl-drift has nothing to compare.\n' \
            "\`$INPUT_IDL\`" >>"$GITHUB_STEP_SUMMARY"
          echo "exit-code=0" >>"$GITHUB_OUTPUT"
          return
        fi
      fi
      cmd+=("$old" "$INPUT_IDL")
    fi
  else
    export IDL_VERIFY_RPC_URL=${INPUT_RPC_URL:-}
    cmd=(--idl "$INPUT_IDL")
    if [[ -n ${INPUT_PROGRAM:-} ]]; then
      cmd+=(--program "$INPUT_PROGRAM")
    fi
    live=true
    for a in ${extra[@]+"${extra[@]}"}; do
      if [[ $a == --samples || $a == --samples=* ]]; then
        live=false
      fi
    done
    if [[ $live == true && -z ${INPUT_RPC_URL:-} ]]; then
      fail "idl-verify could not run" "Set the rpc-url input to the secret that holds your RPC URL, or give --samples in args."
    fi
  fi
  if [[ -n ${INPUT_FAIL_ON:-} ]]; then
    cmd+=(--fail-on "$INPUT_FAIL_ON")
  fi
  if [[ -n ${INPUT_ALLOW:-} ]]; then
    cmd+=(--allow "$INPUT_ALLOW")
  fi

  # Two runs give the JSON and the Markdown report. A live run samples one time: the
  # second run reads the saved samples, without the limits that only live sampling takes.
  local first=("${cmd[@]}") second=("${cmd[@]}") skip=false
  if [[ $live == true ]]; then
    samples="$out/samples.json"
    first+=(--save-samples "$samples")
    second+=(--samples "$samples")
    for a in ${extra[@]+"${extra[@]}"}; do
      if [[ $skip == true ]]; then
        skip=false
        continue
      fi
      case $a in
        --max-tx | --stall) skip=true ;;
        --max-tx=* | --stall=*) ;;
        *) second+=("$a") ;;
      esac
    done
  else
    second+=(${extra[@]+"${extra[@]}"})
  fi
  first+=(${extra[@]+"${extra[@]}"})

  "$bin/$tool" "${first[@]}" --json >"$report" 2>"$out/stderr.txt" || code=$?
  if [[ $code == 2 && $live == true ]]; then
    printf '**idl-verify could not run (exit code 2).** The cause is in the job log.\n' >"$summary"
  else
    "$bin/$tool" "${second[@]}" --markdown >"$summary" 2>/dev/null || true
  fi
  cat "$summary" >>"$GITHUB_STEP_SUMMARY"

  # IDL text in the log must not run as a workflow command.
  local token
  token=$(od -An -N16 -tx1 /dev/urandom | tr -d ' \n')
  echo "::stop-commands::$token"
  cat "$out/stderr.txt"
  if command -v jq >/dev/null; then
    # A control character in IDL text could move the cursor or add a line in the log. jq's gsub
    # is quadratic in the string length, so map the code points.
    jq -r 'def clean: explode | map(if . < 32 or (. >= 127 and . < 160) then 63 else . end) | implode;
      {breaking: 0, dangerous: 1, additive: 2, cosmetic: 3, error: 0, warning: 1, note: 2} as $rank
      | (.changes // .findings // []) | sort_by($rank[.severity])[]
      | "\(if .allowed then "allowed " else "" end)\(.severity): \(.kind // .check) \(.subject | clean): \(.message | clean)"' \
      "$report" 2>/dev/null || true
  fi
  echo "::$token::"

  {
    echo "exit-code=$code"
    echo "report=$report"
    if command -v jq >/dev/null; then
      echo "counts=$(jq -c '.counts // {}' "$report" 2>/dev/null || echo '{}')"
    fi
    if [[ -f $samples ]]; then
      echo "samples=$samples"
    fi
  } >>"$GITHUB_OUTPUT"

  case $code in
    0) ;;
    1)
      if [[ $command == source ]]; then
        echo "::error title=Program id drift::At least one finding is at or above the fail-on tier: the workspace does not give one program id. The job summary lists each finding."
      elif [[ $tool == idl-drift ]]; then
        echo "::error file=$(escape_property "$INPUT_IDL"),title=IDL drift::At least one change is at or above the fail-on tier. The job summary lists each change."
      else
        echo "::error file=$(escape_property "$INPUT_IDL"),title=IDL drift::At least one finding is at or above the fail-on severity: the program or your parser does not match the IDL. The job summary lists each finding."
      fi
      exit 1
      ;;
    *)
      echo "::error title=$tool could not run::Exit code $code, so nothing was checked. The cause is in the log above. This is not a finding about the IDL."
      exit "$code"
      ;;
  esac
}

# The text of one Markdown table cell.
cell() {
  local s=${1//|/\\|}
  printf '%s' "${s//$'\n'/ }"
}

# watch: check the programs one time. For each on-chain IDL that changed, open a pull request
# that updates the local IDL when each change is additive or cosmetic, else an issue.
# run calls it, so it reads the locals of run: bin, out, summary, extra and gh_token.
watch_run() {
  local events="$out/events.jsonl" saved="$out/onchain" run_state="" code=0 failed=0 event cmd token
  local annotations="$out/annotations.txt"
  if [[ -n ${INPUT_IDL:-}${INPUT_OLD:-}${INPUT_PROGRAM:-}${INPUT_ALLOW:-}${INPUT_FAIL_ON:-} ]]; then
    fail "idl-drift could not run" "The watch command reads the programs input, and takes no idl, old, program, allow or fail-on input."
  fi
  if [[ ! -f ${INPUT_PROGRAMS:-} ]]; then
    fail "idl-drift could not run" "The programs input \`${INPUT_PROGRAMS:-}\` is not a file. Give the path of the programs file."
  fi
  if ! in_workspace "$INPUT_PROGRAMS"; then
    fail "idl-drift could not run" "The input \`$INPUT_PROGRAMS\` resolves to a path outside the workspace."
  fi
  if [[ -n ${INPUT_STATE:-} ]] && ! in_workspace "$(dirname -- "$INPUT_STATE")"; then
    fail "idl-drift could not run" "The input \`$INPUT_STATE\` resolves to a path outside the workspace."
  fi
  if [[ -z ${INPUT_RPC_URL:-} ]]; then
    fail "idl-drift could not run" "Set the rpc-url input to the secret that holds your RPC URL."
  fi
  if ! command -v jq >/dev/null; then
    fail "idl-drift could not run" "The watch command needs jq on the runner."
  fi
  export IDL_DRIFT_RPC_URL=$INPUT_RPC_URL
  cmd=(watch --programs "$INPUT_PROGRAMS" --once --json --save-onchain "$saved")
  # The run writes a copy of the state. An entry goes to the state input only after its pull
  # request or issue, so that the next run tries a failed one again.
  if [[ -n ${INPUT_STATE:-} ]]; then
    run_state="$out/state.json"
    if [[ -f $INPUT_STATE ]]; then
      cp -- "$INPUT_STATE" "$run_state"
    fi
    cmd+=(--state "$run_state")
  fi
  "$bin/idl-drift" "${cmd[@]}" ${extra[@]+"${extra[@]}"} >"$events" 2>"$out/stderr.txt" || code=$?

  printf '### idl-drift watch\n\n| Program | IDL | Deploy slot | On-chain IDL | Changes | Result |\n|---|---|---:|---|---|---|\n' >"$summary"
  : >"$annotations"
  # IDL text in the log must not run as a workflow command.
  token=$(od -An -N16 -tx1 /dev/urandom | tr -d ' \n')
  echo "::stop-commands::$token"
  cat "$out/stderr.txt"
  while IFS= read -r event; do
    watch_event "$event" || failed=1
  done < <(jq -c 'select(type == "object" and .event == "change")' "$events" 2>/dev/null)
  jq -r 'select(type == "object" and .event == "error")
    | "| `\(.program)` | \(.idl | gsub("\\|"; "\\|")) | | | | error: \(.error | gsub("[\n|]"; " ")) |"' \
    "$events" >>"$summary" 2>/dev/null || true
  echo "::$token::"
  # The runner shows no annotation inside the span, so they come after it.
  cat "$annotations"
  cat "$summary" >>"$GITHUB_STEP_SUMMARY"

  if [[ $failed == 1 ]]; then
    code=2
  fi
  {
    echo "exit-code=$code"
    echo "report=$events"
  } >>"$GITHUB_OUTPUT"
  if [[ $code != 0 ]]; then
    echo "::error title=idl-drift watch::Exit code $code: a program could not be checked, or a pull request or issue could not be opened. The cause is in the log above."
    exit 2
  fi
}

# watch_event <event JSON>: act on one change event, and add its row to the summary.
watch_event() {
  local event=$1 program idl path status highest saved source slot deploy counts key="" result=""
  program=$(jq -r .program <<<"$event")
  idl=$(jq -r .idl <<<"$event")
  path=$(jq -r .idl_path <<<"$event")
  status=$(jq -r .status <<<"$event")
  deploy=$(jq -r .deploy_slot <<<"$event")
  highest=$(jq -r '.diff.highest // ""' <<<"$event")
  saved=$(jq -r '.onchain.saved // ""' <<<"$event")
  source=$(jq -r '.onchain.source // ""' <<<"$event")
  slot=$(jq -r '.onchain.source as $s | [.idls[] | select(.source == $s) | .slot][0] // ""' <<<"$event")
  counts=$(jq -r '.diff.counts // empty | "\(.breaking) breaking, \(.dangerous) dangerous, \(.additive) additive, \(.cosmetic) cosmetic"' <<<"$event")
  # Any transaction that names the IDL account moves its slot, so the text names the IDL.
  if [[ -f $saved ]]; then
    key=$(sha256 "$saved" | cut -c1-12)
  fi
  # The program, the slots and the key go into a branch name and a title.
  if [[ ! $program =~ ^[1-9A-HJ-NP-Za-km-z]{32,44}$ || ! $deploy =~ ^[0-9]+$ || ! $slot =~ ^[0-9]*$ ||
    ! $key =~ ^[0-9a-f]*$ ]]; then
    echo "::error title=idl-drift watch::An event has an invalid program, slot or IDL." >>"$annotations"
    return 1
  fi
  if [[ $status == stale ]]; then
    echo "::warning title=Stale on-chain IDL::Program $program was deployed in slot $deploy, after the last write to its on-chain IDL. The IDL can be out of date." >>"$annotations"
  fi
  case $highest in
    "") result="no change" ;;
    additive | cosmetic) watch_pr || result=${result:-"error: the pull request could not be opened"} ;;
    *) watch_issue || result=${result:-"error: the issue could not be opened"} ;;
  esac
  if [[ $result != error:* ]] && ! keep_state; then
    result="error: the state input could not be written"
  fi
  echo "$program $idl: $result"
  printf '| `%s` | %s | %s | %s | %s | %s |\n' "$program" "$(cell "$idl")" "$deploy" \
    "$(cell "${source:-none}${slot:+, slot $slot}")" "$(cell "${counts:-none}")" "$(cell "$result")" >>"$summary"
  [[ $result != error:* ]]
}

# watch_body <file> <intro>: the body of a pull request or an issue, with the Markdown diff.
watch_body() {
  local body=$1 limit=60000
  {
    printf '%s\n\n' "$2"
    "$bin/idl-drift" diff --markdown "$path" "$saved" 2>/dev/null || true
    printf '\nOpened by the idl-drift watch action.\n'
  } >"$body.full"
  # GitHub takes at most 65536 characters in a body.
  if [[ $(wc -c <"$body.full") -gt $limit ]]; then
    head -c "$limit" "$body.full" >"$body"
    printf '\n\n(The diff is cut here. Run `idl-drift diff` on the two files to see all of it.)\n' >>"$body"
  else
    mv "$body.full" "$body"
  fi
}

# keep_state: copy the entry of this event from the state of the run to the state input.
keep_state() {
  local old='{"version":1,"programs":[]}'
  [[ -n $run_state && -f $run_state ]] || return 0
  if [[ -f $INPUT_STATE ]]; then
    old=$(cat -- "$INPUT_STATE")
  fi
  mkdir -p -- "$(dirname -- "$INPUT_STATE")" &&
    jq --slurpfile run "$run_state" --arg p "$program" --arg i "$idl" \
      '.programs = ([.programs[] | select(.program != $p or .idl != $i)]
        + [$run[0].programs[] | select(.program == $p and .idl == $i)])' \
      <<<"$old" >"$INPUT_STATE.tmp" &&
    mv -- "$INPUT_STATE.tmp" "$INPUT_STATE"
}

# Sets `result` and returns 1 when gh cannot run.
needs_gh() {
  if [[ -z $gh_token ]]; then
    result="error: give the token input, which is github.token by default"
    return 1
  fi
  if [[ -z ${GITHUB_REPOSITORY:-} || -z ${GITHUB_REF_NAME:-} || ! -f $saved || -z $key ]]; then
    result="error: GITHUB_REPOSITORY, GITHUB_REF_NAME or the on-chain IDL is missing"
    return 1
  fi
}

# A pull request that writes the on-chain IDL to the local path. git plumbing makes the
# commit, so the checkout and its index stay as they are for the next program.
watch_pr() {
  local dir top rel branch title new blob mode index tree commit body n
  needs_gh || return 1
  if ! in_workspace "$path"; then
    result="error: the local IDL resolves to a path outside the workspace"
    return 1
  fi
  dir=$(dirname -- "$path")
  top=$(git -C "$dir" rev-parse --show-toplevel) || return 1
  rel="$(git -C "$dir" rev-parse --show-prefix)$(basename -- "$path")"
  branch="idl-drift/$program-$key"
  title="Update the IDL of $program to its on-chain IDL $key"
  # A closed pull request is an answer too, so the bot does not open it again.
  n=$(GH_TOKEN=$gh_token gh pr list --repo "$GITHUB_REPOSITORY" --head "$branch" --state all --json number --jq length) || return 1
  if [[ $n != 0 ]]; then
    result="a pull request from $branch exists"
    return 0
  fi
  # jq keeps the key order of the on-chain text; a file that is not JSON stops here.
  new="$out/$program-$key.json"
  jq . "$saved" >"$new" || return 1
  blob=$(git -C "$top" hash-object -w -- "$new") || return 1
  mode=$(git -C "$top" ls-files -s -- "$rel" | cut -d' ' -f1)
  index="$out/index-$program-$key"
  GIT_INDEX_FILE=$index git -C "$top" read-tree HEAD || return 1
  GIT_INDEX_FILE=$index git -C "$top" update-index --add --cacheinfo "${mode:-100644},$blob,$rel" || return 1
  tree=$(GIT_INDEX_FILE=$index git -C "$top" write-tree) || return 1
  commit=$(GIT_AUTHOR_NAME="github-actions[bot]" GIT_COMMITTER_NAME="github-actions[bot]" \
    GIT_AUTHOR_EMAIL="41898282+github-actions[bot]@users.noreply.github.com" \
    GIT_COMMITTER_EMAIL="41898282+github-actions[bot]@users.noreply.github.com" \
    git -C "$top" commit-tree "$tree" -p HEAD -m "$title") || return 1
  # No pull request is from this branch, so a branch that is there is from a run whose
  # `gh pr create` failed. Only branches under idl-drift/ are replaced.
  git -C "$top" push -q --force origin "$commit:refs/heads/$branch" || return 1
  body="$out/body-$program-$key.md"
  watch_body "$body" "The $source IDL account of \`$program\` was written in slot $slot (deploy slot $deploy). Against \`$idl\`, each change is additive or cosmetic, so this pull request writes the on-chain IDL to that file."
  GH_TOKEN=$gh_token gh pr create --repo "$GITHUB_REPOSITORY" --base "$GITHUB_REF_NAME" \
    --head "$branch" --title "$title" --body-file "$body" || return 1
  result="pull request opened from $branch"
}

# An issue with the Markdown diff, one time for each program and IDL text.
watch_issue() {
  local title body n
  needs_gh || return 1
  title="idl-drift: the on-chain IDL of $program has $highest changes (IDL $key)"
  n=$(GH_TOKEN=$gh_token gh issue list --repo "$GITHUB_REPOSITORY" --state all \
    --search "$program $key in:title" --json title --limit 100 |
    jq --arg t "$title" '[.[] | select(.title == $t)] | length') || return 1
  if [[ $n != 0 ]]; then
    result="the issue exists"
    return 0
  fi
  body="$out/body-$program-$key.md"
  watch_body "$body" "The $source IDL account of \`$program\` was written in slot $slot (deploy slot $deploy). Against \`$idl\`, at least one change is $highest: a parser built from that file can misread data. Update the parser, then the file."
  GH_TOKEN=$gh_token gh issue create --repo "$GITHUB_REPOSITORY" --title "$title" \
    --body-file "$body" || return 1
  result="issue opened"
}

case ${1:-} in
  install) install ;;
  run) run ;;
  *)
    echo "usage: action.sh install|run" >&2
    exit 2
    ;;
esac
