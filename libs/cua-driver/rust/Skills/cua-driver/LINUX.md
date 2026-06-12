---
name: cua-driver-rs-linux
description: Drive a native Linux app (X11 / Wayland) via the cua-driver CLI — snapshot the AT-SPI tree, click by element_index or pixel, verify via re-snapshot. Linux backend is BETA in cua-driver-rs and the no-foreground contract has open issues (see this doc).
---

# cua-driver-rs — Linux

**Status: BETA.** The Linux backend in cua-driver-rs covers the core
tool surface (click, type_text, scroll, hotkey, screenshot,
launch_app, list_apps, list_windows, get_window_state) but several
behaviors that the macOS / Windows skills consider table-stakes are
**not yet implemented or only partially supported**:

- **No-foreground contract**: limited. X11's input model has no clean
  per-pid event routing equivalent of macOS `CGEventPostToPSN` or
  Windows `PostMessage(WM_LBUTTONDOWN)`. `XTestFakeKeyEvent` /
  `XTestFakeButtonEvent` synthesize input but route to the focused
  window — similar focus-stealing behavior to Windows `SendInput`,
  which the Windows backend avoids by using `PostMessage` instead.
  Linux has no equivalent per-window-message channel that bypasses
  focus, which is why XTest's focus-stealing is the binding
  limitation here. AT-SPI `accDoDefaultAction` works for accessible
  elements but requires the user's accessibility bus to be running,
  which is not the default on every distro.
- **Wayland support**: depends on compositor. Under GNOME-Mutter and
  KDE-KWin with `org.freedesktop.portal.RemoteDesktop` enabled, some
  click and key paths work. Under most other compositors, input
  synthesis is denied by the security model and the tool surface
  degrades to "passive" (snapshot, screenshot) only. Hyprland is the
  exception: it is fully supported for background element-index
  workflows — see the Hyprland section below.
- **UIA / AX-tree equivalent**: AT-SPI when available, otherwise
  empty. Many GTK4 / Qt6 apps populate AT-SPI lazily; agents should
  expect partial trees and re-snapshot.
- **launch_app**: true background launch on Hyprland — the app is
  dispatched onto the hidden `special:cua` workspace with no focus or
  workspace change, and the child env gets the AT-SPI bridge forced
  (`GTK_MODULES=gail:atk-bridge`, `NO_AT_BRIDGE=0`, Qt a11y vars) plus
  an XWayland preference (`GDK_BACKEND=x11`, `QT_QPA_PLATFORM=xcb`) so
  the first `get_window_state` returns a populated tree. Known gap:
  windows the app maps LATER (modal dialogs) can still land on the
  user's active workspace (shuv1337/cua#15).
- **Recording**: supported but **opt-in** — the daemon must be started
  with `--allow-recording` / `CUA_RECORDING_ENABLED=1`
  (+`CUA_RECORDING_CAPTURE_AX=1` for `app_state.json`). Per-turn
  screenshots + video; wlr-screencopy on Wayland, `x11grab` on X11;
  ffmpeg on PATH.

See `SKILL.md` (macOS) and `WINDOWS.md` (Windows) for the full
patterns. On **Hyprland** the backend is first-class for background
element_index workflows (launch → snapshot → act → verify, all
invisible to the user); on other Wayland compositors prefer read-only
inspection (screenshot, list_windows, get_window_state).

## Quick triage

If you're agent-driving on Linux and a tool call surprises you:

1. Run `cua-driver doctor` — reports display server (X11 / Wayland),
   AT-SPI bus reachability, XTest availability.
2. Check `XDG_SESSION_TYPE` — `wayland` means most input synthesis
   is gated by portals; `x11` means XTest works but routes via focus.
3. If Wayland: confirm `org.freedesktop.portal.RemoteDesktop` is
   present (`gdbus introspect --session --dest
   org.freedesktop.portal.Desktop --object-path
   /org/freedesktop/portal/desktop`). Without it, input synthesis
   is denied.

## Hyprland

Hyprland is the exception to the "passive-only under Wayland" rule —
background element-index workflows are fully supported. What's
specific to it:

- **Window ids**: `window_id` values come from hyprctl window
  addresses and exceed `u32::MAX`. That's expected — pass them
  through verbatim, don't truncate.
- **Per-window screenshots**: captured via the
  `hyprland-toplevel-export-v1` protocol — true surface capture, so
  the screenshot shows the correct content even for occluded /
  background windows and windows on other workspaces. This is what
  makes background computer use verifiable on Hyprland. grim
  region-crop is the fallback when the protocol is unavailable.
