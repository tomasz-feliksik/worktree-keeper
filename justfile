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

# The same suite against the frozen Bash tool, to see what the original does.
# After the foundation task, t_help_never_acts fails here by design.
parity-bash *tests:
    mkdir -p target
    { printf '#!/usr/bin/env bash\nset -euo pipefail\n'; cat reference/bash/worktree.sh; } > target/worktree-bash
    chmod +x target/worktree-bash
    WORKTREE="$PWD/target/worktree-bash" bash tests/parity/test.sh {{tests}}

mutants:
    cargo mutants

check: lint test
