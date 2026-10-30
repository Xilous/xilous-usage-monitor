//! Edge docking for the floating widget.
//!
//! The widget lives on an outer edge of the desktop. Dragged, it follows the
//! cursor freely; near an outer edge a magnet leans it in, and inside the
//! catch distance it snaps on and slides along that edge. Let go, it carries
//! the throw's momentum onto the nearest outer edge. On the left or right edge
//! the two ring pairs stack; on the top or bottom they sit side by side.
//! Hovered, it expands into the full widget, growing away from its edge.
//!
//! This module is the geometry, the springs and the layouts only. It knows
//! nothing of Win32, so it is unit tested on its own: window.rs feeds it the
//! monitors, the cursor and the clock, and draws whatever frame it returns.

use crate::window::{
    FIGURE_W, LABEL_W, LEFT_DIVIDER_W, RESET_W, RING_D, RING_Y, SPLIT_H, SPLIT_X, SPLIT_Y,
    SYSTEM_X, TEXT_DX, USAGE_X, WIDGET_HEIGHT, WIDGET_W,
};

/// Magnet and throw tuning, in base pixels (96 DPI) and seconds.
const MAGNET_RANGE: f64 = 160.0;
const CATCH_DISTANCE: f64 = 32.0;
/// A caught widget lets go only past this multiple of the catch distance, so
/// it does not chatter on and off the edge at the boundary.
const RELEASE_HYSTERESIS: f64 = 1.5;
/// How far a release carries: the landing spot is projected this many seconds
/// along the throw.
const THROW_REACH: f64 = 0.22;
const MAX_THROW_SPEED: f64 = 6000.0;
/// Movement before a press becomes a drag, so a plain click does not collapse
/// the widget.
const DRAG_THRESHOLD: f64 = 4.0;
/// How long the widget stays expanded after the cursor leaves it.
const HOVER_LINGER: f64 = 0.3;
/// How much a picked-up widget grows.
const LIFT_GROWTH: f64 = 0.06;
/// Only the last stretch of a drag counts toward the throw.
const THROW_WINDOW: f64 = 0.09;
const THROW_STALE: f64 = 0.06;

/// Length of the drag handle's hairlines.
const HANDLE_LEN: f64 = 25.0;

// ---- Geometry ---------------------------------------------------------------

/// A rectangle in screen pixels, half-open like a Win32 RECT.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub left: f64,
    pub top: f64,
    pub right: f64,
    pub bottom: f64,
}

impl Rect {
    fn distance_to(&self, p: [f64; 2]) -> f64 {
        let dx = p[0] - p[0].clamp(self.left, self.right);
        let dy = p[1] - p[1].clamp(self.top, self.bottom);
        dx.hypot(dy)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
    Top,
    Bottom,
}

impl Side {
    /// The left and right edges stack the ring pairs; the others lay them side
    /// by side.
    pub fn stacked(self) -> bool {
        matches!(self, Side::Left | Side::Right)
    }

    /// Outward from the screen, toward this edge.
    fn normal(self) -> [f64; 2] {
        match self {
            Side::Left => [-1.0, 0.0],
            Side::Right => [1.0, 0.0],
            Side::Top => [0.0, -1.0],
            Side::Bottom => [0.0, 1.0],
        }
    }
}

/// A stretch of outer edge the widget may sit on. `fixed` is the edge's
/// coordinate across it, `from..to` its extent along it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Segment {
    pub side: Side,
    pub fixed: f64,
    pub from: f64,
    pub to: f64,
    pub monitor: usize,
}

/// The outer edges of the desktop: every monitor edge, minus wherever it
/// touches another monitor, so the widget never docks in the middle of a
/// multi-monitor desktop.
pub fn edge_segments(monitors: &[Rect]) -> Vec<Segment> {
    fn subtract(ranges: Vec<(f64, f64)>, a: f64, b: f64) -> Vec<(f64, f64)> {
        let mut out = Vec::with_capacity(ranges.len() + 1);
        for (from, to) in ranges {
            if b <= from || a >= to {
                out.push((from, to));
                continue;
            }
            if a > from {
                out.push((from, a));
            }
            if b < to {
                out.push((b, to));
            }
        }
        out
    }

    let mut segments = Vec::new();
    for (i, m) in monitors.iter().enumerate() {
        let sides = [
            (Side::Left, m.left, m.top, m.bottom),
            (Side::Right, m.right, m.top, m.bottom),
            (Side::Top, m.top, m.left, m.right),
            (Side::Bottom, m.bottom, m.left, m.right),
        ];
        for (side, fixed, from, to) in sides {
            let mut ranges = vec![(from, to)];
            for (j, n) in monitors.iter().enumerate() {
                if j == i {
                    continue;
                }
                let (touches, a, b) = match side {
                    Side::Left => (n.right == m.left, n.top, n.bottom),
                    Side::Right => (n.left == m.right, n.top, n.bottom),
                    Side::Top => (n.bottom == m.top, n.left, n.right),
                    Side::Bottom => (n.top == m.bottom, n.left, n.right),
                };
                if touches {
                    ranges = subtract(ranges, a, b);
                }
            }
            segments.extend(ranges.into_iter().map(|(from, to)| Segment {
                side,
                fixed,
                from,
                to,
                monitor: i,
            }));
        }
    }
    segments
}

