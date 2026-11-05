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
    markdownlint-cli2 "**/*.md" "#.beads" "#.agents" "#target" "#AGENTS.md" "#CLAUDE.md"

lint: fmt-check clippy audit deny markdownlint

# ==============================================================================
# Test
# ==============================================================================

test:
    cargo test

# The black-box suite. Not part of `check` until the port reaches parity.
parity *tests:
    cargo build
    WORKTREE="$PWD/target/debug/worktree" bash tests/parity/test.sh {{tests}}

mutants:
    cargo mutants

check: lint test
