# Xilous Usage Monitor

A small native Windows desktop widget for Claude, Codex and Antigravity usage.
This fork's compact cards show quota-window labels, **percentage
used**, reset countdowns and warning-coloured gauges. There is no browser runtime.

## Claude Fable

The Claude card includes a **Fable** row showing its model-specific quota used
and reset countdown. It follows the account's reported Fable allocation across
sessions, not token estimates or an individual agent's share. The Claude tray
tooltip includes the same reading. Enabling Claude enables this row too.

Fable data comes from model-scoped `limits` in the existing Claude usage request;
no additional request or Fable prompt is sent. Missing or malformed Fable data
shows **Not reported**, while a valid percentage without a reset shows **Unknown**.
Overall Claude 5h and 7d quotas remain visible.

## Codex

Claude and Codex are enabled on fresh installs. Existing saved provider choices
are preserved: right-click a tray icon and enable **Models > Codex**. To start
with only Codex selected, launch `xilous-usage-monitor.exe --codex-only`.
Close any already-running copy first (the app is single-instance).

Sign into Codex with your ChatGPT account. The monitor reads
`%CODEX_HOME%\auth.json`, or `%USERPROFILE%\.codex\auth.json` when `CODEX_HOME`
is unset, and makes a read-only request to the existing Codex usage endpoint.
Credentials are never displayed. API-key billing and credentials stored only
in an OS keyring are not supported by this file-based adapter.

- Percentages represent the account quota **used**, not remaining, token totals,
  dollar spend, or separate Astra/Sol consumption.
- Codex displays only the general **7d** quota, including its reset countdown.
  Its tray badge and tooltip also use weekly usage. The 5h row is hidden;
  model-specific limits such as Codex-Spark are not substituted for Codex usage.
  Quotas are mapped by reported duration, not response order.
- An em dash is unavailable data, never a fabricated 0%. `Offline` means the
  provider failed; `Waiting` means loading/retrying; `Not reported` means that
  quota window was absent. `Unknown` is an unavailable reset time with a valid
  percentage.
- On an expired Codex sign-in, use Codex to sign in again, then **Refresh** from
  the tray. Credential-file changes are also detected while auth polling is paused.
- No adapter runs its CLI, sends a prompt, or refreshes tokens itself. The
  Claude adapter reads only the read-only usage endpoint and never sends a model
  request. On an expired Claude sign-in, run `claude` and use `/login`, then
  **Refresh** from the tray.

The HTTP usage endpoint is an internal service interface, not a stable public
API. A future adapter can use the documented
[Codex app-server rate-limit API](https://developers.openai.com/codex/app-server)
to support CLI-managed authentication. This build retains the repository's
existing lightweight HTTP adapter.

## Windows behaviour

### Appearance Studio

Right-click the widget or tray icon and choose **Appearance...**. The studio
provides System, Light, Dark and Custom modes; Midnight, Porcelain, Evergreen
and Afterglow palettes; and native RGB color pickers for the background, text
and three provider accents. Clicking a swatch switches to Custom only when a
color is confirmed. Cancel leaves the current appearance unchanged. The
**Widget size** slider makes the widget larger or smaller, from 75% to 200% in
5% steps, and the widget resizes live as you drag, keeping its top-left corner
in place.

Changes update the widget immediately and persist automatically. The studio's
preview uses explicitly labelled sample usage. **Undo changes** restores the
appearance from when the studio opened; **Reset** restores System mode, the
default custom palette and 100% widget size. **Done**, Escape or the close button
closes the studio without quitting the monitor. Controls support Tab and Space,
and the slider also takes the arrow keys and PageUp/PageDown. High-usage warning
gauges retain their amber/red semantic colors. Customization applies to the
widget, not the Windows shell's notification-area icon badges.

At the default 100% widget size, the widget is 62 logical pixels tall to fit
Claude's Fable row, and each provider takes 174 logical pixels with an 8-pixel
gap. The **Widget size** slider scales all of that from 75% to 200%, on top of
monitor DPI scaling. Each provider is identified by its accent colour (set in the appearance studio), and
the used/reset header explains the figures. Dark and light system themes are
supported. Animation runs at 10 Hz, RAM sampling at 2 seconds; neither causes
additional network polls.

The widget floats above normal application windows. **Drag anywhere on it** to
move it in any direction, including between monitors. It always stays fully on
screen: it can't be dragged off an edge, and it can span two monitors as long as
every corner is on one of them (the taskbar area doesn't count). It remembers
the dropped position across restarts; taskbar/tray updates no longer move it
back to the bottom of the screen. On a fresh start it appears near the
top-right of the primary display. Removed monitors, a changed display layout,
or a larger widget size bring it back fully on screen at the nearest spot.

Right-click the widget or its tray icon for providers, refresh, visibility, and
**Reset Position** (returns it to the top-right). Old taskbar/click-through
settings migrate to interactive floating mode; provider choices are preserved.

## Build and verify

With Rust and the appropriate Windows linker/resource tools installed:

```powershell
cargo test --locked
cargo build --release --locked
```

Both MSVC with Windows build tools and GNU with MinGW-w64 are supported. The
binary is `target\release\xilous-usage-monitor.exe`.

Offline previews exercise the actual GDI renderer at 200% DPI using fixture data;
they don't read credentials, poll providers, start the tray, or change settings:

```powershell
.\target\release\xilous-usage-monitor.exe --preview dark.bmp
.\target\release\xilous-usage-monitor.exe --preview light.bmp --light
.\target\release\xilous-usage-monitor.exe --preview unavailable.bmp --unavailable
```

`--check-codex` performs one read-only Codex poll and writes a sanitized summary
to stdout (or an error category to stderr), exiting 0 on success and 1 on failure.
As a Windows GUI executable, use redirected output or `Start-Process -Wait` when
running diagnostics from PowerShell.
