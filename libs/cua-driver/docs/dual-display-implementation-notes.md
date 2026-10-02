# Aggregate Hyprland desktop implementation notes

Local implementation journal, 2026-10-02 PDT. This is **not native certification**
or a claim that every acceptance blocker is closed.

## Source and scope

Built on upstream `ab628e0d1cf1e993eef2f7f99d9ed8faf364506c`, preserving
the four nonmerge #4305 commits with their original authors, trailers and
`cherry-pick -x` source references. The scroll conflict retains main's
`point_at` focus-refresh dispatch and the contribution's signed origin mapping.
The cursor conflicts retain main's `matches!` style and the shared placement
predicate. Main's floating-point native cursor readback replaces the duplicate
integer readback introduced by the contribution.

No public display selector, protocol expansion, background allowlist change,
monitor/plugin configuration, installation or publication is part of this work.
The separately authored supplemental GTK acceptance tests are integrated intact;
their native row remains explicitly ignored and is not run by implementation.

## Coordinates and qualified topology

- The powered, unmirrored logical output union is the nonnegative desktop image
  frame; frame origin translates once into signed compositor layout coordinates.
- Each native output buffer is decoded under limits, checked against its qualified
  native mode, independently resized to its rounded logical rectangle and placed
  at its frame offset. Two 3840×2160@1.5 outputs at `(0,0)` and `(2560,0)` produce
  **5120×1440**. A 2560×720 encoded image maps `(1920,360)` to `(3840,720)` once.
- `get_screen_size.scale_factor` is compatibility output-DPI metadata (maximum
  powered scale), not an input multiplier. `get_desktop_state.scale_factor` is
  native-capture normalization; an already logical composite reports 1.
- Standby outputs are absent from pixels but remain in absolute-pointer coverage.
  Non-mirror overlap and differently sized mirrors are refused. Rotations and
  reflections have a typed `unsupported_display_transform` refusal.
- Missing/duplicate output keys are refused. Identity is canonicalized by output
  name and includes native dimensions, each scale, transform, origins, optional
  output ID/serial/refresh, disabled/DPMS/mirror state and thereby pointer coverage.
  IPC socket identity plus attested compositor PID/start time bind incarnation.

## Shared owners and fences

The common capture registry stores optional internal `CaptureFrameIdentity`
alongside immutable pixels, affine transform, dimensions and lifetime binding.
Atomic one-shot admission requires exact optional-identity equality: a bound
capture cannot be admitted against unavailable or different metadata. Existing
adapters remain explicitly unbound rather than manufacturing fake topology IDs.

Desktop input keeps its immutable qualified snapshot through overlay waits and
virtual-pointer setup. Admission and final dispatch use that same frame; native
metadata, not a second desktop screenshot, validates dimensions. Motion, button
presses and repeated scroll/click/drag phases have freshness checks. After a
possible effect, a frame failure is reported as partial/unknown, never effect-none
or a reason to replay. Cancellation releases only tracked buttons on the ORIGINAL
owned proxy, frames and boundedly acknowledges that release before destruction.
It does not create a replacement device or move/focus a successor target. Stalled
or lost acknowledgment is explicitly cleanup-unconfirmed/partial, not successful
release. Owner send/reply waits and device roundtrips are bounded.

Coverage is half-open powered-output coverage, not just the rectangular canvas.
Invalid endpoints and actual interpolated drag points are refused before input.
`point_at` keeps the stationary-pointer focus refresh, choosing its approach
neighbor on the same powered output rather than across a seam or gap.

Desktop observations (including uncapped images) retain stale state until a new
successful publication. Cap ratios are isolated by runtime and session generation;
publication cannot resurrect an old generation after retirement. Publication
binding and per-target sequence are pinned BEFORE pixels; reversed completion
refuses the predecessor. Frame identity, cap ratio and exact source ID publish
together. Window screenshots/zoom use CLIENT-local lifetime/dimension identity,
  not aggregate rotation or another output's DPMS; an observed stale context stays
  stale until refreshed.

