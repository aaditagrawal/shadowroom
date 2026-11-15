#!/usr/bin/env bash
# One native-library environment for local checks, builds, tests, and releases.
set -euo pipefail

usage() {
    cat <<'HELP'
Usage: scripts/dev.sh [--runner cargo|mbx|blaze] [--profile NAME]
                      [--linker system|wild|mold] TASK [extra arguments...]

Tasks:
  check          Type-check the desktop with dev-fast.
  build, run     Build or run the desktop with dev-fast.
  test-core      Core/integration tests with dev-test, native color/codecs, no GPUI.
  test-desktop   Desktop unit tests with dev-fast.
  release        Build both production binaries.
  setup-linker   Install a pinned --linker wild or mold for build/run (Linux x86_64).

Examples:
  scripts/dev.sh check --offline
  scripts/dev.sh run -- /path/to/photos
  scripts/dev.sh test-core grouping -- --nocapture
  scripts/dev.sh --runner mbx build
  scripts/dev.sh --linker wild setup-linker
  scripts/dev.sh --linker wild run -- /path/to/photos

Runner defaults to Cargo. An explicitly selected runner must succeed itself.
Build/check/run and desktop tests default to dev-fast; core tests use dev-test.
Release always uses the release profile. Native LCMS builds are consistently static.
Alternative linkers apply to build/run only and preserve dependency compiler flags.
HELP
}

runner=cargo
profile=
linker=system
while [[ $# -gt 0 ]]; do
    case "$1" in
        --runner)
            runner=${2:-}
            case "$runner" in
                cargo|mbx|blaze) shift 2 ;;
                *) printf '%s\n' '--runner requires cargo, mbx, or blaze.' >&2; exit 2 ;;
            esac
            ;;
        --profile)
            profile=${2:-}
            if [[ -z "$profile" || "$profile" == -* ]]; then
                printf '%s\n' '--profile requires a Cargo profile name.' >&2
                exit 2
            fi
            shift 2
            ;;
        --linker)
            linker=${2:-}
            case "$linker" in
                system|wild|mold) shift 2 ;;
                *) printf '%s\n' '--linker requires system, wild, or mold.' >&2; exit 2 ;;
            esac
            ;;
        *) break ;;
    esac
done
task=${1:---help}
if [[ "$task" == --help || "$task" == -h ]]; then
    usage
    exit 0
fi
shift
if [[ -z "$profile" ]]; then
    case "$task" in
        test-core) profile=dev-test ;;
        *) profile=dev-fast ;;
    esac
fi

export LCMS2_STATIC=${LCMS2_STATIC:-1}
script_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
if [[ "$linker" != system ]]; then
    case "$task" in
        setup-linker) link_task=setup ;;
        build|run) link_task=$task ;;
        *) printf '%s\n' '--linker applies to build, run, or setup-linker.' >&2; exit 2 ;;
    esac
    if [[ "$runner" == blaze ]]; then
        printf '%s\n' '--linker requires the cargo or mbx runner.' >&2
        exit 2
    fi
    if command -v uv >/dev/null 2>&1; then
        uv_bin=$(command -v uv)
    elif [[ -x "$HOME/.local/bin/uv" ]]; then
        uv_bin="$HOME/.local/bin/uv"
    else
        printf '%s\n' 'uv is required for alternative linker builds.' >&2
        exit 1
    fi
    exec "$uv_bin" run --no-project python "$script_root/dev-link.py" \
        --runner "$runner" --profile "$profile" --linker "$linker" "$link_task" "$@"
fi
case "$task" in
    check|build|run) arguments=("$task" --locked --profile "$profile" --bin shadowroom) ;;
    test-core) arguments=(test --locked --profile "$profile" --no-default-features --features native-color,native-codecs) ;;
    test-desktop) arguments=(test --locked --profile "$profile" --bin shadowroom) ;;
    release) arguments=(build --locked --release --bins) ;;
    setup-linker) printf '%s\n' 'Select --linker wild or mold for setup-linker.' >&2; exit 2 ;;
    *) printf 'Unknown task: %s\n' "$task" >&2; usage >&2; exit 2 ;;
esac

exec "$script_root/check-linux-local.sh" --runner "$runner" "${arguments[@]}" "$@"
