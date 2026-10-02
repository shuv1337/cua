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
sends a foreground release to successor focus. Without original-resource event
proof, a focus/root-loss outcome remains explicitly unconfirmed; it does not
infer confirmation from a disappeared surface. Active DnD is rejected
by foreground admission, and DnD beginning during a hold is a cancellation with
potentially unconfirmed delivery. Native focus/close/DnD qualification remains
required; this source description is not that evidence.

### Public native-event queue evidence

The lane registers a public `wl_display_add_protocol_logger` once at start and
unregisters it before journal/resource state destruction or plugin retirement.
Registration absence advertises `held_foreground:false` and refuses held press;
finite foreground and independent input do not require this observer. Foreground
held pointers must be version 5+ and their bounded ledger is allocated before
press. The synchronous original-owner press send stack binds each actual
`wl_pointer.button(pressed)` resource. No `fromResource`, wrapper userdata or
private-member casts are used. A destroy listener pins that exact incarnation;
resource-address reuse cannot contribute to its journal. Callbacks record only
owned-button/leave/frame booleans, perform no input or allocation, and never
retain arbitrary protocol payloads, keyboard events or titles.

In qualified Wayland 1.26.0 `handle_array`, the logger is AFTER object verification
and closure marshalling but BEFORE `wl_closure_send`. A logged event alone is
only `marshalled`, not queued. A send failure sets the client error bit; every
subsequent event is then suppressed. Therefore a subsequent event on that same
resource proves the previous send returned successfully. BEGIN and cleanup use
two empty frame sends: observation of the first proves button queue success,
observation of the second proves the first frame was queued. Failure to marshal
or queue the release or its first frame never becomes confirmed cleanup. A
failure queueing the second sentinel does not undo the already proven release
and first-frame queue fact; it can still lead to client destruction. Confirmation
means **server queued dispatch**, not socket flush, client receipt, toolkit
processing, application effect, or continued client health.

Native automatic release-before-leave is observed on the original resource;
cleanup sends only empty frame sentinels after such a release, never a duplicate
button release into successor focus. Silent native refusal has no matching event.
DnD leave without a release stays unconfirmed (`dnd_release_withheld`). Missing
original resources stay unconfirmed unless release/frame queue proof was already
recorded before destruction. Later original events may upgrade the cleanup fact
of an already retired phase, but never reopen input admission or renew its lease.

Foreground debt retains only the original journal/resource lifetimes, not a dead
client's input authority. It blocks only foreground reuse in that lane; independent
resource debt is separate and cannot be cleared by foreground completion (or vice
versa). Every actually observed original press must have release/frame proof
before foreground debt is cleared. Real libwayland resource/logger fixtures,
extracted native seat methods, and injected failures through the exact qualified
`handle_array` body exercise this distinction; they are not compositor/application
native delivery qualification.

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