All eight native desktop-to-window consumers use exact PID/full compositor
address and logical client bounds, with frame-to-layout translation once. They
do not query X11 or truncate an address. Logical plugin surface coordinates are
unchanged. Existing synchronous plugin layout/mode/DPMS revocation and effect-free
retry requirements remain unchanged.

Readback brackets `cursorpos` with frame identity. Recording now owns one immutable
turn context; before/after/marker/cursor evidence refer to that same geometry and
binding. Consumed DESKTOP admission hands off its original binding/affine result
before coordinates are discarded. Client-local admissions do not pretend their
digest is global recording authority; unresolved action-guard reconciliation omits
markers. Per-output `grim` shares the received phase deadline, bounded native-mode
stdout/decode limits and aggregate adapter buffer-budget preflight, with kill/reap
cleanup. Decode/resize/encode check deadlines between phases; image-library CPU
phases are non-preemptible and the budget is not an OS RSS limit.

## Remaining blockers and limitations

1. **Observation is not atomic native dispatch.** Desktop identity now includes a
   same-incarnation bounded socket2 reader generation/event epoch, drained through
   an unpredictable ordered custom-event marker, PLUS the existing loaded
   production v3 plugin lane `(epoch, desktop_generation)` status. Hyprland v0.56.2
   socket2 alone lacks layout/mode/DPMS events: without that production plugin
   authority the desktop returns `topology_epoch_unavailable`. Unseen transitions
   represented by plugin signals now change retained identity even after geometry
   returns. Metadata-query→generic-pointer-send is still non-atomic; socket2 does
   not invent missing events, and this is not synchronous held-device cancellation.
   Monitor/client/plugin-status replies now retain the exact IPC peer PID,
   process start time and socket inode/device from the query that produced them;
   qualification and pointer connection creation reject a successor peer rather
   than rebinding old metadata to a freshly queried compositor. Desktop and local
   client barriers share the same incarnation key. Drag translation uses one
   client observation for both endpoints, with local lifetime checks bracketing
   that translation; this is still not the later plugin grant guard.
2. **Window-local/plugin dispatch guard is not end-to-end.** Native desktop
   translation and screenshot/zoom admission check qualified identity, but those
   guards are not yet carried through every later window-action overlay wait and
   plugin retry. A moved client with the same dimensions also needs pinned surface
   origin/revision for desktop-derived coordinates. This is a readiness blocker.
3. **Recording action handoff is not yet complete.** The separately authored
   immutable turn implementation is integrated and shares the actual composition
   deadline. Desktop consumed admission is wired; unbound/capped/zoom points and
   exact client/root/revision reconciliation still require dispatch-owner wiring.
   Missing/unprovable handoff is classified unavailable, never guessed/retried.
4. Held generic virtual pointers use a 100-ms idle poll, with bounded metadata
   query latency up to its IPC deadline, not synchronous event cancellation.
   Original-device release-before-destroy has owner order/dropped/stalled-peer
   unit coverage, but still needs native primary-seat button-state evidence.
   Dropped command replies clean up on that original owner; unresolved receive
   cancellation remains observed even when response send wins the timeout race.
   The primary-seat owner refuses concurrent generic holds and quarantines after
   unconfirmed cleanup rather than producing replacement pointer effects.
5. macOS/Windows/X11 keep legacy unbound capture metadata. Shared signatures remain
   compatible, but their native builds/desktop qualification were not performed.
   macOS overlay remains main-screen scoped. Fractional artwork fidelity and seam
   artwork clipping remain outside this placement fix.
6. The complete canonical desktop matrix, supplemental native dual-display row,
   qualified-app background isolation and host MCP session evidence remain parent
   responsibilities on a stable candidate, after the blockers above are resolved.

Focused hermetic validation and exact local commit/log paths are recorded in
`/tmp/shuvcode/cua-dual-display-implementation-result.md` by the implementation
worker. That local ledger is not a public release or native passing result.
