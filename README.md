# Xilous Usage Monitor

A small native Windows desktop widget for Claude Code usage and the device's
load. There is no browser runtime.

## The widget

Two ring gauges, each with its figures beside it, and no product name on the
widget itself:

- **Usage** (left): the 5h window on the outer ring and the 7d window on the
  inner ring, each with its **percentage used** and reset countdown. A short
  tick across a ring marks where an even spend would have put you; a ring turns
  amber, then red, when it runs high or well ahead of that pace.
- **Device** (right): CPU load on the outer ring and RAM in use on the inner
  ring. Both shift from green through amber to red as headroom runs out.

The widget shows Claude Code only. Codex and Antigravity are no longer shown or
polled, and any provider choices saved by an older version load as Claude Code
only. The Claude tray tooltip still includes the model-specific **Fable** quota,
which comes from the same usage request; no additional request is sent.

- Percentages represent the account quota **used**, not remaining, token totals
  or dollar spend.
- An em dash is unavailable data, never a fabricated 0%. `Offline` means the
  sign-in needs attention; `Waiting` means no reading has landed yet;
  `Not reported` means that quota window was absent. `Unknown` is an
  unavailable reset time with a valid percentage.
- A failed request keeps the last reading on screen while it retries; the
  hairline along the top stays full until a new reading lands. The figures
  flash only when a reading differs from the one shown.
- The adapter never runs a CLI, sends a prompt, or refreshes tokens itself. It
  reads only the read-only usage endpoint. On an expired Claude sign-in, run
  `claude` and use `/login`, then **Refresh** from the tray.

The HTTP usage endpoint is an internal service interface, not a stable public
API.

## Windows behaviour

### Appearance Studio

Right-click the widget or tray icon and choose **Appearance...**. The studio
provides System, Light, Dark and Custom modes; Midnight, Porcelain, Evergreen
and Afterglow palettes; and native RGB color pickers for the background, text
and three provider accents. Clicking a swatch switches to Custom only when a
color is confirmed. Cancel leaves the current appearance unchanged. The
**Widget size** slider makes the widget larger or smaller, from 75% to 200% in
5% steps, and the widget resizes live as you drag, staying on its edge.

Changes update the widget immediately and persist automatically. The studio's
preview uses explicitly labelled sample usage. **Undo changes** restores the
appearance from when the studio opened; **Reset** restores System mode, the
default custom palette and 100% widget size. **Done**, Escape or the close button
closes the studio without quitting the monitor. Controls support Tab and Space,
and the slider also takes the arrow keys and PageUp/PageDown. High-usage warning
gauges retain their amber/red semantic colors. Customization applies to the
widget, not the Windows shell's notification-area icon badges.

The widget is collapsed to just its rings and docked on an outer edge of your
desktop, floating above normal application windows with a transparent
background. On the left or right edge the usage and device rings stack; on the
top or bottom edge they sit side by side. **Hover** it and it expands into the
full widget, with its background, figures and reset times, growing away from
the edge it sits on; it collapses again a moment after the cursor leaves.

At the default 100% widget size, the full widget is 310 by 62 logical pixels and
the collapsed one 136 by 62 (62 by 136 stacked). The **Widget size** slider
scales those from 75% to 200%, on top of monitor DPI scaling. Dark and light
system themes are supported. Ambient animation runs at 10 Hz and motion at the
fastest timer rate while something moves; CPU and RAM sampling runs at 2
seconds. None of it causes additional network polls.

**Drag** the widget anywhere, across monitors too. Near an outer edge a magnet
pulls it in, and close enough it snaps on and slides along the edge; pull it
away to free it. Let go and it carries the throw onto the nearest outer edge.
Edges shared by two monitors don't count, so it never docks in the middle of
the desktop. It remembers its edge across restarts; a position saved by an
older version docks to the nearest edge. On a fresh start it docks on the right
edge of the primary display, a third of the way down. Removed monitors or a
changed display layout move it to the nearest outer edge.

Right-click the widget or its tray icon for appearance, refresh, update
frequency, settings, visibility, and **Reset Position** (returns it to the
right edge). The menu always opens above the widget. Old taskbar/click-through
settings migrate to interactive floating mode.

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
