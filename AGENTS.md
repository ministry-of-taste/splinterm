# Splinterm Agent Guardrails

## 0. Branch-First Repository Workflow

### 0.1 Main Worktree Is Coordination-Only

The repository-root worktree on `main` is reserved for fetch, status inspection,
review, approved integration, and release operations. Do not begin task-file
mutations or task commits there; approved integration and release-boundary
commits remain permitted.

Before the first edit for an authorized task:

* Fetch the current remote state.
* Create a short-lived branch from the reviewed `origin/main` base for 0.2 and
  general work, or from `origin/maint/0.1` for an explicitly authorized 0.1
  maintenance patch.
* Create or use a dedicated worktree for that branch.
* Confirm the branch and worktree path before editing.

Use milestone-oriented names such as `feat/binding-help-search`,
`fix/font-reload-race`, or `docs/configuration-font-sync`. Do not use one long-lived branch for all of a
release program.

If pre-existing changes are found on `main`, partition and preserve them before
creating the task worktree. Never sweep unrelated tracked or untracked files
into the new branch, stash, commit, move, or delete them without establishing
ownership. If ownership cannot be established, do not alter the changes; stop
and obtain an ownership decision.

### 0.2 One Writer Per Branch and Worktree

One writer owns each task branch and worktree. Read-only scouting, review, and
validation may inspect that worktree. Review fixes return to the same writer and
branch by default.

Concurrent writers still require the approval in Section 1.2 and separate
branches and worktrees. Branch isolation does not authorize uncontrolled
parallel edits. Serialize dependent milestones and work that overlaps known
convergence points such as `wayland.rs`, `keymap.rs`, `action_menu.rs`, or
shared release and package files.

### 0.3 Pull Request and Merge Boundary

Each coherent branch must pass its task's focused checks, the appropriate
non-graphical boundary, actual-diff inspection, `git diff --check`, and required
independent review before merge. Record exact validation and residual risks in
the pull request.

Prefer squash merge so the owning integration branch receives one coherent
milestone commit. Merge 0.2 and general work into `main`; merge 0.1 maintenance
patches into `maint/0.1`. Delete the merged task branch and remove its worktree
after verifying the merge.

`maint/0.1` reaches `main` only by merging it, never by squash, rebase, or
cherry-pick, so every published release stays an ancestor of `main`. After each
maintenance release, merge `origin/maint/0.1` into a reviewed integration branch
cut from `origin/main` and land that pull request with a merge commit before
other work lands on `main`; `main` CI fails while a release tag is missing from
its history. The repository allows only squash merges and `main` requires linear
history, so the integration merge needs the maintainer to enable merge commits
and suspend the linear-history rule for that one merge, then restore both. That
settings change requires explicit user approval each time. New features land on
`main` first; `maint/0.1` receives fixes only.

Release candidates, promotions, tags, and publication may originate only from
`main` or `maint/0.1`; the candidate and promotion must use the same authority
branch and retain their separate approval requirements.

## 1. Cost and Delegation Stop-Loss

### 1.1 Routine Subagent Authorization

A user request that authorizes implementation, review, investigation, or delegated work also authorizes the routine subagent launches required to complete that request.

For every subagent launch:

* Announce the launch.
* State the subagent’s role.
* State its bounded task.
* Do not wait for separate user confirmation unless Section 1.2 applies.

### 1.2 Subagent Launches Requiring Approval

Ask the user before launching a subagent if the launch would:

* Introduce a new concurrent writer.
* Materially expand the scope.
* Materially increase the expected cost.
* Perform graphical testing.
* Require a destructive or irreversible action.

### 1.3 Failed or Incomplete Subagents

Distinguish execution failure from review outcome:

* An execution failure means the subagent timed out, crashed, could not access its
  required inputs, exhausted its tools before producing the requested result, or
  returned incomplete work.
* A reviewer finding defects or rejecting a milestone is a successful review, not
  a failed subagent.

If a reviewer successfully returns actionable findings:

* Apply in-scope fixes directly in the single-writer parent without asking for
  another approval.
