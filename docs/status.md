# Current product status

- **Current public version tag:** `v0.2.0`

This document is the repository authority for Splinterm's current maturity,
validated product scope, availability, and release gates. Retained public
evidence explains how a capability was accepted; the [product roadmap](product-roadmap.md)
owns strategic direction, maintainer-controlled coordination owns delivery
sequence, and this page owns what the product is today.

## Maturity

**Splinterm 0.2.0 is the current stable release.**
0.1.1 and 0.1.0 remain historical stable releases; 0.1.0 was the first.

Source, documentation, and immutable versioned GitHub packages are publicly
available. AUR distribution of 0.2.0 is pending; both AUR bases still offer
0.1.1-1. Core terminal emulation, daemon-owned persistence, multiplexing,
native Wayland presentation, Arch packaging, and bounded automation workflows
are implemented and validated in the scopes named below. Public availability is
not a promise of broader platform compatibility or a support lifetime. The
validated target remains narrow, and future 0.x releases may change interfaces
with documented migration.

Splinterm is **security-conscious**, not absolutely secure. Automation is
constrained by exact executable identity, explicit scopes, resource and message
bounds, exclusive controller ownership, consent, revocation, and body-free audit
metadata. Terminal output remains untrusted data and cannot grant authority or
become an automatic instruction.

## Validated environment

The current product target is:

- x86_64 Omarchy/Arch Linux;
- native Wayland under the documented Hyprland environment;
- Splinterm-owned semantic, renderer, contract, package, and guarded graphical
  tests as release authority; Foot 1.27.0 commit
  `3c5b584b0eafa772eb4376fb6eaf6643399e190e` remains an optional historical
  differential under [ADR 0013](adr/0013-splinterm-owned-renderer-acceptance.md);
- stable `v0.2.0` release assets built from clean committed source; and
- guarded installed-package evidence for the Alpha3 command, scrollback,
  saved-Lair, Wayland file-drop, and Omarchy screensaver workflows; isolated
  exact-package acceptance for the Alpha3.1 transient-tab hotfixes; accepted
  Beta 1 graphical, package, provenance, wide-grid, sparse-publication, and
  active-tab contrast evidence; and accepted Beta 2 terminal-lifetime,
  font-family, workload-isolation, package, and release evidence.

Other Linux distributions, compositors, architectures, and package formats are
not current compatibility promises. Headless `splinterd` does not require a
graphical environment; its packaged and remote workflows are scoped to the
documented platform.

## Live font synchronization

Stable 0.1.0 includes live native Omarchy system-font synchronization. When
`main.font` is unset, a graphical client follows the effective fontconfig `monospace` family, stages complete
immutable face generations off dispatch paths, and atomically rebuilds active
and hidden presentation state while preserving configured size/policy, padding,
DPI, runtime zoom, topology, focus, history, modal input, and IME preedit.
Explicit `main.font` disables native following. Invalid live candidates retain
the last valid generation, and observer panes do not acquire control to resize.
Fontconfig named instances remain selected through shaping and rasterization;
ambient probes and deferred resize retries are bounded. Accepted staging stays
synchronized with probe state, and cached raster faces share immutable font
mappings across sizes instead of copying complete files.

Focused renderer, watcher, cache-generation, source-replacement, geometry, and
resize-authority tests pass. The exact RC2 maintenance package also passed
guarded live valid-font replacement and invalid-candidate rollback acceptance
without replacing the Window, daemon, shell, or Splint incarnation. This behavior
shipped in stable 0.1.0 after the release-candidate cycle.

## Stable 0.2.0 publication

