# Larger candidate reconciliation — incomplete, not qualification

Comparison inputs are the immutable commits in [the ledger](README.md).
The final candidate's `hyprland-plugin/docs/window-source-guard.md`, not its
early implementation journal, defines the private guard/held-phase behavior.
The current release already owns useful safeguards; absence of a private
extension is not evidence that all upstream input safety is absent.

## Desktop capture and stale sources

Release owners: Linux `wayland/hyprland.rs` (`DesktopSnapshot`,
`DesktopCapturePlan`, `desktop_snapshot`, `desktop_capture_plan`),
`wayland/hyprland_capture.rs`, `wayland/mod.rs`, and the shared capture admission
boundary in core `tool.rs` / capture-source state.

Retain upstream logical composition and fractional-scale normalization. Existing
tests include `screen_report_describes_the_same_snapshot_as_its_frame` and
`capture_plan_composes_every_layout_with_more_than_one_monitor`. Those establish
frame construction, not continuity of an old capture ID through same-sized
topology changes. #4387's recorded gap explicitly distinguishes these contracts.

The private source binds the original compositor/connection epoch, complete
topology generation, root/client nonce, full PID/address, fractional geometry and
revision. Reconcile publication, admission, waits and each dispatch phase as one
contract; a later fresh observation is not proof of the original capture.
Visual-only read-only observation must not manufacture action authority.

## Finite native input versus cross-call held input

Release owner: Linux `wayland/hyprland_input.rs`, plugin
`src/input_experiment.cpp` and shared cancellation/progress types. Existing
`DispatchUnknown` / `unknown_dispatch` preserve unknown effects. Keep the strong
current tests: `foreground_partial_unknown_keeps_only_proven_acknowledgements`,
`uncertain_io_after_reuse_is_an_error_without_replay`, and
`aborted_acknowledged_drag_disconnects_only_its_own_lane_without_replay`.
Do not replace these with old candidate implementations solely to get matching
file layouts.

Neither release owner negotiates `source_guard_version:1` or
`held_phase_version:1`. Therefore the release's finite-input/drag contracts do
not establish the candidate's cross-call HOLD contract. The private extension
requires original-connection, coordinate-free release/abort, irreversible
retirement, route-separated debt and quarantine without successor TARGET
acquisition. Missing extension must be `window_source_guard_unavailable` on a
retained guarded route, not a fallback into generic pointer input.

The private logger/journal confirms only server-queued button/frame dispatch on
the original resource incarnation. It does not prove client receipt, toolkit
processing or application effect. Focus loss, root destruction and DnD still
need independent native postconditions. Preserve debt after unconfirmed cleanup.

## Semantic mutations

Release owners: Linux `atspi/native.rs`, snapshot/token resolution and
`tools/impl_.rs`; core `batch_tools.rs` owns foreground retry at batch level.
#5574 actively owns uncertain AT-SPI outcomes and batch no-replay. Preserve its
contributor history if integrating it rather than implementing a competing copy.

The private source separately binds an immutable AT-SPI unique owner/node/frame
and bounded live ancestry. Plain point-free activation is not a compositor root
grant and must not acquire TARGET merely to obtain recording evidence. An
uncertain setter cannot become an ordinal rewalk or insert/value fallback.
Even the private source documents that same-owner object-path reuse may require
an additional toolkit lifetime epoch; do not label that limitation solved.

## Recording and launch/fixture ownership

Core recording already owns retained supplemental click captures and failure
classifications. Linux recording hooks map evidence coordinates. Keep those
owners while pairing before/after frames, action/cursor markers and native
capture provenance to one immutable turn geometry. The focused unique-window
fix does not establish that larger contract.

Hidden-workspace launch remains required: port its splash/child/late-modal
lifetime guard into current Hyprland launch/plugin owners, without stealing the
human workspace. Native dual-display fixtures belong in `cua-driver-e2e` and
canonical runners, not as a parallel unselected acceptance script. Neither this
mapping nor the passing hermetic suites qualifies these still-unported paths.
