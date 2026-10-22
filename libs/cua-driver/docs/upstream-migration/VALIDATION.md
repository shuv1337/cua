# Migration checkpoint — October 22, 2026 (PDT)

**Not deployment-ready.** Phase 1 preservation is complete. The five focused
Phase 2 fixes are implemented and pass local hermetic/contract validation.
Phase 3 reconciliation, target-package qualification and cutover remain open.

## Implemented behavior

- L8/La8 PNG resizing preserves color format, alpha, dimensions and pixels;
  RGB/RGBA remain covered by the same owner test.
- Recording argv preserves `--video`, `--no-zoom` and `--scale` on either side
  of paths. Invalid/missing values, unknown options and extra paths fail before
  daemon or filesystem access. Help and generated CLI reference are updated.
- A recording owns a reference-counted idle protection for its exact private
  session key. Replacing/stopping/dropping the recorder releases protection.
  Acquisition cancels only a racing deferred idle end, not explicit teardown.
  The ownerless backstop checks ownership under the recorder mutex. Finalized
  trajectories identify `manual`, `session_end` or `idle_ttl` termination.
- Unix daemon connection establishment retries only transient failures for
  at most 1.5 seconds. Linux uses nonblocking AF_UNIX connection attempts so a
  full listen backlog cannot hang or masquerade as an established connection.
  Missing sockets fail promptly. No request is re-sent after dispatch; the
  upstream daemon-required CLI behavior replaces the old fallback guard.
- Linux recording window resolution requires an exact stable ID or one unique
  PID-owned window. Ambiguous PID-only targets and exact foreign-toplevel IDs
  from another connection produce unavailable evidence, never first-window
  capture. Full-output Wayland capture is still explicitly full-output.

## Evidence collected

Logs are under `~/.cache/cua-upstream-migration-20261022/evidence/`.

Production source tested:
`564a509071ccfea237facca34573929506b7eed7`. Subsequent
`98d99ac639f0959015bdd20aaf6ef08b456a197a` only synchronizes nine existing
canonical skill texts into the SDK bundle; the final checkpoint adds only
migration documentation. No production Rust changes follow the tested source.

- `full-hermetic-tests.log`: complete core, SDK and CLI crates passed, including
  their default integration tests and doctests; ignored desktop tests remain
  unverified. This was rerun after restoring all five mutations.
- `core-final-tests.log`: whole core library, 1,087 passed, 1 ignored.
- `sdk-final-tests.log`: whole SDK library, 103 passed.
- `cli-unit-tests.log`: whole CLI binary unit suite, 347 passed.
- `cli-final-tests.log`: recording CLI process tests (3), command help (1),
  protocol/schema tests (4), all passed. A private fake daemon observes the
  actual CLI's directory/video arguments; no video capture is started.
- `recording-window-tests.log`: all 4 Linux recording-hook owner tests passed.
- `linux-hermetic-tests.log`: the whole Linux crate passed with inherited GUI
  session selectors removed: 712 unit tests and 8 integration tests passed;
  30 desktop-dependent tests remained ignored. No test was excluded by name.
- Native AF_UNIX backlog recovery, fragmented replies, missing socket refusal,
  and no reconnect/replay after a lost reply are exercised in the core suite.
- CLI reference was generated from the built binary using the repository's
  `scripts/docs-generators/cua-driver.ts`, not hand-edited snapshots.
- `docs-check.log`: generated CLI/MCP reference drift check passed.
- `clippy.log`: affected crates' `--all-targets` Clippy passed with warnings
  in unchanged files. Cargo formatting and `git diff --check` passed.
- `preservation-check.json`: original checkout diff/status, installed artifact
  hashes, resolved executable links and rollback copies all remain unchanged.

`mutation-results.json` records five caught mutations: removal of grayscale
support, recording-owner protection, transient connect retry, correct scale
argument consumption, and unique PID-window selection. Each corresponding
owner test failed by assertion (not compilation failure). All mutations were
restored before the final verification runs. A filtered owner test is
development evidence, not a substitute for a whole-crate or desktop matrix.

## Baseline limitations retained, not hidden

The unchanged Linux overlay test
`overlay::tests::unreported_arrival_releases_the_waiter_after_the_cap` fails at
`started.elapsed() >= ARRIVAL_WAIT_CAP`. It failed in parallel and serial
whole-crate runs here (711 passed, 1 failed, 10 ignored), and in a separate
untouched `85e2b1318` worktree (710 passed, 1 failed, 10 ignored).
See `library-tests.log`, `library-tests-serial.log` and
`stock-platform-linux-tests.log`. This test assumes an X11 command queue, but
the inherited Wayland session selectors route away from it before the wait.
Serial execution alone does not resolve it. The complete crate passes in a
headless environment without changing or weakening the test:

```bash
env -u DISPLAY -u WAYLAND_DISPLAY -u HYPRLAND_INSTANCE_SIGNATURE \
  -u XDG_SESSION_TYPE -u DBUS_SESSION_BUS_ADDRESS \
  cargo test --locked -p platform-linux -- --test-threads=1
```

The test's dependence on inherited session state remains a workflow papercut
(`pc_8dc82db94527`); the failed runs remain available alongside the passing run.

`libs/cua/scripts/sync-skills.sh --check` also fails on the untouched release
tag: nine other vendored skill files already differ from their canonical
sources. This was repaired mechanically with the canonical sync script in
`98d99ac63`; the complete drift gate now passes. No edits were made to the
original checkout. See `stock-skill-drift.log`; papercut `pc_19a6904e1c3d` is
resolved against that commit and verified gate.

No hosted CI, Nix certification, macOS/Windows native run, plugin CTest/ALPM
lifecycle run, complete desktop matrix, hyprutils 0.14.3 qualification, fresh
session cutover or full-environment rollback has been claimed. Linux timing
tests and source-level coverage do not qualify native input or cleanup.

## Next gate

Resolve/account for the source-baseline limitations above, then reconcile the
active upstream ownership listed in [the patch ledger](README.md). Required
private capture/source/held-input invariants, immutable recording turn geometry,
hidden-workspace launch lifetime guards and native fixtures are not yet ported.
Do not install this branch as a replacement for the matching private
Driver/plugin pair. Preserve the installed rollback set until the complete
target environment has passed the plan's qualification and rollback gates.