/// Where a collapsed widget would sit on an outer edge, and how far the
/// widget being placed is from it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Spot {
    pub center: [f64; 2],
    pub side: Side,
    pub monitor: usize,
    /// From the near side of a collapsed widget centred at the query point to
    /// the edge, never negative.
    pub gap: f64,
}

/// The docked spot nearest to a collapsed widget centred at `p`. `size` gives
/// the collapsed size for a stacked (true) or side-by-side (false) widget.
pub fn nearest_spot(
    p: [f64; 2],
    segments: &[Segment],
    size: impl Fn(bool) -> [f64; 2],
) -> Option<Spot> {
    let mut best: Option<(f64, Spot)> = None;
    for s in segments {
        let stacked = s.side.stacked();
        let [w, h] = size(stacked);
        let along = if stacked { h } else { w };
        if s.to - s.from < along {
            continue;
        }
        let (left, top, gap) = match s.side {
            Side::Left => {
                let left = s.fixed;
                (
                    left,
                    (p[1] - h / 2.0).clamp(s.from, s.to - h),
                    (p[0] - w / 2.0) - left,
                )
            }
            Side::Right => {
                let left = s.fixed - w;
                (
                    left,
                    (p[1] - h / 2.0).clamp(s.from, s.to - h),
                    left - (p[0] - w / 2.0),
                )
            }
            Side::Top => {
                let top = s.fixed;
                (
                    (p[0] - w / 2.0).clamp(s.from, s.to - w),
                    top,
                    (p[1] - h / 2.0) - top,
                )
            }
            Side::Bottom => {
                let top = s.fixed - h;
                (
                    (p[0] - w / 2.0).clamp(s.from, s.to - w),
                    top,
                    top - (p[1] - h / 2.0),
                )
            }
        };
        let center = [left + w / 2.0, top + h / 2.0];
        let d = (p[0] - center[0]).hypot(p[1] - center[1]);
        if best.as_ref().map_or(true, |(bd, _)| d < *bd) {
            best = Some((
                d,
                Spot {
                    center,
                    side: s.side,
                    monitor: s.monitor,
                    gap: gap.max(0.0),
                },
            ));
        }
    }
    best.map(|(_, spot)| spot)
}

fn nearest_monitor(p: [f64; 2], monitors: &[Rect]) -> usize {
    let mut best = 0;
    let mut best_d = f64::INFINITY;
    for (i, m) in monitors.iter().enumerate() {
        let d = m.distance_to(p);
        if d < best_d {
            best_d = d;
            best = i;
        }
    }
    best
}

/// Keep a free widget's centre where the whole collapsed widget stays on the
/// monitor nearest to it.
fn keep_on_monitor(p: [f64; 2], size: [f64; 2], monitors: &[Rect]) -> [f64; 2] {
    let m = monitors[nearest_monitor(p, monitors)];
    let [w, h] = size;
    let clamp = |v: f64, lo: f64, hi: f64| if hi < lo { lo } else { v.clamp(lo, hi) };
    [
        clamp(p[0], m.left + w / 2.0, m.right - w / 2.0),
        clamp(p[1], m.top + h / 2.0, m.bottom - h / 2.0),
    ]
}

// ---- Springs ----------------------------------------------------------------

/// Spring constants from a perceived duration and a bounce, the same mapping
/// Motion uses for `visualDuration` and `bounce`.
#[derive(Clone, Copy, Debug)]
pub struct SpringCfg {
    stiffness: f64,
    damping: f64,
}

impl SpringCfg {
    pub const fn new(duration: f64, bounce: f64) -> Self {
        let root = 2.0 * std::f64::consts::PI / (duration * 1.2);
        let stiffness = root * root;
        let bounce = if bounce < 0.0 {
            0.0
        } else if bounce > 0.95 {
            0.95
        } else {
            bounce
        };
        // const fn has no sqrt; root is sqrt(stiffness) already.
        Self {
            stiffness,
            damping: 2.0 * (1.0 - bounce) * root,
        }
    }
}

