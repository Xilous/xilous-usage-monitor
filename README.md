# Xilous Usage Monitor

A small native Windows desktop widget for Claude Code usage and the device's
load. There is no browser runtime.

## The widget

Collapsed, the widget is a pill of two ring clusters, with no product name on
it. Hovered, it grows into a card and each ring unrolls into a straight pace
bar, one row per ring:

- **Usage** (left cluster, top rows): the 5h window on the outer ring, the 7d
  window inside it, and the model-specific **Fable** quota innermost, each
  with its **percentage used** and reset countdown. Expanded, the 7d and
  Fable figures read as usage over pace, `58% / 65%`, the denominator being
  where an even spend would have put the window by now. A tick across each ring,
  and a notch across each bar, marks where an even spend would have put you.
  A window that runs 8 points ahead of that pace turns the over-pace colour,
  and 18 points ahead the far-over colour, however low or high its
  percentage is; on a bar, the stretch past the notch is hatched, so how far
  ahead you are reads as a length. On a plan with no 5h limit the usage
  response carries no 5h window, so the widget shows two usage rings and two
  bars, 7d and Fable, and the tray badge shows the 7d figure. An account
  without the Fable quota has no Fable ring. This follows the response, not a
  setting: each account shows the windows it has.
- **Device** (right cluster, bottom rows): CPU load and RAM in use, each in its
  own colour so the two rings never read as one. Both warm toward the warning
  colours as headroom runs out.

With two usage rings or fewer, the collapsed rings carry the most pressing
usage percentage in their centre: the window furthest ahead of its pace, else
the outermost. It moves out to its row as the widget expands. Each row's label
is drawn in its window's colour, so the rows double as the key.

The widget shows Claude Code only. Codex and Antigravity are no longer shown or
polled, and any provider choices saved by an older version load as Claude Code
only. The Fable quota on the usage ring and in the Claude tray tooltip comes
from the same usage request; no additional request is sent.

- Percentages represent the account quota **used**, not remaining, token totals
  or dollar spend.
- An em dash is unavailable data, never a fabricated 0%. `Offline` means the
  sign-in needs attention; `Waiting` means no reading has landed yet;
  `Not reported` means that quota window was absent. `Unknown` is an
  unavailable reset time with a valid percentage, and `Idle` a window with no
  usage that has not started yet.
- Each window in the usage response is read on its own, so a window that is
  missing or changes shape never takes the others down with it. A window with
  no usable percentage, a negative one, or a reset far beyond its length counts
  as absent.
- A failed request keeps the last reading on screen while it retries; the
  hairline along the top stays full until a new reading lands. After three poll
  intervals (at least ten minutes) without a fresh reading, the usage rings dim
  and say `Stale` in place of the countdown. The figures flash only when a
  reading differs from the one shown.
- When a reset time passes, the widget polls every 5 seconds for at most a
  minute to pick up the new window, then falls back to its regular interval.
- The adapter never runs a CLI, sends a prompt, or refreshes tokens itself. It
  reads only the read-only usage endpoint. On an expired Claude sign-in, run
  `claude` and use `/login`, then **Refresh** from the tray.

The HTTP usage endpoint is an internal service interface, not a stable public
API.

## Windows behaviour

### Appearance Studio

Right-click the widget or tray icon and choose **Appearance...**. A theme is
nine colours: background, text, 5h, 7d, Fable, CPU, RAM, over pace and far
over. Muted text, tracks, ticks and hairlines are worked out from the
background and text, so they always stay visible against it. The studio offers:

- **Themes**: System (Graphite or Paper, following Windows), Graphite, Paper,
  Midnight, Porcelain, Evergreen, Afterglow, Black & gold, Red & white, Mono
  and High contrast.
- **Colours**: a native colour picker for each of the nine. Editing one copies
  the current theme into a Custom theme first, and only a confirmed colour is
  kept; Cancel leaves the appearance unchanged.
- **Window colours**: each window in its own colour, one colour in deeper
  shades, or monochrome, where colour appears only for a warning.
- **Warning colours**: the theme's own pair, a colour-blind-safe yellow and
  magenta, or hatching only.
- **Background**: Solid (a see-through pill collapsed, an opaque card
  expanded) or Floating rings (no backing at all until the card opens).
- **Widget size**: 75% to 200% in 5% steps; the widget resizes live as you
  drag, staying on its edge.

Changes update the widget immediately and persist automatically; settings from
older versions carry over, including a palette picked from the old presets.
**Undo changes** restores the appearance from when the studio opened;
**Reset** restores the System theme and 100% widget size. **Done**, Escape or
the close button closes the studio without quitting the monitor. Controls
support Tab and Space, and the slider also takes the arrow keys and
PageUp/PageDown. Customization applies to the widget, not the Windows shell's
notification-area icon badges.

The widget is collapsed to a pill of rings and docked on a screen edge,
floating above normal application windows. On the left or right edge
the usage and device rings stack; on the top or bottom edge they sit side by
side. **Hover** it and it grows into the card of pace bars, away from the edge
it sits on; it collapses again a moment after the cursor leaves.

At the default 100% widget size, the collapsed widget is 136 by 62 logical
pixels (62 by 136 stacked) and the card 340 wide: 121 tall with two usage
windows, 145 with three. The **Widget size** slider
scales those from 75% to 200%, on top of monitor DPI scaling. Dark and light
system themes are supported. Ambient animation runs at 10 Hz and motion at the
fastest timer rate while something moves; CPU and RAM sampling runs at 2
seconds. None of it causes additional network polls.

**Drag** the widget anywhere, across monitors too. Near an edge a magnet pulls
it in, and close enough it snaps on and slides along the edge; pull it away to
free it. Let go and it carries the throw onto the nearest edge. With more than
one monitor, the seam between two of them counts as an edge too: the widget
docks flush against the seam on whichever side you drop it, stays on that
monitor, and grows away from the seam when hovered. Dragging across a seam
catches on it briefly before letting go onto the other monitor. It remembers
its edge across restarts; a position saved by an older version docks to the
nearest edge. On a fresh start it docks on the right edge of the primary
display, a third of the way down. Removed monitors or a changed display layout
move it to the nearest edge.

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
.\target\release\xilous-usage-monitor.exe --preview weekly-only.bmp --no-session
.\target\release\xilous-usage-monitor.exe --preview stale.bmp --stale
.\target\release\xilous-usage-monitor.exe --preview collapsed.bmp --docked --palette 0
.\target\release\xilous-usage-monitor.exe --preview hovered.bmp --docked --expanded
```

`--docked` renders the collapsed widget over a light, a busy and a dark
wallpaper (add `--stacked` for the upright form, `--expanded` for the fully
expanded card, or `--morph T` for a frame part way, T from 0 to 1), and
`--palette N` renders with the Nth theme preset (0 Graphite, 1 Paper,
2 Midnight, 3 Porcelain, 4 Evergreen, 5 Afterglow, 6 Black & gold, 7 Red &
white, 8 Mono, 9 High contrast).

`--check-codex` performs one read-only Codex poll and writes a sanitized summary
to stdout (or an error category to stderr), exiting 0 on success and 1 on failure.
As a Windows GUI executable, use redirected output or `Start-Process -Wait` when
running diagnostics from PowerShell.
