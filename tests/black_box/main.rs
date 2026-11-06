//! The black-box suite: every guarantee, through the real binary, against real git
//! repositories whose remotes are local bare clones. Offline, and every path stays inside
//! each test's own scratch directory, because `just mutants` runs broken builds of a tool
//! that deletes things as your own user.
//!
//! Each test states the guarantee it holds; docs/internals.md lists the mutation that
//! must turn each one red. `WORKTREE_UNDER_TEST` points the suite at another build, which
//! is how `nix flake check` runs it against the wrapped package.

// A test here is one story through several states of a repository, told in order. Cutting
// one up would only move its setup further from what it proves.
#![allow(clippy::too_many_lines)]

mod support;

mod guard;
mod help;
mod provision;
mod questions;
mod seed;
mod sweep;