/// A dragged widget trails the cursor a touch.
const FOLLOW: SpringCfg = SpringCfg::new(0.1, 0.0);
/// Sliding along an edge it is caught by.
const SLIDE: SpringCfg = SpringCfg::new(0.12, 0.0);
/// Snapping onto an edge, and gliding there after a release.
const SNAP: SpringCfg = SpringCfg::new(0.45, 0.3);
/// Turning between side by side and stacked.
const MORPH: SpringCfg = SpringCfg::new(0.4, 0.12);
/// Picked up and set down.
const LIFT: SpringCfg = SpringCfg::new(0.25, 0.35);
const HOVER_IN: SpringCfg = SpringCfg::new(0.34, 0.15);
const HOVER_OUT: SpringCfg = SpringCfg::new(0.28, 0.0);

#[derive(Clone, Copy, Debug, Default)]
pub struct Spring {
    pub x: f64,
    pub v: f64,
}

impl Spring {
    fn at(x: f64) -> Self {
        Self { x, v: 0.0 }
    }

    /// Advance by `dt` seconds toward `target`, in small fixed substeps so a
    /// long frame cannot destabilise a stiff spring.
    fn step(&mut self, target: f64, cfg: SpringCfg, dt: f64) {
        let mut left = dt;
        while left > 0.0 {
            let h = left.min(0.004);
            let accel = -cfg.stiffness * (self.x - target) - cfg.damping * self.v;
            self.v += accel * h;
            self.x += self.v * h;
            left -= h;
        }
    }

    fn settled(&self, target: f64, eps: f64) -> bool {
        (self.x - target).abs() < eps && self.v.abs() < eps * 10.0
    }
}

// ---- Layouts ----------------------------------------------------------------

/// Where everything sits inside the widget, in base pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Layout {
    pub w: f64,
    pub h: f64,
    /// Top-left of the usage and device rings.
    pub usage: [f64; 2],
    pub device: [f64; 2],
    /// The hairline between the two ring pairs, and the drag handle: x, y, w, h.
    pub split: [f64; 4],
    pub handle: [f64; 4],
    /// Left edge of each block's figures.
    pub usage_text_x: f64,
    pub device_text_x: f64,
}

fn mix(a: f64, b: f64, t: f64) -> f64 {
    a + (b - a) * t
}

fn mix_n<const N: usize>(a: [f64; N], b: [f64; N], t: f64) -> [f64; N] {
    std::array::from_fn(|i| mix(a[i], b[i], t))
}

impl Layout {
    fn mix(&self, o: &Layout, t: f64) -> Layout {
        Layout {
            w: mix(self.w, o.w, t),
            h: mix(self.h, o.h, t),
            usage: mix_n(self.usage, o.usage, t),
            device: mix_n(self.device, o.device, t),
            split: mix_n(self.split, o.split, t),
            handle: mix_n(self.handle, o.handle, t),
            usage_text_x: mix(self.usage_text_x, o.usage_text_x, t),
            device_text_x: mix(self.device_text_x, o.device_text_x, t),
        }
    }
}

/// The spacing between a ring and the hairline beside it.
fn pad() -> f64 {
    (SYSTEM_X - SPLIT_X) as f64
}

fn usage_text_w() -> f64 {
    (LABEL_W + FIGURE_W + RESET_W) as f64
}

fn device_text_w() -> f64 {
    (LABEL_W + FIGURE_W) as f64
}

/// Space after the last figure, at the far end of the expanded widget.
fn end_pad() -> f64 {
    (WIDGET_W - (SYSTEM_X + TEXT_DX + LABEL_W + FIGURE_W)) as f64
}

/// The two ring pairs side by side: expanded is the shipped widget; collapsed
/// drops the figure columns and closes the gap.
fn side_by_side(expanded: bool) -> Layout {
    let ring_y = RING_Y as f64;
    let usage_x = USAGE_X as f64;
    let short = WIDGET_HEIGHT as f64;
    let (split_x, device_x, w) = if expanded {
        (SPLIT_X as f64, SYSTEM_X as f64, WIDGET_W as f64)
    } else {
        let split_x = usage_x + RING_D as f64 + pad();
        let device_x = split_x + pad();
        (split_x, device_x, device_x + RING_D as f64 + end_pad())
    };
    Layout {
        w,
        h: short,
        usage: [usage_x, ring_y],
        device: [device_x, ring_y],
        split: [split_x, SPLIT_Y as f64, 1.0, SPLIT_H as f64],
        handle: [
            0.0,
            (short - HANDLE_LEN) / 2.0,
            LEFT_DIVIDER_W as f64,
            HANDLE_LEN,
        ],
        usage_text_x: usage_x + TEXT_DX as f64,
        device_text_x: device_x + TEXT_DX as f64,
    }
}

