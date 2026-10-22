# Migration validation — October 22, 2026 (PDT)

**Not deployment-ready.** Phase 1 preservation is complete. The five focused
Phase 2 fixes are implemented and pass local hermetic/contract validation.
Phase 3 ports and disposable native development checks have since progressed
below. Target-package certification and cutover remain open.

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

## Phase 2 checkpoint evidence (historical, not current certification)

Logs are under `~/.cache/cua-upstream-migration-20261022/evidence/`.

Production source tested:
`564a509071ccfea237facca34573929506b7eed7`. Subsequent
`98d99ac639f0959015bdd20aaf6ef08b456a197a` only synchronizes nine existing
canonical skill texts into the SDK bundle; the final checkpoint adds only
migration documentation. No production Rust changes followed that checkpoint
before Phase 3 resumed.

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

## Phase 3 development evidence (not stable-candidate certification)

The larger ports now include capture topology/source authority, original-resource
held cleanup, immutable recording frames and ordered observation publication.
Current semantic routing retains contributor-owned #5574, with the original
linked Victor Vogel authorship. Private uncertain-source errors share upstream's
`InputMayHaveLanded` type so they cannot bypass token/batch no-replay checks.

- `phase3-held-tests.log`: whole core library 1,113 passed / 1 ignored; whole
  Linux library 792 passed / 10 ignored. These are implementation iterations,
  not the complete post-integration native matrix.
- `phase3-profile-tests-final.log`: 139 packaging/profile/recipe tests passed,
  including schema-3 typed IME extension binding and source/kit-option refusal.
- `arch-plugin-native-launch-build.log`: the production plugin and all 23 CTests
  passed inside a fully updated disposable Arch container with Hyprland
  0.56.2-4 and hyprutils 0.14.3-1. Build used input ON, test input/trace OFF,
  and retained IME guard ON. This working-source smoke is not package certification.
- `phase3-native-hidden-smoke.log` and `...hidden-smoke-early-map.log`: retained
  **failures**. The first Lua adaptation allowed the main window onto workspace
  5; an early move/setprop adaptation also lost primary focus. These tests did
  not pass, and their actions were never replayed to manufacture a pass.
- `phase3-native-hidden-plugin-smoke.log`: the replacement synchronous native
  guard passed the strengthened fixture. Splash, main, token-inheriting child
  and a modal 12 seconds later stayed hidden, even after the launching Driver
  exited. Independent GTK focus-out, ordered compositor focus events and
  monitor workspace snapshots remained unchanged. Only test-owned processes
  were killed; pidfds bound cleanup to their inspected process incarnation.

The disposable parent is rootless Sway on headless outputs with NVIDIA CDI,
not the home compositor. Its xdg-shell global uses wlroots' real v6 support
because Aquamarine unconditionally requests v6 while Sway requests v5. This
parent-only adapter and the startup capability-event workaround are preserved
under `arch/`, alongside exact container/image and package identities. Target
Hyprland, hyprutils and the tested plugin are not protocol-shimmed. This is an
environment limitation/provenance fact, not permission to alter the live host.

Both native fixture binaries are integrated into the canonical Linux native/all
runner on an explicitly prepared disposable dual-output desktop. They are no
longer manual-only inventory exceptions. Stable-candidate execution still needs
the complete required input/capture/recording/cleanup/IME matrix and artifact
hashes, followed by package transaction, restart and rollback qualification.

## Next gate

Finish post-integration drift/lint checks and review the exact source candidate;
then certify its exact built artifacts and complete target environment. Do not
install a development smoke as the default CLI/MCP/plugin pair or relabel the
preserved rollback bytes as a tested environment. Fresh-session cutover and
rollback remain gated; no upstream PR or completion notification is authorized
by a passing development row alone.
