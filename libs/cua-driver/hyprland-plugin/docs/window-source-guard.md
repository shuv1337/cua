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

Desktop-derived stateful native holds and AT-SPI/element-token actions currently
return that refusal: those adapters cannot apply a compositor root grant. They
must not silently focus, synthesize a semantic substitute, or use current geometry
to reinterpret an old point. Ordinary window-local input and coordinate-less
release of an original owned device remain available. A generic desktop pointer
has topology authority, not a window root grant. Userspace observation followed
by generic native input is not compositor-atomic.

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
