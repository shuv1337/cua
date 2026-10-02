# Private window-source guard

Production protocol v3 optionally negotiates `source_guard_version: 1` in
HELLO and TARGET. This is an internal capability, not a public display selector.
TARGET supplies `source_identity`, six-number `source_geometry`, and
`desktop_generation`. The source identity is a nonce for the exact root/client
connection; it survives one-shot grants, but invalidation clears it. Geometry
contains the original fractional compositor witness, not rounded screenshot
coordinates. The driver retains the original epoch, connection path, PID/full
address, source nonce, revision and geometry before translating any point.

Every action phase reconciles a fresh one-shot grant against that immutable
source. Changed authority refuses before dispatch. Guarded stale-geometry replies
are not retried; possible effects are never replayed. Missing capability returns
`window_source_guard_unavailable`. Installing or hotloading a module is not part
of capability negotiation.

Coordinate-bearing semantic pointer fallbacks cannot substitute for a guarded
native phase. A generic desktop pointer has topology authority, not a window
root grant. Userspace observation followed by generic native input is not
compositor-atomic.

`held_phase_version: 1` negotiates cross-call held-button phases. `HOLD_BEGIN`
consumes the reconciled capability-8 grant and returns a connection-owned opaque
handle. `HOLD_MOVE` carries the original root/client nonce, revision, fractional
geometry and desktop generation; it never reacquires TARGET. `HOLD_RELEASE`
is coordinate-free and remains available after invalidation for only the
original resource set. The original five-second grant lease is not renewed.
Source loss retires the handle irreversibly. Missing or malformed ACKs produce
partial/unknown outcomes and quarantine the original connection; there is no
reconnect, successor-resource cleanup, or replay. Cleanup confirmation means
protocol delivery acknowledgement, not application processing proof.

Coordinate-free plain element activation and `set_value` do not translate
desktop pixels. They use the token's immutable AT-SPI unique owner/node/frame,
verify the frame owner PID and bounded live ancestry, and retain that exact object
for mutation/read-back. They neither acquire a plugin TARGET for recording nor
accept a coordinate capture ID as semantic permission. Observed setters do not
retry an ordinal or insert/value fallback after an uncertain write. These checks
are not a compositor root grant, nor a toolkit-atomic check/mutation transaction.
Exporters that reuse an object path within the same unique bus owner need an
additional toolkit lifetime epoch to prove absence of unseen destroy/recreate.

Read-only desktop observation remains available without production plugin event
authority, using an explicitly limited socket2/metadata identity. That cannot
qualify native capture publication. Only operation-proven native captures with
complete topology authority receive action capture IDs/cap observations. Opaque
fallbacks remain visual-only and do not install a new coordinate source; a current
visual-only refresh marks a prior implicit observation stale without invalidating
a newer publication's sequence. Socket2's informational vocabulary is pinned to
Hyprland 0.56.2: screencast/screencastv2, layers, bells and custom markers do not
revoke topology. Monitor/config events and unknown future names still revoke;
complete layout/mode/DPMS authority requires the plugin generation.

## Stateful capability and cleanup limits

The implemented private `held_phase_version:1` extension is input authority,
not a read-only SOURCE_GUARD passed to a generic wlroots device. Its lease lasts
five seconds from the original grant without renewal. Holds are intra-client
(selection, canvas, sliders); titlebar/resize motion changes the retained witness
and retires the hold. Ordinary local `persistent_vptr` and semantic availability
remain independent of optional recording and this private grant.

`HOLD_ABORT sequence original_root_nonce` permits exact original-connection
cleanup after a lost/malformed BEGIN handle ACK. It does not acquire a handle,
target or device. Independent release uses the captured pointer proxies even
after root destruction, before leave/destruction. An uncertain BEGIN/MOVE ACK
attempts bounded coordinate-free cleanup on that original socket, then closes
or quarantines it; EOF alone is never proof of release. Cleanup lock timeout
publishes retirement first, and the effect owner services that release obligation
when it exits. A verified original-handle/sequence retired reply can retain
confirmed aborted completion; other retirement never implies confirmation.

Hyprland 0.56.2's native `CWLPointerResource::sendLeave` releases pressed buttons
on the old surface before changing focus, except during DnD. The plugin never
sends a foreground release to successor focus. Without an observed original
resource release ACK, a focus/root-loss outcome remains explicitly unconfirmed;
the native void `sendButton` also cannot confirm ordinary foreground RELEASE
when capabilities or the native pressed set disappeared. This candidate retains
an unconfirmed cleanup quarantine even after a same-focus release attempt;
independent protocol resources provide a direct original-resource queue fact.
it does not infer confirmation from a disappeared surface. Active DnD is rejected
by foreground admission, and DnD beginning during a hold is a cancellation with
potentially unconfirmed delivery. Native focus/close/DnD qualification remains
required; this source description is not that evidence.

Any module implementing it needs a newly built artifact and disposable matching-
version compositor qualification before home distribution. Existing trusted
profile hashes do not trust a new SO merely because its ABI version matches.
No buildkit/profile pins are updated by these source changes; canonical packaging
and qualification remain the distribution authority.

Socket2 authority observations emit `custom>>cua-authority-*` events visible to
all subscribers. A held-pointer idle observation uses two bracketing markers
per 500 ms poll (at most four markers/s for one continuously idle held target,
excluding command validation); slow observations reduce this rate. An IPC
failure revokes the original hold. Cleanup releases only owned buttons on the
original connection before destruction; failed acknowledgment remains
`cleanup_unconfirmed` and quarantines new holds.

Qualified recording pixels require in-process screencopy with the actual
connection fd verified against the original compositor and exact protocol output
name/native mode. Opaque grim/portal/X11/legacy byte providers remain unqualified.
Recording evidence and capture-ID associations never authorize input.