/// The two ring pairs stacked, usage on top. Collapsed is the side-by-side
/// collapsed widget turned on its side. Expanded puts each block's figures
/// beside its ring, on the left when `mirror` (the right edge) so the rings
/// themselves stay put as it expands.
fn stacked(expanded: bool, mirror: bool) -> Layout {
    let flat = side_by_side(false);
    let ring_inset = RING_Y as f64;
    let w = if expanded {
        ring_inset + RING_D as f64 + pad() + usage_text_w() + end_pad()
    } else {
        flat.h
    };
    let ring_x = if mirror {
        w - ring_inset - RING_D as f64
    } else {
        ring_inset
    };
    let (usage_text_x, device_text_x) = if mirror {
        (
            ring_x - pad() - usage_text_w(),
            ring_x - pad() - device_text_w(),
        )
    } else {
        (ring_x + TEXT_DX as f64, ring_x + TEXT_DX as f64)
    };
    let split_inset = SPLIT_Y as f64;
    Layout {
        w,
        h: flat.w,
        usage: [ring_x, flat.usage[0]],
        device: [ring_x, flat.device[0]],
        split: [split_inset, flat.split[0], w - 2.0 * split_inset, 1.0],
        handle: [
            (w - HANDLE_LEN) / 2.0,
            0.0,
            HANDLE_LEN,
            LEFT_DIVIDER_W as f64,
        ],
        usage_text_x,
        device_text_x,
    }
}

/// The layout `stacked` of the way from side by side to stacked and
/// `expanded` of the way from collapsed to the full widget.
pub fn layout(stacked_t: f64, expanded: f64, mirror: bool) -> Layout {
    let flat = side_by_side(false).mix(&side_by_side(true), expanded);
    let upright = stacked(false, mirror).mix(&stacked(true, mirror), expanded);
    flat.mix(&upright, stacked_t)
}

// ---- Motion -----------------------------------------------------------------

/// A press on the widget, which becomes a drag once it moves far enough.
struct Press {
    start: [f64; 2],
    /// Cursor minus the collapsed widget's centre.
    grab: [f64; 2],
    /// Where the cursor puts the widget's centre.
    at: [f64; 2],
    active: bool,
    caught: bool,
    /// Where the free widget is headed this frame, magnet included.
    free: Option<[f64; 2]>,
    samples: Vec<(f64, [f64; 2])>,
}

/// One frame of the widget: where its window goes and what to draw in it.
#[derive(Clone, Copy, Debug)]
pub struct Frame {
    /// Window position and size in screen pixels.
    pub left: f64,
    pub top: f64,
    pub width: f64,
    pub height: f64,
    /// Screen pixels per base pixel, lift included.
    pub scale: f64,
    pub layout: Layout,
    /// How far each morph has gone, and how much of the background shows.
    pub stacked: f64,
    pub expanded: f64,
    pub hover: f64,
    pub side: Side,
    /// Figures to the left of the rings (docked on the right edge).
    pub mirror: bool,
    /// Whether the widget sits on an edge rather than following a drag.
    pub docked: bool,
    /// Something is still moving, so another frame is needed soon.
    pub moving: bool,
}

pub struct Motion {
    x: Spring,
    y: Spring,
    stacked: Spring,
    hover: Spring,
    expand: Spring,
    lift: Spring,
    /// The point the widget docks nearest to.
    anchor: [f64; 2],
    placed: bool,
    side: Side,
    monitor: usize,
    press: Option<Press>,
    hovered: bool,
    left_at: f64,
}

impl Motion {
    pub fn new(anchor: [f64; 2]) -> Self {
        Self {
            x: Spring::at(anchor[0]),
            y: Spring::at(anchor[1]),
            stacked: Spring::default(),
            hover: Spring::default(),
            expand: Spring::default(),
            lift: Spring::default(),
            anchor,
            placed: false,
            side: Side::Right,
            monitor: 0,
            press: None,
            hovered: false,
            left_at: f64::NEG_INFINITY,
        }
    }

    /// The point to save: where the widget docks, or is headed.
    pub fn anchor(&self) -> [f64; 2] {
        self.anchor
    }