- **Input**: native-Wayland windows accept `element_index` actions
  (AT-SPI) but not pixel input.
- **launch_app = background launch**: the command is dispatched via
  `hyprctl dispatch exec "[workspace special:cua silent] env … <cmd>"`
  (modern ≥0.55 Lua grammar `hl.dsp.exec_cmd('…')` with legacy
  fallback), so the window maps onto the hidden `special:cua`
  workspace. Hyprland forks the child, so the result's pid/window come
  from a client-list diff (≤10 s poll). A placement guard re-sweeps
  the pid's windows for ~20 s because splash screens consume the exec
  rule (the rule only covers the FIRST window the pid maps —
  PrusaSlicer's main frame arrives ~6 s after its splash). Caveat:
  dialogs mapped after the guard window can land on the user's active
  workspace (shuv1337/cua#15).
- **list_windows** reports `workspace_id` (negative = special
  workspaces) and `on_current_space` — use these to find the hidden
  window and to detect placement leaks.
- **Sweeping windows back by hand**: `hl.dsp.window.move` ALWAYS
  reveals the target special workspace (its `silent` key is accepted
  but ignored) — the driver uses an atomic move-then-
  `workspace.toggle_special('<name>')` Lua payload so no frame renders
  with the overlay visible. If you script hyprctl directly, do the
  same or you will pop the hidden workspace over the user's session.
- **launch_app focus guard**: if a newly launched window steals focus,
  the driver restores the previously active window. Best-effort,
  watches for ~2 s after launch.
- **Recording**: opt-in — `start_recording` errors unless the daemon
  was started with `--allow-recording` (or `CUA_RECORDING_ENABLED=1`);
  the per-turn AT-SPI `app_state.json` dump additionally requires
  `CUA_RECORDING_CAPTURE_AX=1`. Video captures the focused monitor via
  wlr-screencopy frames piped to ffmpeg. `cursor.jsonl` sampling
  works via the Hyprland IPC `cursorpos` query; samples are stored as
  physical pixels local to the recorded (focused-at-start) monitor so
  they line up with the video frame — samples while the cursor is on
  another monitor are dropped (counted in `session.json` as
  `cursor.dropped_offscreen`, so a low average sample rate is
  self-explaining), and the file is empty on other Linux sessions.
- **Permission caveat**: if `ecosystem:enforce_permissions` is
  enabled in the Hyprland config and screencopy is denied, captures
  silently return black "permission denied" frames — no error is
  raised. Add an allow rule for the cua-driver binary to the
  Hyprland permission config.

## Pixel click reliability (XSendEvent)

Pixel clicks are synthesized as `ButtonPress`/`ButtonRelease` via
`XSendEvent` directly to the target window — no focus steal, no
pointer move, but also **no delivery guarantee**: the X server
accepting the events says nothing about the toolkit processing them,
and toolkits see `send_event=true` and may drop such events entirely.
The tool therefore reports pixel clicks as "dispatched … delivery
UNVERIFIED" — treat the post-action `get_window_state` diff as the
only success signal. When the cached AT-SPI tree for the target
window is empty, the result carries an explicit warning: an empty
tree usually means the toolkit never registered on the accessibility
bus, which correlates strongly with dropping synthetic events.

| Toolkit / surface | Synthetic XSendEvent click |
|---|---|
| wxWidgets (e.g. PrusaSlicer 2.9.5) — main frames AND modal dialogs | ❌ silently ignored (confirmed 2026-06-10) — use element_index, or the headless-X backend (below) |
| GTK3 main frames | ❌ frequently ignored |
| xterm & friends with `allowSendEvents: false` (the default) | ❌ ignored by design |
| Native-Wayland surfaces | ❌ rejected (no X window to address) |
| GTK4 / Qt5 / Qt6 / Chromium & Electron (XWayland) | ⚠️ typically processed — still verify via re-snapshot |

Recovery when a pixel click no-ops **and the app never had a
populated tree**: relaunch via `launch_app` (it forces the atk-bridge
into the child environment) and drive it with `element_index`
actions. But if the app's tree WAS populated and suddenly collapsed,
do NOT relaunch — see "Modal dialogs collapse the AT-SPI tree" below;
relaunching throws away the app's in-memory state for nothing.

Known coordinate hazard: the pixel path's resize-ratio correction is
keyed per-pid, so after the main window has been resized, clicks
aimed at a *second* window (a dialog) get scaled by the wrong factor
and miss entirely (shuv1337/cua#16). Check the coords echoed in the
result message against what you passed.

## Modal dialogs (wx) collapse the AT-SPI tree

Confirmed 2026-06-10 against PrusaSlicer 2.9.5's "Send G-Code to
printer host" modal: opening a wx modal dialog **drops the entire
application off the accessibility bus** — not just the dialog. The
previously 203-element main window and the dialog both return
1-element trees, and an external pyatspi probe shows the pid absent
from `org.a11y.Bus` entirely.

The driver detects this (shuv1337/cua#17): it remembers the most
elements it ever saw for the pid, and a near-empty snapshot from a
previously-populated app is reported as a likely modal collapse
(`atspi_tree_collapsed: true` + the peak count in `get_window_state`,
and a matching warning on unverified XSendEvent clicks). **Do not
relaunch the app when you see that warning** — the accessibility
bridge is fine and a relaunch destroys in-memory state (a finished
slice, an open document). Recovery: close or complete the dialog by
other means — keyboard commit, the app's own API (rung 5 below), or
cancel it — and the tree returns when the modal closes. The
"relaunch via launch_app" advice now only appears for pids that
never produced a populated tree (a real launch-env problem).

Every no-foreground input path **on the user's shared session**
dead-ends on such a dialog (all verified):

- XSendEvent mouse + keyboard → silently ignored (wx filters
  `send_event=true`)
- XTest mouse (xdotool) → lands on the wrong surface (XWayland's
  coordinate space disagrees with Hyprland's scaled multi-monitor
  layout)
- XTest keyboard with confirmed X focus → never reaches the real
  Wayland input focus
- Real kernel input (ydotool/uinput) → routes correctly ONLY when the
  window is visible and Hyprland agrees on focus; a modal orphaned
  from its parent (split across workspaces) reports
  `activewindow: None` and swallows even genuine clicks

The escape hatch is to stop fighting the shared session entirely:
run the app in a private headless X server where XTest input IS
genuine focused-window input — see "Headless-X backend" below.

**The escalation ladder** (stop at the first rung that works):

1. `element_index` (AT-SPI `doAction`) — background-safe, the default
2. Keyboard commit (`press_key` / `set_value`) — background-safe
3. Pixel `click` via XSendEvent — verify by re-snapshot diff, expect
   failure on wx/GTK3
4. **Headless-X backend** — restart the daemon with `--headless-x`
   and drive the app in a private off-screen Xvfb via XTest (real
   input wx/GTK3 accept, zero focus disturbance to the user — see
   the section below). Costs a fresh app instance, so prefer it when
   you can anticipate stubborn modals BEFORE building up in-app state
5. **The app's own API or config** — often the cleanest exit. The
   dialog above only existed to PUT a file to PrusaLink; the stored
   credentials in `~/.config/PrusaSlicer/physical_printer/*.ini` plus
   one `curl -X PUT -H "X-Api-Key: …" -H "Print-After-Upload: ?1"
   --data-binary @file http://<host>/api/v1/files/usb/<name>` did the
   whole job. When a dialog's only purpose is a network call, check
   whether you can make that call directly.

## Headless-X backend (`--headless-x`)

`cua-driver serve --headless-x[=WxH]` (default 1920x1080) starts the
daemon against a **private off-screen Xvfb** instead of the user's
session. This is the opt-in answer to apps that defeat both AT-SPI
(wx modals collapse the tree) and XSendEvent (wx/GTK3 drop
`send_event=true`): inside the private server, input is synthesized
via **XTest** (`send_event=false` — the exact input wx accepts), and
the target window IS that server's focused window, so the usual
focus-steal objection to XTest evaporates. Proven live 2026-06-11:
PrusaSlicer's wx Configuration Wizard advanced on an XTest click that
every shared-session path silently dropped.

How it works:

- On startup the daemon spawns `Xvfb` on a free display (`:70..:199`)
  plus `openbox` (best-effort, for wx modal focus/stacking), repoints
  its own `$DISPLAY` at it, and clears `HYPRLAND_INSTANCE_SIGNATURE` /
  `WAYLAND_DISPLAY` — so enumeration, capture, and `launch_app` all
  transparently target the headless server via the plain X11 paths.
  Requires `Xvfb` installed (`pacman -S xorg-server-xvfb`); `openbox`
  recommended.
- `launch_app` spawns apps off-screen with software GL forced
  (`LIBGL_ALWAYS_SOFTWARE=1`, llvmpipe) — GL apps (PrusaSlicer) work
  but render on CPU. `GDK_BACKEND=x11` is forced too.
- `click` / `type_text` / `press_key` / `drag` route via XTest; tool
  results say "via XTest (headless-X) — real input delivered" instead
  of the unverified-XSendEvent caveat. Still verify by re-snapshot.
- Per-window screenshots are captured by cropping the private root
  (a direct per-window `XGetImage` is black for llvmpipe GL windows).
- No agent-cursor overlay in this mode (nobody is watching, and it
  would black out root captures on the uncomposited Xvfb).
- Xvfb + openbox are killed with the daemon (`PR_SET_PDEATHSIG`) —
  no leaked servers even on SIGKILL.

Caveats:

- **Whole-daemon mode**: the daemon is either headless or
  user-session, not both. Use a separate daemon (distinct `--socket`)
  if you need to drive the real session at the same time.
- **Shared app config**: the headless app instance reads/writes the
  same `~/.config` as the user's real instance — don't run both
  concurrently for config-heavy apps (per-app `--datadir` isolation
  is a known follow-up on shuv1337/cua#18).
- X11-only: the app must be able to run under plain X11.

## Verify with closed-loop signals, never tool return values

Every silent failure in the 2026-06 dogfood runs was caught by a
read-back, never by a tool result:

- **Did the click land?** Screenshot before/after +
  `PIL.ImageChops.difference(a, b).getbbox()` — `None` means nothing
  changed, whatever the tool said.
- **Is the pointer where you think?** `hyprctl cursorpos` (logical
  coords) after any pointer move; calibrate against it in a closed
  loop before clicking with real input.
- **Does focus exist at all?** `hyprctl activewindow -j` — `None`
  means the compositor will route input nowhere.
- **Did the window end up where intended?** `list_windows` →
  `workspace_id` / `on_current_space`, or `hyprctl clients -j`.

## Sessions, idle-TTL, and recording ownership

- Sessions idle out after **300 s** without a tool call carrying
  their `session` id (`CUA_DRIVER_RS_SESSION_IDLE_TTL_SECS`
  overrides; the effective value is in `get_config` and in
  `start_session`'s structured output as `idle_ttl_secs`). Pass
  `session` on every call — including read-only ones — to keep the
  clock fresh during long investigations.
- A TTL'd or ended id is recoverable: `start_session` with the SAME
  id starts a fresh session under it (`revived: true`). The gate
  error on a dead session names the id and this remedy.
- **Caveat:** the TTL reclaim also stops any recording the session
  owned — silently (shuv1337/cua#19). After a revival, run
  `cua-driver status` (it does a real daemon round-trip and reports
  recording state) and restart recording if needed; expect the
  trajectory to be split across output dirs when this bites.
- If the daemon socket fails while a recording is live, the CLI
  refuses the in-process fallback (exit 70) instead of punching holes
  in the trajectory — `CUA_DRIVER_RS_ALLOW_DEGRADED_FALLBACK=1`
  overrides.

## Forbidden vectors

Same idea as macOS / Windows — don't shell out to anything that
foregrounds a target:

- `wmctrl -a <window>` — activates the named window.
- `xdotool windowactivate <wid>` — activates.
- `wmctrl -R <window>` — raises and activates.
- `xdotool key --window <wid> alt+Tab` — same problem as Windows
  Alt+Tab.

Prefer cua-driver tools with explicit `window_id`. When in doubt,
ask the user.

## What to expect today

| Intent | Status |
|---|---|
| Snapshot UIA tree | ✅ AT-SPI when available, often partial for GTK4/Qt6 |
| Pixel click | ⚠️ X11/XWayland only, XSendEvent best-effort — silently ignored by some toolkits (see "Pixel click reliability") |
| Element-indexed click | ⚠️ AT-SPI `accDoDefaultAction` when supported |
| Type text | ⚠️ XTest, focus-sensitive |
| Hotkey | ⚠️ XTest, focus-sensitive |
| Screenshot full-display | ✅ X11 (xshm); ✅ Wayland via grim (no portal) |
| Screenshot per-window | ✅ X11; ✅ Hyprland via toplevel-export (correct even when occluded); other Wayland TBD |
| launch_app | ✅ Hyprland: hidden `special:cua` background launch + a11y env injection + placement guard; elsewhere direct exec / xdg-open. Late dialogs may leak to the active workspace (#15) |
| Headless-X (`--headless-x`) | ✅ private Xvfb + XTest: true off-screen real input for stubborn X11/wx apps; root-crop capture; software GL |
| Session lifecycle | ✅ idle-TTL 300s, revivable ids (`start_session` same id), TTL discoverable; TTL reclaim kills owned recordings (#19) |
| Recording | ✅ opt-in (`CUA_RECORDING_ENABLED=1`); wlr-screencopy on Wayland / `x11grab` on X11; ffmpeg required |

Until Linux reaches GA, treat this doc as a planning placeholder
rather than a contract.