`v0.2.0` was published on 2026-11-03 at 04:43 UTC (2026-11-02 in the
maintainer's local time) from
`1f129ff88c1cfdcd498ec44eed38b3328d9c94a1` on `main`, as **Splinterm 0.2.0**.
It is GitHub Latest, not a prerelease.

Current stable release notes and assets:
https://github.com/ministry-of-taste/splinterm/releases/tag/v0.2.0

The release adds live daemon upgrade from 0.2.0 on, easy agent access under
[ADR 0014](adr/0014-easy-agent-access.md), explicitly reported agent status,
the startup Dojo picker, remote launch from inside the app, and Omarchy 4 theme
integration. The one-time upgrade from 0.1.x to 0.2.0 restarts the daemon and
ends its shells. The private protocol changed: a 0.2.0 client and a 0.1.x daemon
cannot communicate, or vice versa; upgrade remote hosts and the optional MCP
adapter together. See [Packaging](packaging.md) for the upgrade procedure.

- Exact merged-commit CI run `41226461095` passed.
- Candidate run `41523853666` built the main and optional MCP packages once.
- Protected promotion run `41578629716` published those exact artifacts without
  rebuilding and verified all five assets: `candidate-manifest.json`,
  `SHA256SUMS`, `splinterm-0.2.0.tar.gz`,
  `splinterm-1f129ff88c1cfdcd498ec44eed38b3328d9c94a1-x86_64.pkg.tar.zst`, and
  `splinterm-mcp-1f129ff88c1cfdcd498ec44eed38b3328d9c94a1-x86_64.pkg.tar.zst`.
- Manifest SHA-256:
  `42a7012051ba5422b33c1ac0f9fbcb35d786ab99634a45b58c8daa4ab953e917`.
- Source archive SHA-256:
  `285d2eb815bbf90cd83bcdf79d8a1ed4b8106586a7cd34dca42d50b505a1428f`.
- Main package SHA-256:
  `61936ffb9df2917e25e69e65360c4813103da265dae4697d4043e78ff73c48ee`.
- MCP package SHA-256:
  `9841678a36fb518db2bd9d6dd683bb7f2ec46f2541bb614d6c52f7b748fb9dda`.
- AUR distribution of 0.2.0 has not happened. Both source and prebuilt package
  bases still publish `0.1.1-1`, including their matching optional MCP packages.

No new graphical sequence was run for finalization. Feature acceptance was
recorded in the Omarchy VM on earlier builds of the same features; this is not
a fresh installed-final-package matrix. Live upgrade remains capability-gated:
Nix installations refuse it in 0.2.0, and a daemon adopted through live upgrade
or rollback needs a restart before another upgrade. Stopping the daemon still
ends its child processes; saved topology does not checkpoint running programs.

## Historical stable 0.1.1 publication

`v0.1.1` was published on 2026-10-09 from
`136ef81d268d9734f5ffb687089859438ba99579` on `maint/0.1`.
It retains the product implementation from `v0.1.1-rc.3` unchanged;
finalization changed workspace/package version metadata and release notes,
not runtime source, configuration, dependencies, or fixtures.

Historical 0.1.1 release notes and assets:
https://github.com/ministry-of-taste/splinterm/releases/tag/v0.1.1

The release adds the RC series' Explorer navigation and context actions,
activity filters, opt-in ligatures, bounded local clipboard-image saving,
scoped AT-SPI navigation/status, remote hostname labels, and reliability fixes.
Accessibility does not expose terminal bodies or provide whole-terminal
screen-reader support. Saved-layout removal is not included. Nix package,
module and headless checks passed for the release; this does not extend Arch
installed graphical acceptance to NixOS or aarch64. The development source on
`main` is not the immutable maintenance release and may differ from it.

- Independent source review and local serialized workspace, strict Clippy,
  release/package, semantic and contract checks passed.
- Exact merged-commit CI run `37878336998` passed, including Nix checks.
- Candidate run `37879898683` built the main and optional MCP packages once;
  extracted package and MCP runtime validation passed.
- Protected promotion run `37881194238` published those exact artifacts as
  stable, without rebuilding. The immutable tag, all five downloaded assets,
  and GitHub Latest designation were reverified.
- Manifest SHA-256:
  `e1df8b23e950c93fcfd30a029c401cb3422abb9d57c68b84a00bc101bf0c294b`.
- AUR source package base commit:
  `6137f541b01d8410a318a3f5219d8058998176b2`.
- AUR prebuilt package base commit:
  `6dd91233abb044fcdbfd37dfc33fddb8a5eb8a17`.
  Fresh anonymous clones matched all three approved draft files in each base;
  public package pages showed `0.1.1-1`, including exact optional MCP pairing.

No new graphical sequence or package installation was run for finalization.
Recorded RC3 guest package/search/cwd acceptance used `55d7c7b9`; its product
source matches published RC3, with only testbed cleanup changes afterward.
This does not claim a fresh installed-final-package matrix or close every
Explorer follow-up. Live daemon upgrade is supported from 0.2.0 on; the
one-time 0.1.x to 0.2.0 upgrade restarts the daemon and ends its shells. Save
work and perform that upgrade from an independent terminal. See [Packaging](packaging.md) for exact-tag installation and recovery.

## Historical stable 0.1.0 and release candidates

Splinterm 0.1.0 was published on 2026-09-05 as the first stable release. It
carries the RC3 terminal and daemon implementation forward unchanged and
includes live Omarchy theme and default-font following, bounded automation,
and source-built and prebuilt Arch packages.

Historical 0.1.0 release notes and assets:

https://github.com/ministry-of-taste/splinterm/releases/tag/v0.1.0

The subsequent `v0.1.1-rc.2`, published on 2026-09-15, and `v0.1.1-rc.3`
are historical prereleases now superseded by stable `v0.1.1`. Release listing:

https://github.com/ministry-of-taste/splinterm/releases

**Live daemon upgrade is supported from 0.2.0 on.** The one-time 0.1.x to
0.2.0 upgrade restarts the daemon and ends its shells; saved topology does not
checkpoint running applications. Save your work and perform that upgrade from
a terminal not owned by `splinterd`, then reopen Splinterm Windows. See
[Packaging](packaging.md) for upgrade and recovery instructions.

## Historical releases

The beta and alpha records below describe earlier releases, not current maturity.

## Beta 2 release

[`v0.1.0-beta2`](https://github.com/ministry-of-taste/splinterm/releases/tag/v0.1.0-beta2)
was published as a GitHub prerelease on 2026-08-24 and distributed through both
AUR package bases as `0.1.0beta2-1`. Beta 2 adds configurable Window-owned
terminal lifetimes with atomic promotion to persistent Lairs, follows the
selected Fontconfig family for newly opened Windows, and places packaged
terminal workloads in verified nested systemd user units so the daemon retains
an independent resource-failure boundary.

- Release commit `f0c5dd176e36ce88abc1328c8e67839da263e56a` passed exact-SHA CI
  run `32684539475`. Candidate run `32685119499` built the release once, and
  protected promotion run `32690770541` published and reverified the exact
  five public assets; publication receipt artifact `9507412607` remains the
  release record.
- Source archive SHA-256:
  `ba8f953c2bf7562923af0c2e131b86a469ab2b4788c352ab3cfaec521125a1f2`.
  Main package SHA-256:
  `1e7060fea06867743a88a0021ad33385f32f5cc4a20a22afb45448130ebdded5`.
  MCP package SHA-256:
  `889b7a6457789ee601f095dfaf8e208172177fa93aba232fa20394e07ff3eacd`.
- AUR source package commit: `0dfb8e31fcb8dcefebff9b1735bcd9e8b0b33093`.
  AUR prebuilt package commit: `eae0d825a0331f5494eb61dfd9e01734374025f2`.
  Both visible recipes identify `0.1.0beta2-1` and pin the immutable GitHub
  source or package assets to the release hashes above.
- Complete serialized workspace, package, release-tooling, portable Foot
  provenance, documentation, independent review, disposable real-systemd
  placement, local package-integrity, and guarded staged-package graphical
  boundaries passed before publication. Beta 2 does not claim to fix the
  separately observed client-side Tokio panic.

Upgrading from Beta 1 restarts the `0.1` daemon and ends active Dojos. The
[packaging guide](packaging.md) documents the required external-terminal
upgrade and rollback workflow.

## Beta 1 release

[`v0.1.0-beta1`](https://github.com/ministry-of-taste/splinterm/releases/tag/v0.1.0-beta1)
was published as a GitHub prerelease on 2026-08-17 and distributed through both
AUR package bases as `0.1.0beta1-1`. Beta 1 accepts wide terminal grids through
`480×128`, bounds sparse terminal publication ownership and accounting, and
separates active-tab contrast from terminal selection colors while retaining the
accepted Alpha3 command, persistence, input, and Omarchy integration behavior.

- Release-state PR #29 merged as
  `40ac9d7bb803fc71495e36eb760174de7fcdfff0`; candidate-metadata correction
  PR #30 merged as `8d95e75704104750f8e8e4585e629010855963c8`.
- Candidate run `32004316406` built corrected commit `8d95e75` once. Candidate
  manifest SHA-256:
  `a09bef84812c2da3cfe729099d5bcdec2c4a809b234c8a6dc8d1c7f2ccbf018e`.
- Protected promotion run `32004973522` created the immutable tag, published and
  redownloaded the exact five public assets, verified every hash, and retained
  publication receipt artifact `9279942672`.
- Source archive SHA-256:
  `72bd626474f2f660cf5cf595f4e9dd040dafacd2f3087d58a81f01a32d39f5ef`.
  Main package SHA-256:
  `fb25323ca2edbb61243c942c84de4d1f4cb52280fbc7dbd4369243f603288eda`.
  MCP package SHA-256:
  `ededfa71a10b1bb3f199e78d56c4bfc32c5633f5188c3e89790980bf3803fecc`.
- AUR source package commit: `2052214554e25218f7329e299a3fc0f076d76756`.
  AUR prebuilt package commit: `d902f0bf49f7b454480724612613dd2f35d13bd4`.
  Both public recipes passed `makepkg --verifysource` against the immutable
  GitHub assets before publication.
- Complete serialized workspace tests, warnings-denied Clippy, release/package
  automation, portable Foot provenance, package-content/runtime validation, and
  accepted active-tab, wide-grid, and sparse-publication evidence passed. Independent candidate review found
  alpha-specific install wording and a binary split-package recommendation;
  both were corrected and regression-covered before the replacement candidate
  was built or promoted.

## Alpha3.3 release

[`v0.1.0-alpha3.3`](https://github.com/ministry-of-taste/splinterm/releases/tag/v0.1.0-alpha3.3)
was published as a GitHub prerelease on 2026-08-14 and distributed through both
AUR package bases as `0.1.0alpha3.3-1`. New persistent and transient Lairs start
with `Dojo 1`; later implicit Dojo names advance from the highest exact canonical
`Dojo N` in that Lair without reusing gaps. Explicit and persisted names remain
unchanged, generated Lair names remain collision-resistant, and bounded
exhaustion is reported as a stable machine `invalid_argument`.

- Candidate workflow run `31859941186` built commit
  `0c4276703eaa01b347fdbeb6327669b2b109e8b6` once. Candidate manifest SHA-256:
  `da995d5f0fe7dcd3993ebdc289160d1ef9fd1fcf325a3c46c353a2967f29cce6`.
- Protected promotion workflow run `31860432730` created the versioned tag,
  published and downloaded the exact five-asset set, verified every hash, and
  retained publication receipt artifact `9240460284`.
- Source archive SHA-256:
  `cf6726439a2b8977610453edd3368700304727027753f88e39ed88302dea1093`.
  Main package SHA-256:
  `5104941b47776b1a06aea044c50a4179afcc9fa6c843a663d47ac728d99bc456`.
  MCP package SHA-256:
  `a5b8e5ac5c583df75275881e2527940d3a999c20cb33e2521a366840d2abc4c2`.
- AUR source package commit: `8f373bf27f14831722c59fcd6e3f7f3ac2cd1907`.
  AUR prebuilt package commit: `b618d457bcc43a9a1993b596479c9d22c9cd1f25`.
  Both public recipes passed `makepkg --verifysource` against the immutable
  GitHub assets before publication.
- Full serialized workspace tests, Clippy with warnings denied, release/package
  automation tests, portable Foot provenance, exact package-content checks, and
  two fresh read-only release reviews passed. An isolated development build on
  freeside confirmed `Dojo 1` without stealing focus; no installed Alpha3.3
  graphical acceptance was run.

## Alpha3.2 release

[`v0.1.0-alpha3.2`](https://github.com/ministry-of-taste/splinterm/releases/tag/v0.1.0-alpha3.2)
was published as a GitHub prerelease on 2026-08-14 and distributed through both
AUR package bases as `0.1.0alpha3.2-1`. It prevents held Backspace and other
ordinary terminal input from terminating the Wayland client when a pane's
bounded command queue is temporarily saturated. Pending input remains globally
bounded, pane-bound, ordered before focus/control changes, atomic at input-event
boundaries, and safe across pane teardown; file-drop input uses the same path.

- Candidate workflow run `31849090413` built commit
  `647b0eaa83314d588ce7b5bf97e65578e6d0f96f` once. Candidate manifest SHA-256:
  `40dee74487229451e65fb5c871d8f163dfdaf867f839fe5f04e76dcad7a0ec5f`.
- Protected promotion workflow run `31849556572` created the immutable tag,
  published and downloaded the exact five-asset set, verified every hash, and
  retained publication receipt artifact `9237255756`.
- Source archive SHA-256:
  `7684923026cefe8373a13468988286b4b83e214ab4bb04c06d75a44046e0d868`.
  Main package SHA-256:
  `868729721fc901d7b6c507974f0817c3f9e21901f393dad9e6e695f0a41bd6c4`.
  MCP package SHA-256:
  `2532a8171934a11ea1f6ac78b7021e4e9857fb68cc9ecb8932729c4f881dea1f`.
- AUR source package commit: `908d85a14f9a80abd57fbe1b662bf91dc6841e66`.
  AUR prebuilt package commit: `2b0c55c9a8d63526072e4e083c3a7105aed73c5b`.
  Both public recipes passed `makepkg --verifysource` against the immutable
  GitHub assets before publication.
- Full serialized workspace tests, Clippy with warnings denied, release/package
  automation tests, portable Foot provenance, focused queue regressions, and
  independent read-only review passed. No graphical held-Backspace acceptance
  was run for this hotfix.

## Alpha3.1 release

[`v0.1.0-alpha3.1`](https://github.com/ministry-of-taste/splinterm/releases/tag/v0.1.0-alpha3.1)
was published as a GitHub prerelease on 2026-08-14 and distributed through both
AUR package bases as `0.1.0alpha3.1-1`. It hides the initial tab strip for
command-bearing private XDG launches, keeps transient Lairs non-persistent while
allowing additional live Dojos and splits, and gives the selected tab its exact
theme-provided background role at the configured terminal alpha.

- Candidate workflow run `31816769542` built commit
  `f77602214ef504348845cdcc0640d641fbe2af11` once. Candidate manifest SHA-256:
  `05c16987343c5dc99901fd12ffd7b283ac16b323a4e678d0b5ded6aa45bf446d`.
- Protected promotion workflow run `31818277079` created the immutable tag,
  published and downloaded the exact five-asset set, verified every hash, and
  retained publication receipt artifact `9225923430`.
- Source archive SHA-256:
  `802ed735c6715200183426198f738a4bfb919214d15e3d300fa7d5b2b459d443`.
  Main package SHA-256:
  `b029bbf9ea06371f23d205220f1a065ab80fd82245411e83fd5be301cc3a9e42`.
  MCP package SHA-256:
  `b54357aca4c3ddd60514c0e4e38c7c3ce2d622d0c26694c8f66b138ad4af2c0e`.
- AUR source package commit: `60c89b1151cd6d748d1c9c923444059baa7bc8ca`.
  AUR prebuilt package commit: `dd74dc69d9792c21175664ef9e95861b24473498`.
- The exact package passed isolated graphical acceptance for hidden initial
  chrome, theme alpha on the strip and selected-tab body, exact selected-tab
  color and accent underline, and live transient New Dojo attachment.
- Known source-metadata limitation: the immutable Alpha3.1 source tag retains the
  previous `Cargo.lock` digest in Foot oracle provenance after the version-only
  lockfile update, so its portable provenance check reports drift. The packaged
  runtime and oracle fixtures are unaffected. `main` refreshes that digest and
  future candidate construction now fails fast on the same check.

## Alpha3 release

[`v0.1.0-alpha3`](https://github.com/ministry-of-taste/splinterm/releases/tag/v0.1.0-alpha3)
was published as a GitHub prerelease on 2026-08-14 and distributed through both
AUR package bases as `0.1.0alpha3-1`.

- Candidate workflow run `31761128534` built commit
  `11742b60cb5b502cdadb60a582b9c3c838120d2b` once. Candidate manifest SHA-256:
  `8fc8a2bd5468260b3f259f14191bf4cc931fbe9109e4744e20f4f11e1f74e077`.
- Protected promotion workflow run `31762844821` created the immutable tag,
  published and downloaded the exact five-asset set, verified every hash, and
  retained a publication receipt.
- Source archive SHA-256:
  `5dfbed061d8c0c210d5ce9f1fac7eac88989f14c551f0ccd8eb3081d3fb070cf`.
  Main package SHA-256:
  `1a7f2a31c04dfc87495740938a3e8410f2a464f99c382a0f5d563045d8798cfb`.
  MCP package SHA-256:
  `e53a2b567619d6d8058522c4e18ef077e3b575cc99889d26cc1a18d65647ead0`.
- AUR source package commit: `ca1f80f40c94e3e469973cbee81b3a210419ffce`.
  AUR prebuilt package commit: `fbe9878af3938e5df87df79ecae05d4ec39b9667`.
- Before candidate construction, the adjacent installed package matrix passed
  the final screensaver, command-palette, saved-layout, scrollback-safety, and
  file-drop runtime implementation, including package integrity, daemon health,
  exact trusted-client identity, and complete cleanup.

Full user-defined tab identity, behavior, and appearance remain a separate
post-alpha3, pre-1.0 roadmap milestone.

## Capability truth table

| Area | Classification | Current boundary and evidence |
| --- | --- | --- |
| Native Wayland presentation | Implemented and validated | Keyboard, pointer, selection, clipboard, IME, scaling, damage-driven SHM rendering, and guarded Hyprland matrices are accepted within the documented target. See [Architecture](architecture.md). |
| Native blur | Implemented and validated | Optional, compositor-capability-gated blur for translucent themes; unsupported protocol capability falls back to ordinary transparency. See [Configuration](configuration.md) and [ADR 0004](adr/0004-font-and-cpu-renderer.md). |
| Persistent sessions and explicit restore | Implemented and validated | `splinterd` owns shells, terminal state, layouts, and metadata. Client detachment does not end them; exited processes restart only through explicit restore. See [Architecture](architecture.md) and [Headless operation](headless.md). |
| Panes and multiple Dojos | Implemented and validated | Persistent split trees, focus, ratios, lifecycle operations, search, and multiple simultaneous clients are accepted. See [Usage](usage.md). |
| Configurable keymaps and trusted help | Implemented and non-graphically validated | Effective bindings have bounded deterministic search; Save, pin toggle, Preview, and Restore are closed current-Lair actions with shared palette/topology dispatch. Packaged graphical acceptance remains pending. See [Configuration](configuration.md). |
| Window-local Dojo tabs | Implemented and validated | Up to 32 client-local tabs may span Lairs; closing a tab detaches the view and does not close daemon topology. |
| Multi-client control | Implemented and validated | Exclusive controller ownership, transfer, denial, trusted forced takeover, disconnect cleanup, and observer fallback are bounded. See [Automation](automation.md). |
| JSON/NDJSON automation | Implemented and validated | Versioned schema-major-2 one-shot and subscription contracts with stable exit categories and checked-in schemas. See [Automation](automation.md) and [CLI reference](cli.md). |
| SSH relay | Implemented and validated | Policy-scoped stdio automation relay and private human graphical relay; no daemon network listener. See [Remote access](remote.md). |
| Native remote graphical client | Implemented and validated | Profile-bound OpenSSH transport, native picker/window workflow, control, reconnect diagnostics, and client-local lifecycle; remote image transfer is not supported. See [Remote access](remote.md). |
| MCP adapter | Implemented and validated | Optional, separately packaged, exact-identity adapter over the supported automation surface. See [MCP](mcp.md). |
| Agent status reporting | Released in 0.2.0; accepted in the Omarchy VM | Agents explicitly report `working`, `needs_input`, `done`, `error`, or `idle` through `splinterm agent-status`, `--output json`, or the MCP tool `splinterm.report_agent_status`; `inspect` and `topology` outputs carry an optional `agent_status`. A process inside the Splint's session or ancestry self-reports without a grant; detached agents need `agent_status_report`. Nothing is inferred from terminal content. The graphical client shows a static built-in Tabler icon per Splint in the Explorer, a per-Dojo tab badge, stale and unavailable markers, state-word search, accessible state names and filtered announcements, and a **Clear agent status** palette command; `[explorer] agent-status=no` hides it. See [Automation](automation.md#agent-status), [Human usage](usage.md#agent-status-in-the-explorer-and-on-dojo-tabs), and [MCP](mcp.md). |
| Easy agent access | Released in 0.2.0; accepted in the Omarchy VM | Per [ADR 0014](adr/0014-easy-agent-access.md): three access levels (Look, Look and type, Manage), three durations (just this once, while this workspace is open, always for this workspace), remembered grants for protected and Nix profile installs, a default-deny trusted prompt, and bounded, cancellable pending requests; a request a grant already covers is answered without a prompt. The access list with per-entry revoke and Revoke all access work through `splinterm access` (human and JSON), and `splinterm mcp setup` adds the MCP adapter to Claude Code, Codex, or VS Code in one step without granting anything (human and JSON). The **Agent access** list in the command palette revokes per entry, **Revoke all access** in the palette first shows exact counts, the Explorer marks which workspaces, Dojos, and Splints an agent can reach or is connected to, and a waiting indicator (an Explorer mark, the Window title, and the palette count) shows requests awaiting an answer. A click in the **Agent access** list only selects a row; Enter revokes. See [Automation](automation.md#easy-agent-access) and [MCP](mcp.md#easy-agent-access). |
| Terminal images | Supported documented subset | Sixel, practical static Kitty, and inline iTerm2 PNG subsets are bounded; full Kitty graphics is not claimed. See [Images](images.md). |
| Arch/Omarchy packaging | Stable release available | Immutable versioned GitHub and AUR split packages, service, desktop metadata, upgrade checks, trusted-client identity, and rollback guidance. See [Packaging](packaging.md). |
| AUR packages | 0.1.1-1 available; 0.2.0 distribution pending | Prerelease-capable channel, not stable-only. Prebuilt [`splinterm-bin`](https://aur.archlinux.org/packages/splinterm-bin) publishes `splinterm-bin` and optional `splinterm-mcp-bin` from checksummed immutable versioned-release assets without local compilation. Source-built [`splinterm`](https://aur.archlinux.org/packages/splinterm) and `splinterm-mcp` remain available. |
| Public source and versioned releases | Available | The repository, documentation, versioned GitHub releases and prereleases, and AUR packages are public. The retired rolling edge channel is no longer produced or consumed. |
| Live daemon upgrade | Implemented and validated | From 0.2.0 on a newly installed build takes over running terminals; 0.1.x to 0.2.0 restarts once. See [Packaging](packaging.md). |
| Stable release scope | 0.2.0 released | The documented Omarchy/Arch Linux target only; broader compatibility and a support lifetime are not promised. |
| Nix and broader distribution | Release-scoped Nix checks | Stable 0.2.0 includes a source flake and desktop/headless module; wider deployment acceptance and broader Linux support remain separate. |

**Classification meanings:** implemented means present in current code; validated
means required recorded evidence exists for the named scope; supported means a
documented compatibility contract exists; proposed and planned mean not current
behavior; deferred means intentionally outside the present product.

## Important limitations

- A graphical Window is a disposable view. Topology commands do not map, focus,
  move, resize, or assign compositor windows.
- Native Wayland does not imply GPU rendering, universal compositor support, or
  automatic performance superiority. The renderer currently uses CPU composition
  and Wayland shared-memory buffers.
- Persistence follows the daemon lifetime. Stopping or upgrading an incompatible
  daemon ends its child processes; saved launch metadata is never executed
  automatically.
- Machine clients do not inherit human graphical authority. Raw daemon frames and
  Rust protocol types are private interfaces.
- Controller leases are exclusive and connection-owned. Observers may read only
  within their granted scope and do not implicitly gain input authority.
- Remote graphical sessions currently exclude terminal image transfer.
- Image compatibility is deliberately narrower than full Kitty graphics; external
  file and shared-memory media are rejected.
- Configuration is focused rather than arbitrary `foot.ini` compatibility.
- Beta 1's sparse-publication implementation passed its exact reconstruction,
  bounded-retention, responsiveness, review, integration, and guarded graphical
  comparison gates. Earlier burst-output no-go evidence remains historical; new
  performance concerns require fresh attribution against the current baseline.

## Boundaries beyond stable 0.2.0

The stable release does not imply broader guarantees. Expanding its scope
requires explicit decisions and validation about:

- release channels, signed/immutable source publication, upgrades, and rollback;
- supported architectures, distributions, compositor versions, and compatibility
  duration;
- support and security-reporting processes;
- continued performance validation against the accepted Beta 1 baseline and
  explicit disposition of any new release-blocking regression;
- public installation and recovery testing beyond the maintainer workflow; and
- any promised Nix, sandboxed package, or broader Linux support.

These boundaries do not change the published stable 0.2.0 designation and
must not be inferred as additional compatibility or support promises.

## Documentation authority

| Subject | Authority |
| --- | --- |
| Current maturity, availability, validated scope, and release gates | This document |
| Product entry point and first workflow | [`README.md`](../README.md) |
| Human operation and controls | [Usage](usage.md) |
| Human and machine command inventory | [CLI reference](cli.md) |
| Ownership and system boundaries | [Architecture](architecture.md) |
| Configuration and Foot migration | [Configuration](configuration.md) |
| JSON/NDJSON policy, schemas, limits, and exit behavior | [Automation](automation.md) |
| SSH and native remote workflows | [Remote access](remote.md) |
| MCP integration | [MCP](mcp.md) |
| Image compatibility | [Images](images.md) |
| Service, persistence, policy, backup, and reset | [Headless operation](headless.md) |
| Package installation and upgrades | [Packaging](packaging.md) |
| Product direction, audiences, and outcome horizons | [Product roadmap](product-roadmap.md) |
| Development workflow and test guardrails | [`CONTRIBUTING.md`](../CONTRIBUTING.md) |

The public website may summarize these sources for readers, but it does not
replace repository authority.
