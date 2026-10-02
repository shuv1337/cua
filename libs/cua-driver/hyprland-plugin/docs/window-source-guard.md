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

Desktop-derived stateful native holds and coordinate-bearing semantic pointer
fallbacks currently return that refusal: those adapters cannot apply a compositor root grant. They
must not silently focus, synthesize a semantic substitute, or use current geometry
to reinterpret an old point. Ordinary window-local input and coordinate-less
release of an original owned device remain available. A generic desktop pointer
has topology authority, not a window root grant. Userspace observation followed
by generic native input is not compositor-atomic.

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

## Remaining stateful phase seam

`source_guard_version:1` is a TARGET observation extension; protocol v3 still
offers only finite CLICK/KEY/SCROLL/DRAG/ACTIVATE one-shot grants. There is no
held-operation handle or move phase which applies a retained root to the original
device. `persistent_vptr` owns a separate wlroots device; that protocol cannot
consume the plugin grant. Its foreign-toplevel activation lookup is not an exact
compositor-root capability. A read-only SOURCE_GUARD query alone would not fix
that delivery/activation seam and must not be advertised as such.

Minimal private extension for full stateful parity: negotiate a held-phase
capability; BEGIN validates the original root nonce/revision/fractional geometry,
desktop generation, owner and delivery policy before focus/press, and returns an
opaque held-operation handle for that same lane/device. MOVE carries that handle
and original source witness, reconciles under the compositor phase handler before
motion and never binds a successor root. RELEASE/abort remain coordinate-free,
release only owned buttons on the original device after invalidation, and preserve
bounded acknowledged/partial/unknown outcomes. Disconnect/Stop/root changes and
idle expiry must revoke that same handle/device; no reconnect or replay. Driver
session lifetime, queued cancellation and cleanup quarantine must carry the handle
through every phase. Existing background package qualifications remain unchanged;
background holds must not silently become foreground injection. This extension
is not implemented or negotiated in this candidate; this is an implementation
gap, not a claim that stateful input is intrinsically impossible.

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
