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
    diff) tool=idl-drift ;;
    verify) tool=idl-verify ;;
    *) fail "idl-drift action" "The command input must be diff or verify, not \`$INPUT_COMMAND\`." ;;
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

run() {
  local tool bin out report summary cmd extra old live=false samples="" code=0
  set_tool
  bin=${IDL_DRIFT_BIN:?the install step sets IDL_DRIFT_BIN}
  : "${GITHUB_OUTPUT:?}" "${GITHUB_STEP_SUMMARY:?}"
  if [[ ! -f ${INPUT_IDL:-} ]]; then
    fail "$tool could not run" "The idl input \`${INPUT_IDL:-}\` is not a file. Give the path of the new IDL."
  fi
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

  if [[ $tool == idl-drift ]]; then
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
    jq -r '{breaking: 0, dangerous: 1, additive: 2, cosmetic: 3, error: 0, warning: 1, note: 2} as $rank
      | (.changes // .findings // []) | sort_by($rank[.severity])[]
      | "\(if .allowed then "allowed " else "" end)\(.severity): \(.kind // .check) \(.subject): \(.message)"' \
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
      if [[ $tool == idl-drift ]]; then
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

case ${1:-} in
  install) install ;;
  run) run ;;
  *)
    echo "usage: action.sh install|run" >&2
    exit 2
    ;;
esac