    /// Move the widget to the edge nearest `anchor`, gliding there.
    pub fn set_anchor(&mut self, anchor: [f64; 2]) {
        self.anchor = anchor;
        self.press = None;
    }

    pub fn dragging(&self) -> bool {
        self.press.as_ref().is_some_and(|p| p.active)
    }

    pub fn pressed(&self) -> bool {
        self.press.is_some()
    }

    fn collapsed_size(&self, scale: f64) -> [f64; 2] {
        let l = layout(self.stacked.x.clamp(0.0, 1.0), 0.0, false);
        [l.w * scale, l.h * scale]
    }

    /// The button went down on the widget at `cursor`. `scale` is screen
    /// pixels per base pixel.
    pub fn press(&mut self, cursor: [f64; 2], now: f64, scale: f64) {
        // A grab on the expanded figures is pulled in over the collapsed
        // widget, which is what the drag will be carrying.
        let [w, h] = self.collapsed_size(scale);
        let grab = [
            (cursor[0] - self.x.x).clamp(-w / 2.0, w / 2.0),
            (cursor[1] - self.y.x).clamp(-h / 2.0, h / 2.0),
        ];
        self.press = Some(Press {
            start: cursor,
            grab,
            at: [cursor[0] - grab[0], cursor[1] - grab[1]],
            active: false,
            caught: false,
            free: None,
            samples: vec![(now, cursor)],
        });
    }

    /// The cursor moved with the button down. `unit` is screen pixels per
    /// 96-DPI pixel.
    pub fn drag(&mut self, cursor: [f64; 2], now: f64, unit: f64) {
        let Some(p) = self.press.as_mut() else {
            return;
        };
        p.at = [cursor[0] - p.grab[0], cursor[1] - p.grab[1]];
        if !p.active
            && (cursor[0] - p.start[0]).hypot(cursor[1] - p.start[1]) > DRAG_THRESHOLD * unit
        {
            p.active = true;
        }
        p.samples.push((now, cursor));
        if p.samples.len() > 16 {
            p.samples.remove(0);
        }
    }

    /// The button came up. Returns whether it ended a drag, in which case the
    /// widget is thrown onto the nearest edge along the release.
    pub fn release(&mut self, now: f64, unit: f64) -> bool {
        let Some(p) = self.press.take() else {
            return false;
        };
        if !p.active {
            return false;
        }
        let v = throw_velocity(&p.samples, now, MAX_THROW_SPEED * unit);
        let from = if p.caught {
            [self.x.x, self.y.x]
        } else {
            p.free.unwrap_or(p.at)
        };
        self.anchor = [from[0] + v[0] * THROW_REACH, from[1] + v[1] * THROW_REACH];
        // The springs set off at the throw's speed, so the glide continues the
        // drag instead of starting from rest.
        self.x.v = v[0];
        self.y.v = v[1];
        true
    }

    pub fn set_hovered(&mut self, hovered: bool, now: f64) {
        if self.hovered && !hovered {
            self.left_at = now;
        }
        self.hovered = hovered;
    }

