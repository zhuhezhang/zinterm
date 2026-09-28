#!/usr/bin/env bash
# macOS / Linux bash script to release a new version of zinterm
#
# When you run something like ./scripts/release.sh v1.2.3 --push --remote zinterm_github, the script:
# - Ensures a clean git tree and that the tag doesn’t already exist
# - Updates the version in Cargo.toml / Cargo.lock to 1.2.3
# - Verifies with cargo check and cargo run -- --version
# - Creates a commit and annotated tag v1.2.3
# - If --push is set, pushes the branch and tag to --remote
#
# Usage: ./scripts/release.sh v1.2.3 --remote zinterm_github            # annotated tag
# Usage: ./scripts/release.sh v1.2.3 --push --remote zinterm_github     # annotated tag and push
# Usage: ./scripts/release.sh v1.2.3 --remote zinterm_github --dry-run  # print actions only, no real modifications

set -euo pipefail  # Halt on error / unset vars / pipe failures; prevent partial releases.

TAG=""      # Required; must look like v1.2.3 or v1.2.3-rc.1
REMOTE=""   # Required; git remote name to push to (e.g. origin, zinterm_github)
PUSH=0      # Optional; if set --push, pushes the branch and tag
DRY_RUN=0   # Optional; if set --dry-run, print actions only, no real modifications

usage() {
    echo "Usage: $0 <vX.Y.Z[-pre]> --remote <name> [--push] [--dry-run]" >&2
    exit 1
}

# Parse CLI flags (supports both bash-style --push and PowerShell-style -Push).
while [[ $# -gt 0 ]]; do
    case "$1" in
        --push|-Push)
            PUSH=1
            shift
            ;;
        --dry-run|-DryRun)
            DRY_RUN=1
            shift
            ;;
        --remote|-Remote)
            if [[ $# -lt 2 || -z "${2:-}" || "$2" == -* ]]; then
                echo "--remote requires a remote name." >&2
                usage
            fi
            REMOTE="$2"
            shift 2
            ;;
        --remote=*|-Remote=*)
            REMOTE="${1#*=}"  # Accept --remote=name form as well.
            if [[ -z "$REMOTE" ]]; then
                echo "--remote requires a remote name." >&2
                usage
            fi
            shift
            ;;
        -h|--help)
            usage
            ;;
        -*)
            echo "Unknown option: $1" >&2
            usage
            ;;
        *)
            if [[ -n "$TAG" ]]; then
                echo "Unexpected argument: $1" >&2
                usage
            fi
            TAG="$1"  # Positional tag argument (exactly one allowed).
            shift
            ;;
    esac
done

if [[ -z "$TAG" ]]; then
    echo "Tag is required (e.g. v1.2.3)." >&2
    usage
fi

if [[ -z "$REMOTE" ]]; then
    echo "--remote <name> is required (e.g. origin, github)." >&2
    usage
fi

