# Cua Driver: multi-display and scaling investigation

Investigated **2026-10-02, Pacific Time (PDT)**. Research and read-only probes only;
this is not an implementation, a selected upstream work item, or certification.

## Result

**Upgrade alone will not restore full desktop use on shuvdev.** Upstream now
compensates fractional scale for one origin-aligned, unrotated Hyprland output,
but still rejects multiple outputs. The substantial multi-monitor implementation
already exists in open [PR #4305](https://github.com/trycua/cua/pull/4305).
Reuse and finish that contribution rather than implement it again.

For the normal home layout, the practical candidate is latest upstream + #4305
+ capture/topology safety and native validation. General-purpose support also
needs an explicit contract for per-display targeting and rotated/flipped outputs.
Background plugin delivery is a separate capability, not a prerequisite to start
the MCP server or observe the desktop.

## Revisions and evidence boundary

- Fork left on `d2a2fd015585dd287ae3a90296bebbe271eaf68b`; existing staged
  `docs/remote-gui-testing-mbp.md` was not changed.
- Fetched upstream main: `ab628e0d1cf1e993eef2f7f99d9ed8faf364506c`, committed
  **2026-10-02 01:10:57 PDT**. Inspected in detached worktree
  `/tmp/shuvcode/cua-upstream-display-research`; no merge into the fork.
- Baseline installed/tested during the trip: `cua-driver-rs-v0.30.1`,
  `039783f9221a08c0daf9cda65a460fc4f346fa6e`.
- Latest published component release inspected: `cua-driver-rs-v0.32.0`,
  tag commit `58aba84b5b83d77e7e2b0f006547699eb594e50d`; published
  **2026-10-01 14:34:36 PDT**. Both canonical installer scripts bake **0.32.0**.
  GitHub labels this release prerelease, but its component assets exist and the
  normal baked-version distribution resolves it; the repository-wide Latest
  badge is not the authority. Explicit version overrides still take precedence.
  [Installer precedence][installer]; [Windows installer][windows-installer];
  [component release][release].
- Downloaded 0.32.0 Linux x86_64 binary kit into
  `/tmp/shuvcode/cua-display-release-032`, without installation/replacement.
  Archive SHA-256 matches the release's `checksums.txt`:
  `bb006010864e9a93b9f67035e345c3a076f0908d42fbe493bb21ede5f82f39a6`.
  This is a checksum check, not a claim of independent signature verification.
- #4305 head inspected: `bfe6de20c9e3edb5a801e4cb88ffa918cae29ff7`.
  PR descriptions' live-test results below are **contributor-reported**, not
  rerun or independently certified by this investigation.

## What changed after 0.30.1

1. **Fractional desktop scaling is fixed for a restricted single output.**
   [#4244](https://github.com/trycua/cua/pull/4244), commit
   `3034435724d4621679f9e99f4dc0d348d6a684ef`, first belongs to **0.30.4**,
   not 0.30.3. Tag ancestry and the changelog both confirm this. It derives
   logical dimensions with `round(mode / scale)`, normalizes desktop capture to
   those dimensions, and uses the same logical extent for virtual-pointer
   motion. Tests cover scales 1, 1.25, 1.5, 1.6666666, and 2.
   [Changelog][changelog]; [single-output qualification][single-frame];
   [capture normalization][normalization]; [pointer extent][pointer-extent].
2. **Multiple outputs, nonzero origins, and rotation are still refused by
   upstream main.** `screen_size_from_monitors` explicitly requires exactly
   one output and an unrotated output at `(0,0)`. Updating to 0.32.0 cannot
   remove this restriction. The relevant main files are unchanged from the
   0.32.0 tag. [Single-output qualification][single-frame].
3. **Screenshot downsizing compensation already exists in common code.**
   `desktop_capture_scale` records separate X/Y ratios per session and maps
   later desktop action coordinates back to the uncapped image. A `capture_id`
   bypasses this map because its immutable capture transform already does the
   conversion. Do not add another multiplier in shuvcode/MCP.
   [Session cap mapping][cap-map]; [capture transform][capture-frame].
4. Relevant nearby changes include native-window capture normalization,
   keyboard/plugin source packaging and keymap handling. These do not replace
   the missing multi-monitor desktop model. See the component changelog and
   the diff from the baseline, not a repository-wide version number.

### Released-binary reproduction on shuvdev

Read-only MCP process: `cua-driver 0.32.0 mcp --direct --no-overlay`, fresh
Hyprland session discovery, Wayland enabled, X11 variables unset, isolated
Driver home and telemetry disabled. **No display or input changes.**

- MCP initialize succeeds and reports `cua-driver / 0.32.0`.
- `list_windows` succeeds; nine windows at observation time.
- `get_screen_size` returns `isError: true`, `tool_invocation_failed`, and
  `Hyprland display identity requires exactly one active output`.
- Process exits cleanly. Evidence:
  `/tmp/shuvcode/cua-display-latest-readonly.json` and reproducible helper
  `/tmp/shuvcode/cua-display-latest-readonly.py`.

No new single-output scaling, desktop input, plugin-background, or full E2E
tests were run on the real desktop during this investigation.

## Coordinate contract required

There are distinct spaces; conflating them is the bug:

- **Backing pixels:** actual captured buffer per output/surface.
- **Compositor layout coordinates:** global logical positions, potentially
  negative; Hyprland window geometry and pointer placement use these.
- **Desktop-frame coordinates:** a nonnegative screenshot/action frame based
  on the union's bounding box, translated relative to the powered outputs.
- **Encoded-image coordinates:** desktop frame after an optional image cap.
- **Window/surface-local coordinates:** independent of monitor origin; derive
  their scale from the actual surface capture and logical window size.

For an unrotated output `i`, use compositor-rounded logical size
`Li = round(Pi / si)`, not an assumed global scale. Let `O` be the powered
desktop bounding-box origin, `F` its dimensions, and `E` delivered PNG size:

```text
desktop_frame = encoded_point * (Fx/Ex, Fy/Ey)
layout_point  = desktop_frame + O
output_local  = layout_point - output_origin_i
backing_point = output_local * measured_backing_to_logical_ratio_i
```

Keep separate axes through resize rounding. Apply each conversion **once**.
Rotation/flipping needs an affine transform/inverse, not scalar compensation.
The common registry already represents a six-coefficient affine transform;
extend its identity/lifetime metadata rather than duplicate this arithmetic in
transport wrappers. [Affine representation][affine].

### Expected home-layout result

Current readback: both outputs **3840×2160, scale 1.5, transform 0, DPMS on**.
DP-2 is at `(0,0)`; DP-1 at `(2560,0)`. Each occupies **2560×1440 logical**
pixels. The aggregate logical desktop must be **5120×1440**.

Without an image cap, desktop `(3840,720)` is DP-1's center: local logical
`(1280,720)`, backing `(1920,1080)`. With an explicit 2560-pixel long-edge cap,
the delivered desktop is 2560×720 and that same point is encoded `(1920,360)`.
Multiplying the already logical desktop point by 1.5 would be incorrect.

Mixed-scale desktops have **no single physical-pixel multiplier**. #4305 reports
the maximum scale in `get_screen_size`, but uses per-output resizing to compose
the logical screenshot. That scalar is compatibility metadata, not the
conversion to apply to every point. A composed `get_desktop_state` can report
normalization scale 1 because the input PNG is already logical; document these
fields explicitly and expose per-output scale/geometry. [PR capture][pr-frame].

## Existing contribution to build on

- [#4161](https://github.com/trycua/cua/issues/4161): original multi-monitor
  refusal; [#4219](https://github.com/trycua/cua/issues/4219): fractional scale.
- [#4217](https://github.com/trycua/cua/pull/4217) was closed as superseded by
  **#4305**, not merged as a completed fix.
- #4305 already implements logical powered-output bounds, negative origins,
  per-output `grim -o` capture with logical resizing/composition, separate
  pointer-layout bounds including standby outputs, window-bound rebasing,
  real pointer readback, per-call snapshots and negative-position cursor fixes.
  It keeps single-output capture on the existing cascade. Multi-output capture
  requires `grim`; `/usr/bin/grim` is present on shuvdev. [PR frame/capture][pr-frame].
- Contributor-reported live results cover negative X/Y, mixed/fractional scales,
  clicks, scrolls and seam-crossing drags; 11 layouts/107 checks are reported by
  a contributor. Rotated outputs remain refused. The PR explicitly acknowledges
  stale captures after same-sized layout changes and missing canonical Hyprland
  E2E. Current listed checks are attribution/release checks, not desktop proof.
- A local, non-checkout `git merge-tree --write-tree upstream/main
  upstream/research-pr4305` detects **four conflicting files** at the pinned
  revisions: Linux `tools/impl_.rs`, `wayland/mod.rs`, `overlay.rs`, and Windows
  `overlay.rs`. Several are style/refactoring overlap; the virtual-pointer
  positioning conflict must preserve main's dispatch behavior plus PR origin
  mapping. Rebase and rerun tests; do not call this a clean drop-in cherry-pick.
  Evidence: `/tmp/shuvcode/cua-display-pr4305-merge-tree.txt`.

Preserve CARLOSDAVID33's and Iann29's commits/credit. The PR already carries the
original #4217 contribution and the contributor's follow-ups. Do not duplicate
the work to discard attribution. [Contributor policy][contribution].

## Remaining work before calling it proper support

### 1. Finish the aggregate Hyprland desktop path

Reconcile #4305 with pinned latest main; keep one qualified topology snapshot
for capture, window metadata, input endpoints and overlay placement. Snapshot
capture bounds and pointer-layout bounds separately: DPMS standby removes visible
pixels but does not necessarily remove the output from absolute-pointer layout.
Reject unsupported layouts/failed observations instead of falling back to one
output's physical extent. Integrate with native identity/routing fixes in
[#4396](https://github.com/trycua/cua/pull/4396); its author and #4305's author
already request a combined desktop + native-window smoke after merge ordering.

### 2. Bind captures to complete geometry identity, not only dimensions

**A safety blocker remains in #4305.** Common capture publication/admission binds
`PrimaryDesktop`, session/generation, immutable pixels, transform and native
width/height; it does not bind the compositor/output topology. Swapping two
same-sized monitors keeps the dimensions while moving the screenshot's target.
Per-call snapshots cannot detect that a screenshot from a *previous* call used
the old layout. This gap is acknowledged by #4305 and draft
[#4387](https://github.com/trycua/cua/pull/4387). [Registry admission][registry];
[Linux publication/admission][capture-frame].

Add common capture-frame/topology identity (or equivalent generation binding),
populated by each platform adapter: compositor session, output identities,
origins, logical/native sizes, per-output scale/transform, disabled/DPMS/mirror
state and pointer coverage. Compare at admission and immediately before native
dispatch; reject stale captures even if the bounding dimensions are unchanged.
Invalidate session-capped images/zoom-derived coordinates on layout changes too.
Cover multi-call held gestures: cancel/release safely on topology change rather
than rescale a held device under a new frame. Requery-before/after guards are not
an atomic compositor transaction; state that residual limitation honestly.

### 3. Finish coordinate consumers beyond screenshots and clicks

Keep AT-SPI/native window-local reconstruction, surface capture scaling, pointer
shapes, cursor render placement and recording markers consistent. Do not apply
a monitor multiplier to browser CSS coordinates or normalized surface-local
plugin coordinates. Test windows moved between differently scaled outputs and
windows straddling the seam, including popups/decorations.

**Native desktop-to-window bug:** `desktop_to_window_local()` unconditionally
uses X11 `translate_coordinates(xid as u32, root, 0,0)`, even for native Hyprland
full-address targets. The window-targeted click path calls it when desktop-frame
coordinates are requested. With X11 variables unset this can fail rather than
translate a valid point; native addresses are not XIDs. #4305 leaves this helper
unchanged. Use the captured frame-to-layout transform, then subtract the fresh,
exact native client-surface origin. Preserve PID/full-address identity checks;
do not guess a low-word XID or substitute a first-window/title match.
[Translation helper][window-translation]; [click caller][window-click].

**Plugin coordinates are already logical:** latest plugin `target_geometry()`
uses `surfaceLogicalBox()`. Background motion goes to exact client pointer
resources in surface-local coordinates; foreground motion warps to logical
surface origin + local point. Another divide by 1.5 is wrong. The plugin has no
single-monitor/scale-1 geometry gate; that does **not** certify its native
multi-output behavior or the installed Omarchy package. Preserve target tokens,
geometry revisions, desktop-transition revocation, synthetic-resource cleanup,
and effect-free-only retry rules. [Plugin geometry][plugin-geometry];
[plugin foreground/background dispatch][plugin-input].

**Overlay support exists, but common placement still needs repair:** layer-shell
already uses per-output logical surfaces and signed origins. Latest common
`paint_cursor()` returns early for `x < -100`, so real cursors on most left-of-zero
monitors are treated as unplaced; tests of output selection alone miss this.
#4305 contains the shared sentinel repair: retain and validate it across all
adapters, not just Linux. Logical-buffer placement is separate from crisp
fractional/high-DPI rendering. Do not advertise fractional-scale buffer fidelity
or unqualified fallback/rotation metadata as already fixed. [Common painter][painter];
[Wayland output rendering][wayland-overlay].

**Additional source-level gap:** #4305 does not change `recording_hooks.rs`.
Its `hyprland_output_point` accepts layout coordinates only when
`0 <= x < display_width` and `0 <= y < display_height`, without subtracting a
desktop origin. A visible target left/above the origin loses its recording point,
or a positive-origin target gets an offset marker, despite valid composed pixels.
For example, a window at `(-2500,20)` containing point `(-2250,270)` in a desktop
starting at `(-2560,0)` should yield frame `(310,270)`, but the current helper
returns `None`. This is a source-derived counterexample, not a live reproduction.
Use the same captured frame/transform for recording screenshots and points,
including whether a single-output recording retains backing or logical pixels.
[Recording helpers][recording].

### 4. Explicitly define the supported display surface

For the home requirement, one qualified composite logical desktop plus monitor
metadata is sufficient; a new per-display selector is not required to make both
monitors usable. However, the public target parser/visual contract accepts only
`display_id="primary"`. Per-display capture/target selection is **not** implemented
by simply adding `monitors` to `get_screen_size`. If that is part of “proper
multi-display,” add stable display identifiers, coordinate-space/units metadata,
selection semantics and capability reporting through common DTOs, CLI/MCP,
manifest and generated SDKs under the repository's RFC contract. Existing output
extension maps allow additive metadata, but don't provide a typed selector.
[Target parser][targets]; [visual source][visual]; [output DTOs][outputs].

Support transform 0 first with named limitations. [#3969](https://github.com/trycua/cua/pull/3969)
is open single-output rotation work with older assumptions; it is not combined
multi-monitor, scaled, rotated/flipped support. General rotation requires all
transform matrices applied to capture/input/overlay and corresponding tests.
Specify gap/overlap/mirror and standby behavior: a point in a black gap or an
off-output region must not silently clamp/snap into a different visible target.

Audit per-output capture cancellation/deadlines and decode/allocation limits.
#4305 bounds the logical canvas, but `grim` uses blocking `Command::output()` and
the shown capture path has no explicit subprocess deadline. Capture each output
without silently stretching a single failed/incorrect output over the union.

### 5. Treat background delivery and local MCP startup separately

The current `/home/shuv/.local/bin/cua-hyprland-mcp` refuses startup unless the
trip-only DP-1/1× layout and plugin input-v3 transport are ready. No upstream
version update changes that shell check. After qualifying a candidate, retain
live compositor discovery and enable Wayland, but remove trip-specific admission
from MCP startup; report unsupported operations individually. Plugin/background
availability must remain a capability with typed refusals, not an excuse to
return success for foreground fallback.

Raw background package admission in latest source is limited to qualified native
Inkscape/LibreOffice builds. A display fix does not enable raw background input
for arbitrary Chromium/Electron/XWayland targets; semantic accessibility delivery
and foreground delivery are different routes. [Compatibility gate][compatibility].

The installed Omarchy plugin remains disabled for the home profile. Enabling it
and qualifying dual-output/1.5× background delivery must be explicit later work,
with ABI/profile checks and no primary focus/pointer disturbance. No plugin or
host configuration was changed here.

### 6. Preserve cross-platform semantics

Use common topology/transform/lifetime owners with thin native adapters; do not
rename every platform's native action units to Hyprland logical pixels. macOS
window captures map backing pixels to logical screen points and its overlay is
currently main-screen scoped. Windows absolute input already handles signed
virtual-desktop origin/extents with its native `65535 / (extent - 1)` endpoint
normalization; that denominator is not the Wayland protocol. Its virtual-screen
overlay still uses backing scale 1 and has a per-monitor-DPI fidelity TODO.
Common cursor negative-position fixes and capture metadata changes therefore
need macOS/Windows coverage even for a Linux-motivated feature. Preserve the
difference between agent-overlay movement, primary-pointer movement, and truly
background client delivery. [macOS frame mapping][mac-frame];
[Windows virtual-desktop mapping][win-frame]; [Windows overlay][win-overlay].

## Validation required for a candidate

Focused tests first, then the complete canonical matrix once on the stable exact
candidate SHA, according to [the harness guide][harness]. Do not repeatedly run
expensive desktop matrices while the implementation is changing.

- Unit/property tests: origins on every side of zero; scales 1, 1.25, 1.5,
  1.6666666 and 2; mixed scales; independently rounded axes; frame↔layout↔image
  round trips; finite/overflow/allocation bounds; mirrors, standby, gaps and
  unsupported transformations. Same-size output swaps/DPMS/origin/scale changes
  must invalidate old captures and release gestures without dispatch.
- Focused common/platform builds:
  `cargo test -p cua-driver-core -p cursor-overlay -p platform-linux --locked --lib`
  from `libs/cua-driver/rust`; preserve existing tests rather than replacing them
  with a contributor's smaller harness. Compile/test Windows and macOS for shared
  capture/overlay changes; do not claim Linux evidence proves their behavior.
- Native home-layout oracle: full desktop 5120×1440; independently compare both
  halves against per-output captures; click/scroll/drag/hover/held gestures at
  corners, monitor centers and seam; verify target app event logs and real
  pointer position, not just Driver success replies. Repeat with an image cap.
- Background oracle: real native apps on each monitor at 1.5×, with a different
  foreground app; log actual target effects and verify primary focus/pointer
  unchanged. Test monitor moves, occlusion, target under primary pointer,
  compositor restart and lock/DPMS typed refusals. No fallback by focus stealing.
  Existing production active-primary and foreground-grab proofs require a single
  unscaled origin output; they do not certify this layout. Add new controlled
  fixtures/proofs rather than deleting those safety preconditions.
  [Current foreground proof][foreground-proof]; [active-primary proof][primary-proof].
- Supplemental layouts: negative X/Y, vertical, mixed/fractional DPI, hotplug,
  mirrored/standby outputs, same-size topology changes, rotated/flipped named
  refusals or validated transforms. Include overlay and recording-video placement.
- Canonical native Linux: `scripts/ci/linux/run-rust-e2e.sh`; in that prepared
  desktop also run the ignored `hyprland_foreground_test` and
  `hyprland_native_observation_test` per the guide. Full Windows/macOS evidence
  is required when changing common behavior or their cursor adapters. A temporary
  source-only review or contributor's smoke is not certification.
- Last: MCP initialize/tool schemas, desktop capture→capture-bound action in one
  persistent session, and reconnect in shuvcode with the normal layout intact.

## Recommendation and non-actions

**Finish #4305's existing work on latest main, add topology-bound capture safety,
repair secondary coordinate consumers, then qualify it on shuvdev's normal
two-display/1.5× layout.** Do not create a second global scaling shim. Expand
per-display selection and rotated/flipped support through an explicit shared
contract if those are required beyond the home setup.

No production code, monitor/plugin settings, installed Driver, MCP wrapper,
remote-desktop service state, GitHub issue/PR or fork branch was changed. No upstream PR was
opened. The new investigation file and isolated fetched/downloaded evidence are
the deliverables; selecting implementation remains a separate user decision.

## Pinned primary-source references

[installer]: https://github.com/trycua/cua/blob/ab628e0d1cf1e993eef2f7f99d9ed8faf364506c/libs/cua-driver/scripts/_install-rust.sh#L613-L635
[windows-installer]: https://github.com/trycua/cua/blob/ab628e0d1cf1e993eef2f7f99d9ed8faf364506c/libs/cua-driver/scripts/install.ps1#L145-L157
[release]: https://github.com/trycua/cua/releases/tag/cua-driver-rs-v0.32.0
[changelog]: https://github.com/trycua/cua/blob/ab628e0d1cf1e993eef2f7f99d9ed8faf364506c/libs/cua-driver/rust/CHANGELOG.md
[single-frame]: https://github.com/trycua/cua/blob/ab628e0d1cf1e993eef2f7f99d9ed8faf364506c/libs/cua-driver/rust/crates/platform-linux/src/wayland/hyprland.rs#L330-L371
[normalization]: https://github.com/trycua/cua/blob/ab628e0d1cf1e993eef2f7f99d9ed8faf364506c/libs/cua-driver/rust/crates/platform-linux/src/tools/impl_.rs#L11981-L12138
[pointer-extent]: https://github.com/trycua/cua/blob/ab628e0d1cf1e993eef2f7f99d9ed8faf364506c/libs/cua-driver/rust/crates/platform-linux/src/wayland/mod.rs#L1292-L1329
[cap-map]: https://github.com/trycua/cua/blob/ab628e0d1cf1e993eef2f7f99d9ed8faf364506c/libs/cua-driver/rust/crates/cua-driver-core/src/desktop_capture_scale.rs#L1-L87
[capture-frame]: https://github.com/trycua/cua/blob/ab628e0d1cf1e993eef2f7f99d9ed8faf364506c/libs/cua-driver/rust/crates/platform-linux/src/capture_action_frame.rs#L59-L179
[registry]: https://github.com/trycua/cua/blob/ab628e0d1cf1e993eef2f7f99d9ed8faf364506c/libs/cua-driver/rust/crates/cua-driver-core/src/capture_registry.rs#L817-L877
[affine]: https://github.com/trycua/cua/blob/ab628e0d1cf1e993eef2f7f99d9ed8faf364506c/libs/cua-driver/rust/crates/cua-driver-core/src/capture_registry.rs#L185-L275
[targets]: https://github.com/trycua/cua/blob/ab628e0d1cf1e993eef2f7f99d9ed8faf364506c/libs/cua-driver/rust/crates/cua-driver-core/src/action_target.rs#L83-L101
[visual]: https://github.com/trycua/cua/blob/ab628e0d1cf1e993eef2f7f99d9ed8faf364506c/libs/cua-driver/rust/crates/cua-driver-contract/src/visual.rs#L69-L98
[outputs]: https://github.com/trycua/cua/blob/ab628e0d1cf1e993eef2f7f99d9ed8faf364506c/libs/cua-driver/rust/crates/cua-driver-contract/src/outputs.rs#L366-L492
[pr-frame]: https://github.com/trycua/cua/blob/bfe6de20c9e3edb5a801e4cb88ffa918cae29ff7/libs/cua-driver/rust/crates/platform-linux/src/wayland/hyprland.rs#L421-L738
[recording]: https://github.com/trycua/cua/blob/ab628e0d1cf1e993eef2f7f99d9ed8faf364506c/libs/cua-driver/rust/crates/platform-linux/src/recording_hooks.rs#L74-L199
[window-translation]: https://github.com/trycua/cua/blob/ab628e0d1cf1e993eef2f7f99d9ed8faf364506c/libs/cua-driver/rust/crates/platform-linux/src/tools/impl_.rs#L2398-L2430
[window-click]: https://github.com/trycua/cua/blob/ab628e0d1cf1e993eef2f7f99d9ed8faf364506c/libs/cua-driver/rust/crates/platform-linux/src/tools/impl_.rs#L6666-L6743
[plugin-geometry]: https://github.com/trycua/cua/blob/ab628e0d1cf1e993eef2f7f99d9ed8faf364506c/libs/cua-driver/hyprland-plugin/src/input_experiment.cpp#L525-L545
[plugin-input]: https://github.com/trycua/cua/blob/ab628e0d1cf1e993eef2f7f99d9ed8faf364506c/libs/cua-driver/hyprland-plugin/src/input_experiment.cpp#L932-L1047
[painter]: https://github.com/trycua/cua/blob/ab628e0d1cf1e993eef2f7f99d9ed8faf364506c/libs/cua-driver/rust/crates/cursor-overlay/src/render_state.rs#L899-L920
[wayland-overlay]: https://github.com/trycua/cua/blob/ab628e0d1cf1e993eef2f7f99d9ed8faf364506c/libs/cua-driver/rust/crates/platform-linux/src/wayland/overlay.rs#L895-L958
[compatibility]: https://github.com/trycua/cua/blob/ab628e0d1cf1e993eef2f7f99d9ed8faf364506c/libs/cua-driver/rust/crates/platform-linux/src/wayland/hyprland_compatibility.rs#L14-L79
[mac-frame]: https://github.com/trycua/cua/blob/ab628e0d1cf1e993eef2f7f99d9ed8faf364506c/libs/cua-driver/rust/crates/platform-macos/src/tools/px_frame.rs#L63-L126
[win-frame]: https://github.com/trycua/cua/blob/ab628e0d1cf1e993eef2f7f99d9ed8faf364506c/libs/cua-driver/rust/crates/platform-windows/src/virtualdesk.rs#L21-L96
[win-overlay]: https://github.com/trycua/cua/blob/ab628e0d1cf1e993eef2f7f99d9ed8faf364506c/libs/cua-driver/rust/crates/platform-windows/src/overlay.rs#L1258-L1268
[foreground-proof]: https://github.com/trycua/cua/blob/ab628e0d1cf1e993eef2f7f99d9ed8faf364506c/libs/cua-driver/rust/crates/cua-driver-e2e/tests/hyprland_foreground_test.rs#L445-L455
[primary-proof]: https://github.com/trycua/cua/blob/ab628e0d1cf1e993eef2f7f99d9ed8faf364506c/libs/cua-driver/hyprland-plugin/tests/production_active_primary_proof_test.py#L476-L491
[harness]: https://github.com/trycua/cua/blob/ab628e0d1cf1e993eef2f7f99d9ed8faf364506c/libs/cua-driver/docs/test-harnesses-guide.md#the-short-version
[contribution]: https://github.com/trycua/cua/blob/ab628e0d1cf1e993eef2f7f99d9ed8faf364506c/CONTRIBUTING.md#preserve-contributor-authorship