    /// Advance everything by `dt` seconds and lay out the frame. `scale` is
    /// screen pixels per base pixel (DPI and WIDGET SIZE), `unit` screen
    /// pixels per 96-DPI pixel for the magnet distances. None without
    /// monitors.
    pub fn tick(
        &mut self,
        now: f64,
        dt: f64,
        monitors: &[Rect],
        scale: f64,
        unit: f64,
    ) -> Option<Frame> {
        let segments = edge_segments(monitors);
        if segments.is_empty() {
            return None;
        }
        let dt = dt.clamp(0.0, 0.064);
        let collapsed = |stacked_edge: bool| {
            let l = layout(if stacked_edge { 1.0 } else { 0.0 }, 0.0, false);
            [l.w * scale, l.h * scale]
        };
        let current_size = self.collapsed_size(scale);
        let resting_orientation = self.stacked.x.round().clamp(0.0, 1.0);

        // Where the widget wants to be, which springs take it there across and
        // along its edge, and which way up it should be.
        let target;
        let (cfg_x, cfg_y);
        let want_stacked;
        let docked;
        let dragging = self.dragging();
        match self.press.as_mut().filter(|p| p.active) {
            Some(p) => {
                let free = keep_on_monitor(p.at, current_size, monitors);
                let spot = nearest_spot(free, &segments, collapsed);
                let catch_at =
                    CATCH_DISTANCE * unit * if p.caught { RELEASE_HYSTERESIS } else { 1.0 };
                match spot {
                    Some(spot) if spot.gap <= catch_at => {
                        p.caught = true;
                        target = spot.center;
                        // Snapped on across the edge, sliding freely along it.
                        (cfg_x, cfg_y) = if spot.side.stacked() {
                            (SNAP, SLIDE)
                        } else {
                            (SLIDE, SNAP)
                        };
                        want_stacked = if spot.side.stacked() { 1.0 } else { 0.0 };
                        self.side = spot.side;
                        self.monitor = spot.monitor;
                        docked = true;
                    }
                    _ => {
                        p.caught = false;
                        let mut to = free;
                        // Inside the range the magnet leans it toward the edge,
                        // more the closer it gets, reaching the edge exactly at
                        // the catch distance so the catch never jumps.
                        if let Some(spot) = spot {
                            let range = MAGNET_RANGE * unit;
                            let catch = CATCH_DISTANCE * unit;
                            if spot.gap < range {
                                let p = (1.0 - (spot.gap - catch) / (range - catch).max(1.0))
                                    .clamp(0.0, 1.0);
                                let pull = p * p * (3.0 - 2.0 * p) * spot.gap;
                                let n = spot.side.normal();
                                to = [to[0] + n[0] * pull, to[1] + n[1] * pull];
                            }
                        }
                        p.free = Some(to);
                        target = to;
                        (cfg_x, cfg_y) = (FOLLOW, FOLLOW);
                        want_stacked = resting_orientation;
                        self.monitor = nearest_monitor(to, monitors);
                        docked = false;
                    }
                }
            }
            None => {
                let spot = nearest_spot(self.anchor, &segments, collapsed)?;
                target = spot.center;
                (cfg_x, cfg_y) = (SNAP, SNAP);
                want_stacked = if spot.side.stacked() { 1.0 } else { 0.0 };
                self.side = spot.side;
                self.monitor = spot.monitor;
                self.anchor = spot.center;
                docked = true;
            }
        }

        if !self.placed {
            self.x = Spring::at(target[0]);
            self.y = Spring::at(target[1]);
            self.stacked = Spring::at(want_stacked);
            self.placed = true;
        }
        self.x.step(target[0], cfg_x, dt);
        self.y.step(target[1], cfg_y, dt);
        self.stacked.step(want_stacked, MORPH, dt);
        let lift_to = if dragging { 1.0 } else { 0.0 };
        self.lift.step(lift_to, LIFT, dt);

        // The background comes in at once and leaves a beat after the cursor
        // does; the expansion follows it, but never during a drag.
        let want_hover = !dragging && (self.hovered || now - self.left_at < HOVER_LINGER);
        let hover_to = if want_hover { 1.0 } else { 0.0 };
        let hover_cfg = if want_hover { HOVER_IN } else { HOVER_OUT };
        self.hover.step(hover_to, hover_cfg, dt);
        self.expand.step(hover_to, hover_cfg, dt);

        let t = self.stacked.x.clamp(0.0, 1.0);
        let e = self.expand.x.clamp(0.0, 1.04);
        let a = self.hover.x.clamp(0.0, 1.0);
        let mirror = docked && self.side == Side::Right;
        let lifted = scale * (1.0 + LIFT_GROWTH * self.lift.x);
        let rest = layout(t, 0.0, mirror);
        let l = layout(t, e, mirror);
        let (cw, ch) = (rest.w * lifted, rest.h * lifted);
        let (w, h) = (l.w * lifted, l.h * lifted);

        // The expansion grows away from the edge the widget sits on.
        let grow = |cx: f64, cy: f64| {
            let mut left = cx - cw / 2.0;
            let mut top = cy - ch / 2.0;
            if docked && self.side == Side::Right {
                left += cw - w;
            }
            if docked && self.side == Side::Bottom {
                top += ch - h;
            }
            [left, top]
        };
        let [mut left, mut top] = grow(self.x.x, self.y.x);
        if !dragging {
            // Shift the expansion to stay on its monitor, worked out at the
            // resting spot rather than the live one so a spring's overshoot is
            // never clipped flat against the screen edge.
            let [rl, rt] = grow(target[0], target[1]);
            let m = monitors[self.monitor.min(monitors.len() - 1)];
            let fit = |v: f64, lo: f64, hi: f64| if hi < lo { lo } else { v.clamp(lo, hi) };
            left += fit(rl, m.left, m.right - w) - rl;
            top += fit(rt, m.top, m.bottom - h) - rt;
        }

        let settled = self.x.settled(target[0], 0.3)
            && self.y.settled(target[1], 0.3)
            && self.stacked.settled(want_stacked, 0.002)
            && self.lift.settled(lift_to, 0.002)
            && self.hover.settled(hover_to, 0.002)
            && self.expand.settled(hover_to, 0.002);
        // A linger still running will collapse the widget when it ends.
        let lingering = !self.hovered && now - self.left_at < HOVER_LINGER;

        Some(Frame {
            left,
            top,
            width: w,
            height: h,
            scale: lifted,
            layout: l,
            stacked: t,
            expanded: e,
            hover: a,
            side: self.side,
            mirror,
            docked,
            moving: dragging || !settled || lingering,
        })
    }
}