# Same ValidatePattern as release.ps1: vX.Y.Z or vX.Y.Z-pre.
if [[ ! "$TAG" =~ ^v[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$ ]]; then
    echo "Tag must look like v1.2.3 or v1.2.3-rc.1, got: $TAG" >&2
    exit 1
fi

# Shell-quote a command line for DryRun printing.
quote_cmd() {
    local out="$1"
    shift
    local arg
    for arg in "$@"; do
        out+=" $(printf '%q' "$arg")"
    done
    printf '%s\n' "$out"
}

# Helper to run git with DryRun support.
run_git() {
    if [[ "$DRY_RUN" -eq 1 ]]; then
        quote_cmd git "$@"  # Print the command and return without executing it.
        return 0
    fi
    git "$@"
}

# Helper to run cargo with DryRun support.
run_cargo() {
    if [[ "$DRY_RUN" -eq 1 ]]; then
        quote_cmd cargo "$@"
        return 0
    fi
    cargo "$@"
}

# Run a command and require its stdout to match $expected exactly.
run_checked_output() {
    local expected="$1"  # Expected output string
    shift                # Remaining args become the command to run.

    if [[ "$DRY_RUN" -eq 1 ]]; then
        quote_cmd "$@"  # DryRun: print only.
        return 0
    fi

    local output
    output="$("$@")"
    # Normalize CRLF / surrounding whitespace before comparing.
    output="$(printf '%s' "$output" | tr -d '\r' | sed -e 's/^[[:space:]]*//' -e 's/[[:space:]]*$//')"
    if [[ "$output" != "$expected" ]]; then  # Fail if output does not match (validate --version).
        echo "Expected '$expected' but got '$output'." >&2
        exit 1
    fi
}

repo_root="$(git rev-parse --show-toplevel 2>/dev/null || true)"  # Find repo root path.
if [[ -z "$repo_root" ]]; then
    echo "This script must be run inside a git repository." >&2
    exit 1
fi

cd "$repo_root"  # cd to repo root.

# Fail if there are unstaged changes to tracked files.
if ! git diff --quiet --exit-code; then
    echo "Tracked files have unstaged changes. Commit or stash them before releasing." >&2
    exit 1
fi

# Fail if there are staged changes.
if ! git diff --cached --quiet --exit-code; then
    echo "Tracked files have staged changes. Commit or stash them before releasing." >&2
    exit 1
fi

# List matching tags; error if the tag already exists.
if [[ -n "$(git tag --list "$TAG")" ]]; then
    echo "Tag '$TAG' already exists." >&2
    exit 1
fi

# Fail early if the named remote is missing.
if ! git remote get-url "$REMOTE" >/dev/null 2>&1; then
    echo "Git remote '$REMOTE' does not exist." >&2
    exit 1
fi

version="${TAG#v}"  # Extract version from tag, e.g. v1.2.3 → 1.2.3
cargo_toml_path="$repo_root/Cargo.toml"
cargo_lock_path="$repo_root/Cargo.lock"

if [[ "$DRY_RUN" -eq 1 ]]; then  # DryRun: print only.
    echo "Would set Cargo.toml and Cargo.lock version to $version."
else
    # perl is available on macOS/Linux and matches the PowerShell (?ms) replacements.
    # Update [package].version in Cargo.toml exactly once.
    ZINTERM_VERSION="$version" perl -i -0pe '
        my $v = $ENV{ZINTERM_VERSION};
        my $n = 0;
        $n += s/^(\[package\]\s+.*?^version\s*=\s*")[^"]+(")/$1$v$2/ms;
        die "Could not update [package].version in Cargo.toml.\n" unless $n == 1;
    ' -- "$cargo_toml_path"

    # Update the zinterm package version entry in Cargo.lock exactly once.
    ZINTERM_VERSION="$version" perl -i -0pe '
        my $v = $ENV{ZINTERM_VERSION};
        my $n = 0;
        $n += s/^(name\s*=\s*"zinterm"\s*)(\r?\n)(version\s*=\s*")[^"]+(")/$1$2$3$v$4/ms;
        die "Could not update zinterm version in Cargo.lock.\n" unless $n == 1;
    ' -- "$cargo_lock_path"
fi

run_cargo check --locked  # Typecheck/build-check; refuse lockfile drift
run_checked_output "zinterm $version" cargo run --locked -- --version  # Require exact zinterm <version> output

run_git add Cargo.toml Cargo.lock                 # Stage the modified manifests for commit
run_git commit -m "Release $TAG"                  # Commit the release with a message
run_git tag -a "$TAG" -m "Release $TAG"           # Annotate the commit with a tag

if [[ "$PUSH" -eq 1 ]]; then  # With --push: push current branch and tag to $REMOTE.
    run_git push "$REMOTE" HEAD
    run_git push "$REMOTE" "$TAG"
    echo "Released $TAG and pushed branch + tag to '$REMOTE'."
else  # Without --push: local only; print the manual push commands.
    echo "Created release commit and tag $TAG."
    echo "Push with: git push $REMOTE HEAD && git push $REMOTE $TAG"
fi
