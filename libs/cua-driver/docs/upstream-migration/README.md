# Downstream migration to Driver 0.38.0

Started October 22, 2026 (PDT), following `CUA_UPSTREAM_MIGRATION_PLAN.md`
in the original checkout. This is a private downstream integration, not an
upstream contribution or a deployment approval.

## Immutable inputs

- Release: `cua-driver-rs-v0.38.0`, `85e2b1318bd77ccf3cfa88ccdf875a4bb7eaed19`.
- Upstream main refreshed at start: `4a44f4d39677c2d906d588b5b205a85aa8353354`.
- Fork main: `0d5c2dd3065fef7d0f09099fbd6071ad698ffbb1`.
- Private Driver/plugin: `19ad9978ccf4b3afa1de91413862c69680649f81`.
- Work branch: `work/cua-upstream-migration-038`, isolated plain-Git worktree.

Local evidence and byte-for-byte rollback copies are retained under
`~/.cache/cua-upstream-migration-20261022/`. `baseline.json` records resolved
executable paths, SHA-256 hashes, package versions, original checkout status
and diff hash. `evidence/release.json` retains the component release's asset
digests; `evidence/SHA256SUMS` retains its published checksums. These are
preservation inputs, **not a qualified full-environment rollback**.

Installed hyprutils remains 0.14.2-1; the package database offers 0.14.3-1.
The installed `cua-hyprland-plugin` and the activation provenance disagree
(0.33.2-3 versus 0.32.0-3). No active compositor, package, launcher or MCP
configuration is changed by this branch. Original lockfile modifications and
`docs/remote-gui-testing-mbp.md` remain in the original checkout.

The release's attached `_install-rust.sh` bakes 0.38.0, whereas the canonical
script on refreshed upstream main still bakes 0.37.0. The component tag prefix
is `cua-driver-rs-v`; an explicit `CUA_DRIVER_RS_VERSION` overrides the baked
version, which precedes API fallback. Keep the target explicitly selected.
The downloaded `SHA256SUMS` matches release API digest
`a2c338d608ded32fe3e41918cc571a2769a38fc5a1316bab7d14a9b615590645`;
the attached installer matches
`0be0bd269056a7c0c5b15e77ff6c64c880267d091957c97efc19103380e4aeb7`.
These checks establish downloaded-file integrity, not native qualification.

## Patch ledger

Paths below are relative to `libs/cua-driver`; Rust owners are under
`rust/crates/`. Source commit references retain original contribution identity;
adapted commits credit the source author rather than copying a whole branch.

- **Recording lifetime/end reasons** — source `1139f7dd2c6b6b3b936e0ca252ab14382251c941`.
  Destination: core `session.rs` / `recording.rs`, SDK `runtime.rs` (maintenance
  moved out of `serve.rs`). Keep only the exact recording owner alive, not its
  transport or all sessions. Ownerless backstop must check ownership under the
  recorder lock. Owner test: SDK runtime recording idle lifecycle test.
- **Recording CLI** — sources `b79ab4e35dcf9d5ad19cbc2759ee4d0fedf84f99` and
  reconciliation `f6ab494c477be8199580285fa7751bd3d1c93216`.
  Destination: `cua-driver/src/cli.rs`. Preserve raw recording flags; parse
  values once, reject malformed invocations before I/O. Owner tests: CLI argv
  and process-level recording tests.
- **Grayscale resize** — source `c79715d38` (only its image helper hunk, not
  the historical headless-X implementation). Destination: core `image_utils.rs`.
  Owner test: resize color-format/dimensions/pixels regression.
- **Transient daemon connections** — source `74a5507e11e0866ad101b9c92ec548eb70ddd9b9`.
  Destination: core `daemon.rs`. Upstream already resumes partial reads/writes
  without resending and CLI requires a daemon (no in-process fallback).
  Carry only bounded pre-dispatch transient connection retry. Owner tests:
  core daemon transport tests and CLI missing-daemon refusal tests.
- **Exact recording-window association** — source `f6ab494c4`'s Linux
  `recording_hooks.rs` reconciliation. Destination: Linux recording hooks.
  Replace PID-first selection with unique-window selection; connection-local
  Wayland IDs cannot establish identity across connections. Owner test: Linux
  recording window selection, including two windows with one PID.
