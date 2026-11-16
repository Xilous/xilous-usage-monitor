# Product

## Register

product

## Users and purpose

Windows users monitoring Claude, Codex and Antigravity quotas while working.
The native desktop widget shows percentage used and reset countdowns at a glance.

## Design principles

- Keep the compact native interface: rings when collapsed, straight pace bars
  when expanded, the one unrolling into the other.
- Identify each quota window by its colour and label it; warnings use their
  own colours and hatching, never a window's colour.
- Distinguish unavailable readings from zero usage.
- Preserve readable figures and monitor DPI scaling.
- Keep monitoring lightweight and avoid additional network polls for animation.

## Scope

Show the Claude windows the account reports (5h, 7d and Fable) and the
device's load. Themes are nine colour roles with presets; derived colours
(tracks, ticks, muted text) always come from the background and text.
