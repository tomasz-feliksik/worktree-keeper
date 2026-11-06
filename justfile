# Recipes assume the dev shell is active: nix develop --command just <recipe>

# ==============================================================================
# Formatting
# ==============================================================================

fmt:
    nix fmt

fmt-check:
    nix fmt -- --ci

# ==============================================================================
# Lint
# ==============================================================================

clippy:
    cargo clippy --all-targets -- --deny warnings

audit:
    cargo audit

deny:
    cargo deny check

markdownlint:
    markdownlint-cli2 "**/*.md" "#.beads" "#.agents" "#target" "#.direnv" "#mutants.out*" "#AGENTS.md" "#CLAUDE.md"

lint: fmt-check clippy audit deny markdownlint

# ==============================================================================
# Test
# ==============================================================================

test:
    cargo test

# The black-box suite: the guarantees, against the real binary and real git.
parity *tests:
    cargo build
    WORKTREE="$PWD/target/debug/worktree" bash tests/parity/test.sh {{tests}}

# Judged by the parity suite too, through tests/parity.rs. Read `unviable` as "not
# tested", never as a pass. The excluded functions are macOS-only: Linux never compiles
# them, so their mutants could only ever read as missed. Use --in-diff for a quick pass.
mutants *args:
    cargo mutants --jobs 8 --timeout 60 --exclude-re 'scan_lsof|own_chain_by_ps' {{args}} -- -- --include-ignored

# A release tag has to name the version the binary reports.
check-tag tag:
    #!/usr/bin/env bash
    set -euo pipefail
    tag={{quote(tag)}}
    reported=$(cargo run --quiet -- --version)
    [[ "$tag" == "v${reported#worktree }" ]] || {
      echo "tag $tag does not match the binary, which reports: $reported" >&2
      exit 1
    }

check: lint test parity