* Run the already-authorized non-graphical validation.
* Request user input only when a finding triggers Section 1.2 or Section 2.4.
* Do not describe the review itself as an agent failure.

### 1.4 Worktree Writers

Keep the active worktree single-writer by default.

The following work may run in parallel without creating additional writers:

* Read-only scouting.
* Read-only review.
* Read-only validation.

Intentionally concurrent writers must:

* Use separate worktrees.
* Receive user approval before launch.

### 1.5 Default Agent and Review Limits

For ordinary implementation, use no more than:

* One scout or planner.
* One writer.
* Two fresh, read-only reviewers.
* One fix writer, when justified.
* One review rounds.

Ask the user before exceeding any of these limits.

### 1.6 Launch Reporting

Before launching a subagent, state:

* The subagent’s role.
* Its bounded task.
* The expected files or area of responsibility.
* The validation command.

After launch, report:

* The subagent’s scope.
* The subagent’s outcome.

Do not introduce an additional confirmation gate after launch.

### 1.7 Subagent Launch-Readiness Gate

Do not use a subagent as a substitute for the parent’s first-pass debugging,
implementation, or validation. Before every reviewer or verifier launch, the
parent must confirm all of the following:

* The coherent milestone is implemented rather than partially sketched.
* Relevant focused tests, formatters, linters, and `git diff --check` pass, or the
  prompt explicitly identifies and bounds a known failure under review.
* The parent has inspected the actual diff and handled obvious missing cases,
  error paths, cleanup paths, and requirement mismatches.
* Every referenced file and artifact exists and is readable.
* The task prompt names exact files, exact evidence, constraints, expected
  decision, and commands the agent may run.
* The agent has enough turn and tool budget to inspect the stated scope and
  produce a complete result. Do not impose a soft budget below the realistic
  read/validation workload.
* Read-only agents receive the evidence directly or an explicit bounded read
  list; do not make them rediscover broad historical context.

If this gate is not satisfied, keep working in the parent. Do not launch the
reviewer yet.

### 1.8 Review and Watchdog Reliability

* Launch fresh review only at a coherent acceptance boundary, not after the first
  draft of a fix.
* Ask reviewers for blockers and fixes worth doing now; do not ask them to
  speculate beyond the bounded milestone.
* A review rejection authorizes the parent to make bounded fixes and validate
  them under the original request. It does not create a new user confirmation
  gate.
* Before treating a watchdog warning as a missed authorization, reconstruct the
  approval chain from the full conversation, including user messages immediately
  before and after the narrow event delta. Do not blindly accept a warning that
  omits a relevant approval.
* Report a false-positive watchdog warning as such and continue only within the
  authorization actually granted.

### 1.9 Remote CLI Delegation

Claude Code and Pi use the same repository runner and contract in
[`docs/agent-delegation.md`](docs/agent-delegation.md). Remote placement grants no
additional authority and does not bypass Sections 0–3.

* The parent remains the coordinator and owns task scope, approval, acceptance,
  and publication. A remote child receives a bounded task, exact source commit,
  role, read/edit boundary, acceptance criteria, and runtime limit.
* One-writer ownership applies across machines and parent tools. An authorized
  remote worker may be the single writer; the parent must not edit that task
  concurrently. Additional concurrent writers still require approval.
* Children do not recursively delegate or escalate their own permissions. Return
  a blocker when a product decision, unavailable capability, or approval is needed.
* Use a private, job-owned checkout and durable job identity. Never repurpose,
  clean, or synchronize an existing checkout whose changes are not owned.
* After a disconnect, query the original job before retrying. Submission or a
  zero process exit is not proof of successful execution or accepted work.
* Announce the remote role, bounded scope, source, and validation plan. Record
  the result and limitations; completion still requires validation and review.
* Scouts/reviewers remain read-only. Validator jobs may execute only the
  coordinator-declared Cargo checks in the runner's build sandbox under an
  explicitly configured restricted UID. Pi receives no shell or write tools.
  Preserve command exit codes independently of the model's report.
