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
or a reason to replay. Cancellation destroys/flushed the owned synthetic proxy;
it does not create a replacement device to release into a successor compositor.

Coverage is half-open powered-output coverage, not just the rectangular canvas.
Invalid endpoints and actual interpolated drag points are refused before input.
`point_at` keeps the stationary-pointer focus refresh, choosing its approach
neighbor on the same powered output rather than across a seam or gap.

Desktop observations (including uncapped images) retain stale state until a new
successful publication. Cap ratios are isolated by runtime and session generation;
publication cannot resurrect an old generation after retirement. Window screenshot
and zoom contexts likewise retain a stale frame after an observed mismatch.

All eight native desktop-to-window consumers use exact PID/full compositor
address and logical client bounds, with frame-to-layout translation once. They
do not query X11 or truncate an address. Logical plugin surface coordinates are
unchanged. Existing synchronous plugin layout/mode/DPMS revocation and effect-free
retry requirements remain unchanged.

Readback brackets `cursorpos` with frame identity. Recording captures are logical
even on a single scaled output, and markers subtract signed frame origin and
validate coverage. Per-output `grim` subprocesses have a shared 10-second deadline,
bounded stdout and predecode allocation/dimension limits, with kill/reap cleanup.

## Remaining blockers and limitations

1. **Polling is not an event epoch or atomic compositor transaction.** An unseen
   A→B→A transition between observations is not detected. There remains a
   metadata-query→native-send race. Generic desktop authority does not yet inherit
   the plugin's stronger synchronous event boundary. Do not claim ABA revocation.
2. **Window-local/plugin dispatch guard is not end-to-end.** Native desktop
   translation and screenshot/zoom admission check qualified identity, but those
   guards are not yet carried through every later window-action overlay wait and
   plugin retry. A moved client with the same dimensions also needs pinned surface
   origin/revision for desktop-derived coordinates. This is a readiness blocker.
3. **Recording hooks do not share one immutable per-turn capture object.** Each
   screenshot and marker is individually fenced and normalized, but topology can
   differ between their separate calls. Capture-ID markers are deliberately omitted
   because this legacy hook lacks the consumed affine transform. Full recording
   frame/marker binding remains a readiness blocker.
4. Held generic virtual pointers use a 100-ms idle poll, with bounded metadata
   query latency up to its IPC deadline, not synchronous event cancellation.
   Destruction/flush on the original device still needs native cleanup evidence.
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