- **Basic Hyprland / fractional scale / multi-monitor composition** — retire
  old implementations in favor of upstream #4305 / #4244. Do not overwrite
  `platform-linux/src/wayland/hyprland.rs` with the candidate's old owner.
- **Capture/semantic/held-input invariants** — sources `ccfb84ea7` through
  `21aeb1a86` and `af6900a22` on the private candidate. Still require reconciliation
  against upstream `wayland/hyprland.rs`, input/AT-SPI/snapshot owners and plugin
  `src/`. Required source/held capabilities must refuse when absent; stock
  plugin is not an equivalent. **Deployment gate: not yet ported/qualified.**
- **Recording immutable turn geometry** — sources `803ef40f3`, `559d0e7e0`,
  `6a73e190d`, `f2947a84a`. Map core recording hooks and platform capture
  preparation rather than re-observing topology after dispatch.
  **Deployment gate: not yet ported/qualified.**
- **Hidden-workspace launch** — sources `1c3c5c889`, `d4199a971`, `811c82a24`,
  `70f360855`, `ec42f733c` (including child/late-window lifetime guard).
  Destination: current Hyprland launch and plugin window lifetime owners.
  **Required, not retired; not yet ported/qualified.**
- **Native fixtures** — sources `e9f911da6`, `e6301d389`, `a66415798`,
  `1a1492b81`, `9acd3f6bb`, `275afbfc6`, `19ad9978c`.
  Destination: `cua-driver-e2e`, native GTK fixtures, canonical Linux runners.
  Keep independent pixels/native-event oracles. **Integration pending.**
- **Omarchy keymap / IME** — recovered installed package's `downstream.patch`
  and all provenance/profile files into `rollback/distro-package/` in the local
  evidence directory. Patch SHA-256:
  `87d3f560cfbd195748321a7e7ad2cb921c77cc022877de7890d3a711940f6e3c`;
  its recorded upstream revision is `c82d32e3e1adbc6578a148962002ddf6e3e8a15a`.
  It admits text-equivalent remaps while retaining per-chord refusal and hooks
  the exact 0.56.2 input-popup surface lifetime. Separate from private
  source-guard patches. **Port and native evidence pending.**
- **Historical headless-X / KRDP** — exclude; no current headless-X availability
  claim, no KRDP sidecar dependency in the target deployment.

## Remaining qualification and cutover gates

### Active upstream ownership discovered during implementation

Do not reimplement these contributions to bypass their history or readiness
gates. Reconcile the private candidate with the selected upstream work before
continuing Phase 3:

- [#5574](https://github.com/trycua/cua/pull/5574), head
  `a26674e3223a9b25ca46baa6f5fd67800b5ce286`: preserves uncertain AT-SPI outcomes
  and prevents batch foreground replay; salvages Victor Vogel's #5454.
- [#4387](https://github.com/trycua/cua/pull/4387), draft head
  `59d53d7d05b9b47ded5e2a252d259c05865112be`: qualified monitor snapshots and
  signed cursor placement, retaining CARLOSDAVID33 and Iann29's contributions.
  Its current body explicitly reports a same-sized topology identity gap and
  missing native acceptance; it is not an already-qualified equivalent.
- [#5604](https://github.com/trycua/cua/pull/5604), draft head
  `a7dc1851751438ddbd5fb15a9be18c0532dd0842`: fractional logical Wayland frames
  and clipped pixels, salvaged from lorenzozanee's #3887 via #4386.

Also keep recording scope work (#3667) and native Wayland identity work (#3448)
in view. None of these unmerged PRs is silently included in this branch.

### Native qualification

Phase 3 invariants must be implemented and tested before claiming a candidate.
Then build Driver/plugin at the exact candidate SHA in a disposable desktop
with the complete target Arch/Omarchy package set. Use the existing measured
profile/bundle tooling and isolated ALPM lifecycle controls; do not relabel an
old module to bypass ABI checks. Run the complete canonical desktop matrix once
stable, including dual-display 1.5x native input/capture, cleanup uncertainty,
recording geometry, hidden launches, keymap and IME.

Only after qualification may a coherent default CLI/MCP/plugin/activator bundle
and full-environment rollback be staged. A fresh-session cutover needs explicit
authorization; never hot-swap the user's native module. No upstream PR is
authorized. Missing native evidence is a blocked gate, not a passing result.