* The build sandbox does not grant VM access. VM credentials and graphical
  operations stay coordinator-owned and retain Section 3's explicit approval,
  exact guest-window targeting, and cleanup requirements. Do not give a child
  unrestricted SSH, QMP, desktop sockets, or sudo to bypass that boundary.
* Pi's read tools are not a filesystem sandbox. Build-command isolation does
  not make the credential-bearing model process safe for hostile tasks.

## 2. Implementation Stop-Loss

### 2.1 Milestones and Validation

Split implementation work into small, dependency-ordered milestones.

Validate after each coherent milestone. Do not require validation after every individual edit.

### 2.2 Pre-Edit Notice

Before editing, state once:

* The immediate coherent change.
* The expected validation for that change.

Do not repeat this notice for every small edit within the same coherent change.

### 2.3 Actions That Do Not Require Separate Approval

Separate user approval is not required for the following actions when they remain within the authorized scope:

* Routine file reads.
* Routine file edits.
* Non-graphical tests.
* Validation commands.

### 2.4 Actions That Require User Input or Approval

Ask the user when the work requires:

* A product decision.
* A scope decision.
* A destructive or irreversible action.
* Publishing or pushing changes.
* Graphical testing governed by Section 3.
* An unexpected, material increase in cost.
* An unexpected, material increase in agent fanout.

### 2.5 Failed Expensive Commands

Do not repeat a failed, expensive command until both of the following have occurred:

* Diagnose the failure.
* Explain the next bounded attempt.

### 2.6 Graphical Test Matrices

Request approval once for the complete bounded graphical sequence, identifying
the guest target Window, permitted focus/input actions, smoke test, gated matrix,
and cleanup plan. Use the repository Omarchy VM testbed described in Section 3.

Run one guarded guest smoke case first. If it succeeds, continue with the
approved matrix.

## 3. Graphical Testing

### 3.1 VM-First Graphical Authority

Run every Splinterm graphical smoke, matrix, capture, benchmark, and packaged
acceptance case in the configured persistent Omarchy VM through
`tools/testbed/omarchy-vm.sh`. The default target is an isolated guest Window on
guest workspace 8 / `Virtual-1`; workstation workspace 8 / DP-2 is not the
default graphical test surface.

Host graphical testing is an exception. It requires separate explicit user
approval for the complete host sequence and must identify either an isolated
host test Window or the exact existing active user Window. Permission to watch
the QEMU viewer does not authorize focusing it, moving it, or sending it host
input; prefer guest-native `input` control and use bounded QMP input only as a
fallback.

### 3.2 Guest-Window Authorization

After explicit approval for a bounded graphical sequence, the agent may focus,
raise, resize, and send bounded keyboard or pointer input only to the identified
guest test Window. Authorization applies only to the named sequence and target.

Before manipulating a guest Window:

* Record its address, process ID, workspace, monitor, geometry, current focus,
  cursor, scale, and transform.
* State the exact actions and expected cleanup.
* Target actions by exact guest Window address whenever possible.
* Preserve unrelated guest Windows and processes and every workstation Window.

Do not close a pre-existing guest Window, terminate its shell/daemon, enter
commands into it, or manipulate production topology unless that exact action was
explicitly approved.

### 3.3 Abort and Cleanup

Abort immediately if input reaches the wrong guest Window, an unrelated Window
moves, the QEMU viewer receives unintended host input, or the approved target
cannot be identified reliably.

After every case, close only disposable test Windows, stop private test daemons,
remove private runtime artifacts, restore the original guest workspace, focus,
monitor state, cursor, size, and position, and verify the guest test workspace is
empty. Report anything that could not be restored.

### 3.4 Host Exception

When the user explicitly approves host graphical testing as an exception, run it
only on inactive, empty workspace 8 on DP-2 and launch the test Window without
taking focus, unless the user names an existing active Window. Apply the same
exact-target, abort, and cleanup rules, and never treat VM authorization as host
authorization.

## 4. Repository Safety

### 4.1 Foot Differential Authority

Preserve the pinned Foot 1.27.0 commit as the authority for optional Foot
differential runs; Splinterm-owned contracts are release authority under ADR
0013:

`3c5b584b0eafa772eb4376fb6eaf6643399e190e`

### 4.2 Prohibited Repository Changes

Do not:

* Modify the canonical Foot checkout.
* Translate comparison images.
* Broadly widen tolerances.
* Silently regenerate references.

### 4.3 Completion Claims

Do not claim that a slice is complete unless both of the following exist:

* Recorded validation evidence.
* Recorded review.

### 4.4 Local Client Installation and Trusted-UI Identity

The daemon grants the graphical trusted-UI bypass to a new connection only when
the client process, at `Hello`, matches the device and inode of the `splinterm`
binary adjacent to the running `splinterd` executable. On the packaged system,
that authority is `/usr/bin/splinterm` next to `/usr/bin/splinterd`.

Trust is per connection and per process (operator decisions, 2026-10-20):

* A connection trusted at `Hello` stays trusted while its own process still
  executes the file it executed at `Hello`. On every request the daemon
  re-reads that process's executable device and inode from `/proc/<pid>/exe`.
  The connection's pinned pidfd then proves the process is still the one that
  said `Hello`; on kernels without `SO_PEERPIDFD`, its recorded start time
  does.
* A new connection is also trusted at `Hello` when the same process (the same
  PID, start time, and pidfs inode where available) holds another open
  connection that was trusted at its own `Hello`, and still executes the same
  file. The daemon tracks such processes in a bounded registry, counted by
  connection. An entry ends with the process's last trusted connection, so
  trust never outlives all of a process's connections.
* A package upgrade that replaces the installed binary therefore leaves a
  running Window working, including new tabs and panes, until it is
  live-upgraded or reopened.
* A process that executes another program loses trust, and so does a process
  that can no longer be proven to be the same one. Any other process, or a
  fresh launch, must match the installed binary or the pinned snapshot below.
* The check reads the connecting process. A descriptor inherited by another
  process is not re-identified.

One exception, approved by the operator on 2026-10-18 for the upgrade handoff
(Plan 0037, ADR 0011): a daemon adopted through a verified handoff may also
grant it to a client process executing the exact sealed forward client
snapshot that the daemon verified against its package record and pinned for
its own lifetime, matched by that snapshot's device and inode. Matching is
never by content hash, a fresh daemon pins no snapshot, and a client running
any other image, including a stale pre-upgrade inode, never regains
trusted-UI authority this way.

Do not install a development client to `~/.local/bin/splinterm`,
`~/.cargo/bin/splinterm`, or another earlier `PATH` entry and then claim the
desktop launcher is installed. The packaged desktop wrapper resolves
`splinterm` through `PATH`; a shadowing user-local copy does not match the
system daemon's trusted-UI inode and exits as unauthorized before mapping a
window.

For packaged graphical acceptance, use the Omarchy VM actions
`package-build`, `package-status`, `package-install --confirm-guest-install`,
`package-launch`, and `package-stop`. Do not install or replace a workstation
package merely to run graphical acceptance.

For installation work:

* First inspect `command -v splinterm`, the desktop-launcher process `PATH`, the
  running daemon executable, and both executable device/inode identities.
* Remember that `./install.sh` and VM `package-build` deliberately package only a
  clean committed `HEAD`; neither can install an uncommitted implementation.
* Ask before replacing an existing Pacman-owned `/usr/bin/splinterm`, including
  inside the guest. The confirmed VM install path saves guest rollback copies
  and uses passwordless guest `sudo`; a separately approved host exception must
  save a rollback copy, use `pkexec`, remove any shadowing user-local client, and
  disclose that Pacman integrity checks will report the host file as altered
  until reinstall or upgrade.
* Validate non-graphically with normal human-mode `/usr/bin/splinterm list`,
  checksum and ownership checks, `command -v splinterm`, and
  `desktop-file-validate`. Do not use `--output json list` as the trusted-UI
  check: machine mode intentionally uses the automation role and may be denied
  without a persistent policy even when installation is correct.
* Do not launch the desktop entry as validation unless the user has separately
  approved the guarded graphical test sequence in Section 3.
