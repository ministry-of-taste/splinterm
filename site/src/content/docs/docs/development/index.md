---
title: Development
description: Build, test, and contribute to the Splinterm Rust workspace using isolated dev daemons and branch-first worktrees.
---

Splinterm is a Cargo workspace split cleanly across the graphical client, daemon, domain model, private protocol, SSH relay, MCP adapter, PTY layer, and Foot-derived terminal kernel.

## Branch-first workflow

Keep your root `main` checkout clean for fetches, review, and releases. Do every feature, fix, or documentation milestone on a short-lived branch in its own worktree:

```bash
git fetch origin
git worktree add --no-track -b feat/my-change \
  ../splinterm-worktrees/my-change origin/main
```

Keep one writer per worktree, run focused checks and `git diff --check`, push with `git push --set-upstream origin HEAD`, and open a PR for squash merge. Clean up the merged branch and worktree afterward. See `CONTRIBUTING.md` and `AGENTS.md` in the repository for the full workflow and agent guardrails.

## Standard Rust validation

Run from the worktree root:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Changes to domain or protocol behavior should include focused tests and respect crate boundaries.

## Testing against an isolated daemon

Use `./splinterm-test` to build and run against a private owner-only socket without touching your live desktop sessions:

```bash
./splinterm-test          # build, start or reuse the isolated daemon, open a window
./splinterm-test restart  # rebuild and restart after daemon or protocol changes
./splinterm-test ping     # check the isolated daemon
./splinterm-test stop     # stop it
```

The wrapper uses an intentionally isolated development auth bypass—never point it at your packaged `splinterd` or normal user state.

## Workspace layout

| Crate | Responsibility |
| --- | --- |
| `splinterm` | CLI, native Wayland frontend, input, and CPU/SHM renderer |
| `splinterd` | Authoritative topology, PTYs, persistence, consent, and policy |
| `splinterm-core` | Transport- and UI-independent domain model |
| `splinterm-protocol` | Versioned private client–daemon wire protocol |
| `splinterm-relay` | Policy-identified SSH stdio relay |
| `splinterm-mcp` | Optional policy-identified MCP stdio adapter |
| `splinterm-pty` | Linux PTY and child-process management |
| `splinterm-terminal` | Foot-derived terminal grid and parser kernel |

## Where docs live

- **User guides:** This site (`site/src/content/docs/`).
- **Public machine contracts:** Repository `docs/automation.md`, `docs/mcp.md`, and checked-in schemas under `dist/schemas/v2/`.
- **Architecture decisions:** `docs/adr/`.
- **Internal plans, spikes, and benchmarks:** Development history and evidence—not user documentation.

## Foot lineage and differential checks

Splinterm's own test suite and fixtures are release authority. The pinned Foot 1.27.0 checkout remains an optional historical differential reference—never edit the canonical Foot checkout or silently regenerate reference images. Any ported code must preserve MIT license compatibility, exact provenance, and attribution in `THIRD_PARTY.md`. See [ADR 0013](https://github.com/ministry-of-taste/splinterm/blob/main/docs/adr/0013-splinterm-owned-renderer-acceptance.md).