/// Cursor velocity over the last stretch of a drag, in pixels per second;
/// zero when the cursor had stopped before the release.
fn throw_velocity(samples: &[(f64, [f64; 2])], now: f64, max: f64) -> [f64; 2] {
    let Some(&(last_t, last)) = samples.last() else {
        return [0.0, 0.0];
    };
    if now - last_t > THROW_STALE {
        return [0.0, 0.0];
    }
    let first = samples
        .iter()
        .find(|(t, _)| last_t - t <= THROW_WINDOW)
        .copied()
        .unwrap_or((last_t, last));
    let dt = (last_t - first.0).max(0.008);
    [
        ((last[0] - first.1[0]) / dt).clamp(-max, max),
        ((last[1] - first.1[1]) / dt).clamp(-max, max),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(left: f64, top: f64, right: f64, bottom: f64) -> Rect {
        Rect {
            left,
            top,
            right,
            bottom,
        }
    }

    fn size(stacked: bool) -> [f64; 2] {
        let l = layout(if stacked { 1.0 } else { 0.0 }, 0.0, false);
        [l.w, l.h]
    }

    #[test]
    fn layouts_match_the_shipped_widget_and_its_collapsed_forms() {
        let full = layout(0.0, 1.0, false);
        assert_eq!((full.w, full.h), (310.0, 62.0));
        assert_eq!(full.device, [186.0, 6.0]);
        let flat = layout(0.0, 0.0, false);
        assert_eq!((flat.w, flat.h), (136.0, 62.0));
        assert_eq!(flat.device, [78.0, 6.0]);
        let upright = layout(1.0, 0.0, false);
        assert_eq!((upright.w, upright.h), (62.0, 136.0));
        assert_eq!(upright.usage, [6.0, 12.0]);
        assert_eq!(upright.device, [6.0, 78.0]);
        let tall = layout(1.0, 1.0, true);
        assert_eq!((tall.w, tall.h), (176.0, 136.0));
        // On the right edge the rings keep their distance from the edge.
        assert_eq!(tall.w - tall.usage[0], upright.w - upright.usage[0]);
        assert!(tall.usage_text_x + usage_text_w() <= tall.usage[0]);
    }

    #[test]
    fn shared_monitor_edges_are_not_outer_edges() {
        let monitors = [
            rect(0.0, 0.0, 1920.0, 1080.0),
            rect(1920.0, 0.0, 3840.0, 1080.0),
        ];
        let segments = edge_segments(&monitors);
        assert!(!segments
            .iter()
            .any(|s| s.side == Side::Right && s.monitor == 0));
        assert!(!segments
            .iter()
            .any(|s| s.side == Side::Left && s.monitor == 1));
        assert_eq!(segments.len(), 6);
    }

    #[test]
    fn a_partly_shared_edge_keeps_its_open_stretch() {
        let monitors = [
            rect(0.0, 0.0, 1920.0, 1080.0),
            rect(1920.0, 400.0, 3840.0, 1480.0),
        ];
        let right: Vec<_> = edge_segments(&monitors)
            .into_iter()
            .filter(|s| s.side == Side::Right && s.monitor == 0)
            .collect();
        assert_eq!(right.len(), 1);
        assert_eq!((right[0].from, right[0].to), (0.0, 400.0));
    }

    #[test]
    fn the_nearest_spot_takes_the_edge_and_its_orientation() {
        let monitors = [rect(0.0, 0.0, 1920.0, 1080.0)];
        let segments = edge_segments(&monitors);
        let right = nearest_spot([1880.0, 500.0], &segments, size).unwrap();
        assert_eq!(right.side, Side::Right);
        assert_eq!(right.center, [1920.0 - 31.0, 500.0]);
        let bottom = nearest_spot([900.0, 1060.0], &segments, size).unwrap();
        assert_eq!(bottom.side, Side::Bottom);
        assert_eq!(bottom.center, [900.0, 1080.0 - 31.0]);
        // Clamped along the edge so it never hangs past a corner.
        let corner = nearest_spot([1919.0, 5.0], &segments, size).unwrap();
        let [w, h] = size(corner.side.stacked());
        assert!(corner.center[0] + w / 2.0 <= 1920.0 && corner.center[1] - h / 2.0 >= 0.0);
    }

    #[test]
    fn a_settled_widget_sits_on_its_edge_and_a_drag_magnets_onto_another() {
        let monitors = [rect(0.0, 0.0, 1920.0, 1080.0)];
        let mut m = Motion::new([1900.0, 300.0]);
        let mut now = 0.0;
        let mut frame = None;
        for _ in 0..200 {
            now += 0.016;
            frame = m.tick(now, 0.016, &monitors, 1.0, 1.0);
        }
        let f = frame.unwrap();
        assert_eq!(f.side, Side::Right);
        assert!(!f.moving);
        assert!((f.left + f.width - 1920.0).abs() < 0.5);
        assert!((f.width - 62.0).abs() < 0.01);

        // Drag it to just above the bottom edge and let go without a throw.
        let start = [f.left + 31.0, f.top + 68.0];
        m.press(start, now, 1.0);
        let end = [900.0, 1040.0];
        for i in 1..=30 {
            now += 0.016;
            let t = i as f64 / 30.0;
            m.drag(
                [mix(start[0], end[0], t), mix(start[1], end[1], t)],
                now,
                1.0,
            );
            m.tick(now, 0.016, &monitors, 1.0, 1.0);
        }
        for _ in 0..10 {
            now += 0.016;
            m.tick(now, 0.016, &monitors, 1.0, 1.0);
        }
        assert!(m.release(now + 0.2, 1.0));
        for _ in 0..200 {
            now += 0.016;
            frame = m.tick(now, 0.016, &monitors, 1.0, 1.0);
        }
        let f = frame.unwrap();
        assert_eq!(f.side, Side::Bottom);
        assert!((f.top + f.height - 1080.0).abs() < 0.5);
        assert!((f.height - 62.0).abs() < 0.01);
    }

    #[test]
    fn the_magnet_reaches_the_edge_at_the_catch_distance() {
        let monitors = [rect(0.0, 0.0, 1920.0, 1080.0)];
        let mut m = Motion::new([1900.0, 500.0]);
        m.tick(0.0, 0.016, &monitors, 1.0, 1.0);
        m.press([1889.0, 500.0], 0.0, 1.0);
        // A side-by-side widget centred so its bottom is just outside the catch.
        let y = 1080.0 - 31.0 - CATCH_DISTANCE - 0.5;
        m.drag([800.0, y], 0.1, 1.0);
        m.tick(0.1, 0.016, &monitors, 1.0, 1.0);
        let pulled = m.press.as_ref().unwrap().free.unwrap();
        assert!((pulled[1] + 31.0 - 1080.0).abs() < 1.0);
    }

    #[test]
    fn a_click_is_not_a_drag() {
        let mut m = Motion::new([100.0, 100.0]);
        m.press([100.0, 100.0], 0.0, 1.0);
        m.drag([102.0, 101.0], 0.05, 1.0);
        assert!(!m.dragging());
        assert!(!m.release(0.1, 1.0));
    }

    #[test]
    fn a_throw_carries_and_a_stopped_cursor_does_not() {
        let samples = [(0.0, [0.0, 0.0]), (0.05, [100.0, 0.0])];
        let v = throw_velocity(&samples, 0.05, 6000.0);
        assert!((v[0] - 2000.0).abs() < 1.0 && v[1] == 0.0);
        assert_eq!(throw_velocity(&samples, 0.5, 6000.0), [0.0, 0.0]);
    }

    #[test]
    fn hovering_expands_away_from_the_right_edge() {
        let monitors = [rect(0.0, 0.0, 1920.0, 1080.0)];
        let mut m = Motion::new([1900.0, 300.0]);
        let mut now = 0.0;
        for _ in 0..100 {
            now += 0.016;
            m.tick(now, 0.016, &monitors, 1.0, 1.0);
        }
        m.set_hovered(true, now);
        let mut f = None;
        for _ in 0..100 {
            now += 0.016;
            f = m.tick(now, 0.016, &monitors, 1.0, 1.0);
        }
        let f = f.unwrap();
        assert!(f.mirror && f.expanded > 0.99);
        assert!((f.width - 176.0).abs() < 0.5);
        assert!((f.left + f.width - 1920.0).abs() < 0.5);
    }
}
