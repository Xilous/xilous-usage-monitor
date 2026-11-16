//! Native, keyboard-accessible appearance controls with an immediate color preview.
use crate::{
    appearance::{
        color, mix, preset, snap_widget_size, Appearance, Material, WarningColours, WindowColours,
        PRESETS, ROLE_NAMES, SYSTEM, WIDGET_SIZE_MAX, WIDGET_SIZE_MIN, WIDGET_SIZE_STEP,
    },
    native_interop::wide_str,
    theme, window,
};
use std::sync::{
    atomic::{AtomicIsize, Ordering},
    Mutex,
};
use window::{create_font, dpi_sc as sc, draw_rounded_rect, draw_text_in};
use windows::core::PCWSTR;
use windows::Win32::{
    Foundation::*,
    Graphics::Gdi::*,
    System::LibraryLoader::GetModuleHandleW,
    UI::{Controls::Dialogs::*, Controls::*, WindowsAndMessaging::*},
};

static STUDIO: AtomicIsize = AtomicIsize::new(0);
static ORIGINAL: Mutex<Option<Appearance>> = Mutex::new(None);
static PICKER: Mutex<Option<Picker>> = Mutex::new(None);
static SLIDER: AtomicIsize = AtomicIsize::new(0);
static SLIDER_BRUSH: AtomicIsize = AtomicIsize::new(0);
const SLIDER_ID: u16 = 140;
/// Not exported by the windows crate; TBM_GETPOS is WM_USER.
const TBM_GETPOS: u32 = WM_USER;
/// Control ids: the themes (System, then each preset), the colour roles, and
/// the three look options.
const THEME_ID: u16 = 200;
const ROLE_ID: u16 = 300;
const WINDOWS_ID: u16 = 400;
const WARNINGS_ID: u16 = 410;
const MATERIAL_ID: u16 = 420;
const THEME_COUNT: u16 = 1 + PRESETS.len() as u16;
const WINDOW_OPTIONS: [(WindowColours, &str); 3] = [
    (WindowColours::Own, "Own colour each"),
    (WindowColours::Shaded, "One colour, shaded"),
    (WindowColours::Mono, "Monochrome"),
];
const WARNING_OPTIONS: [(WarningColours, &str); 3] = [
    (WarningColours::Theme, "Theme"),
    (WarningColours::ColourBlind, "Colour-blind safe"),
    (WarningColours::Hatching, "Hatching only"),
];
const MATERIAL_OPTIONS: [(Material, &str); 2] = [
    (Material::Solid, "Solid"),
    (Material::Floating, "Floating rings"),
];
const WIDTH: i32 = 520;
const HEIGHT: i32 = 748;
/// The rows of the look options, and where their buttons start.
const LOOK_Y: [i32; 3] = [506, 542, 578];
const LOOK_X: i32 = 160;

/// The theme id of theme button `i`: System, then the presets in order.
fn theme_id(i: u16) -> &'static str {
    if i == 0 {
        SYSTEM
    } else {
        PRESETS[(i - 1) as usize].id
    }
}

fn theme_name(i: u16) -> &'static str {
    if i == 0 {
        "System"
    } else {
        PRESETS[(i - 1) as usize].name
    }
}

fn rect(x: i32, y: i32, w: i32, h: i32) -> RECT {
    RECT {
        left: sc(x),
        top: sc(y),
        right: sc(x + w),
        bottom: sc(y + h),
    }
}

fn label(dc: HDC, r: RECT, value: &str, rgb: u32, size: i32, weight: FONT_WEIGHT) {
    unsafe {
        let _ = SetBkMode(dc, TRANSPARENT);
        let font = create_font(sc(-size), weight);
        let old = SelectObject(dc, font);
        draw_text_in(
            dc,
            r,
            value,
            &color(rgb),
            DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
        );
        SelectObject(dc, old);
        let _ = DeleteObject(font);
    }
}

pub fn open(owner: HWND) {
    let existing = STUDIO.load(Ordering::Relaxed);
    unsafe {
        if existing != 0 {
            let hwnd = HWND(existing as *mut _);
            let _ = ShowWindow(hwnd, SW_SHOW);
            let _ = SetForegroundWindow(hwnd);
            return;
        }
        *ORIGINAL.lock().unwrap_or_else(|e| e.into_inner()) = Some(window::appearance());
        let instance = GetModuleHandleW(None).unwrap();
        let class = wide_str("XilousAppearanceStudio");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(proc),
            hInstance: instance.into(),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            lpszClassName: PCWSTR(class.as_ptr()),
            ..Default::default()
        };
        let _ = RegisterClassW(&wc);
        let title = wide_str("Appearance Studio");
        let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_CLIPCHILDREN;
        let mut bounds = rect(0, 0, WIDTH, HEIGHT);
        let _ = AdjustWindowRectEx(&mut bounds, style, false, WS_EX_TOOLWINDOW);
        let mut point = POINT::default();
        let _ = GetCursorPos(&mut point);
        let monitor = MonitorFromPoint(point, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        let _ = GetMonitorInfoW(monitor, &mut info);
        let w = bounds.right - bounds.left;
        let h = bounds.bottom - bounds.top;
        let hwnd = match CreateWindowExW(
            WS_EX_TOOLWINDOW,
            PCWSTR(class.as_ptr()),
            PCWSTR(title.as_ptr()),
            style,
            info.rcWork.left + (info.rcWork.right - info.rcWork.left - w).max(0) / 2,
            info.rcWork.top + (info.rcWork.bottom - info.rcWork.top - h).max(0) / 2,
            w,
            h,
            owner,
            None,
            instance,
            None,
        ) {
            Ok(hwnd) => hwnd,
            Err(_) => return,
        };
        STUDIO.store(hwnd.0 as isize, Ordering::Relaxed);
        for i in 0..THEME_COUNT {
            let (col, row) = (i as i32 % 4, i as i32 / 4);
            button(
                hwnd,
                THEME_ID + i,
                theme_name(i),
                rect(24 + col * 120, 118 + row * 58, 112, 50),
            );
        }
        for (i, name) in ROLE_NAMES.iter().enumerate() {
            let (col, row) = (i as i32 % 5, i as i32 / 5);
            button(
                hwnd,
                ROLE_ID + i as u16,
                name,
                rect(24 + col * 96, 324 + row * 68, 88, 60),
            );
        }
        let segments: [(u16, Vec<&str>); 3] = [
            (WINDOWS_ID, WINDOW_OPTIONS.iter().map(|o| o.1).collect()),
            (WARNINGS_ID, WARNING_OPTIONS.iter().map(|o| o.1).collect()),
            (MATERIAL_ID, MATERIAL_OPTIONS.iter().map(|o| o.1).collect()),
        ];
        for (row, (base, names)) in segments.iter().enumerate() {
            let span = WIDTH - 24 - LOOK_X;
            let w = (span - 6 * (names.len() as i32 - 1)) / names.len() as i32;
            for (i, name) in names.iter().enumerate() {
                button(
                    hwnd,
                    base + i as u16,
                    name,
                    rect(LOOK_X + i as i32 * (w + 6), LOOK_Y[row], w, 30),
                );
            }
        }
        slider(hwnd, rect(112, 646, 320, 32));
        button(hwnd, 130, "Reset", rect(24, 700, 76, 32));
        button(hwnd, 131, "Undo changes", rect(110, 700, 124, 32));
        button(hwnd, 132, "Done", rect(WIDTH - 132, 700, 108, 32));
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
        let _ = SendMessageW(hwnd, WM_NEXTDLGCTL, WPARAM(0), LPARAM(0));
    }
}

unsafe fn button(parent: HWND, id: u16, text: &str, r: RECT) {
    let class = wide_str("BUTTON");
    let name = wide_str(text);
    let _ = CreateWindowExW(
        WINDOW_EX_STYLE(0),
        PCWSTR(class.as_ptr()),
        PCWSTR(name.as_ptr()),
        WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(BS_OWNERDRAW as u32),
        r.left,
        r.top,
        r.right - r.left,
        r.bottom - r.top,
        parent,
        HMENU(id as usize as *mut _),
        GetModuleHandleW(None).unwrap(),
        None,
    );
}

/// The WIDGET SIZE trackbar: 75% to 200% in 5% steps from the arrow keys and
/// PageUp/PageDown, with a tick every 25%.
unsafe fn slider(parent: HWND, r: RECT) {
    let controls = INITCOMMONCONTROLSEX {
        dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32,
        dwICC: ICC_BAR_CLASSES,
    };
    let _ = InitCommonControlsEx(&controls);
    let name = wide_str("Widget size");
    let Ok(hwnd) = CreateWindowExW(
        WINDOW_EX_STYLE(0),
        TRACKBAR_CLASSW,
        PCWSTR(name.as_ptr()),
        WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(TBS_HORZ | TBS_BOTTOM | TBS_AUTOTICKS),
        r.left,
        r.top,
        r.right - r.left,
        r.bottom - r.top,
        parent,
        HMENU(SLIDER_ID as usize as *mut _),
        GetModuleHandleW(None).unwrap(),
        None,
    ) else {
        return;
    };
    let range = (WIDGET_SIZE_MAX << 16 | WIDGET_SIZE_MIN) as isize;
    let _ = SendMessageW(hwnd, TBM_SETRANGE, WPARAM(0), LPARAM(range));
    let step = LPARAM(WIDGET_SIZE_STEP as isize);
    let _ = SendMessageW(hwnd, TBM_SETLINESIZE, WPARAM(0), step);
    let _ = SendMessageW(hwnd, TBM_SETPAGESIZE, WPARAM(0), step);
    let _ = SendMessageW(hwnd, TBM_SETTICFREQ, WPARAM(25), LPARAM(0));
    SLIDER.store(hwnd.0 as isize, Ordering::Relaxed);
    sync_slider();
}

/// Move the thumb to the live WIDGET SIZE, e.g. after Reset or Undo changes.
/// TBM_SETPOS sends no WM_HSCROLL, so this never feeds back into a change.
unsafe fn sync_slider() {
    let slider = SLIDER.load(Ordering::Relaxed);
    if slider != 0 {
        let position = LPARAM(window::appearance().clamped_widget_size() as isize);
        let _ = SendMessageW(HWND(slider as *mut _), TBM_SETPOS, WPARAM(1), position);
    }
}

pub fn translate(msg: &MSG) -> bool {
    let h = STUDIO.load(Ordering::Relaxed);
    if h == 0 {
        return false;
    }
    unsafe {
        let hwnd = HWND(h as *mut _);
        if msg.message == WM_KEYDOWN
            && msg.wParam.0 == 27
            && (msg.hwnd == hwnd || IsChild(hwnd, msg.hwnd).as_bool())
        {
            let _ = DestroyWindow(hwnd);
            return true;
        }
        IsDialogMessageW(hwnd, msg).as_bool()
    }
}

struct Picker {
    base: Appearance,
    index: usize,
    last: u32,
    ready: bool,
}

fn bgr_to_rgb(bgr: u32) -> u32 {
    (bgr & 255) << 16 | (bgr & 0xff00) | ((bgr >> 16) & 255)
}

// The full-open dialog's H/S/L edits, spectrum, and swatches all rewrite its RGB edits,
// control IDs 706-708 (COLOR_RED..COLOR_BLUE in colordlg.h).
unsafe extern "system" fn picker_hook(dialog: HWND, msg: u32, wp: WPARAM, _lp: LPARAM) -> usize {
    match msg {
        WM_INITDIALOG => {
            if let Some(p) = PICKER.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
                p.ready = true;
            }
            1
        }
        WM_COMMAND
            if (wp.0 >> 16) & 0xffff == EN_CHANGE as usize
                && (706..=708).contains(&(wp.0 & 0xffff)) =>
        {
            preview_picked(dialog);
            0
        }
        _ => 0,
    }
}

unsafe fn preview_picked(dialog: HWND) {
    let mut bgr = 0;
    for (i, id) in (706..=708).enumerate() {
        let mut translated = BOOL(0);
        let value = GetDlgItemInt(dialog, id, Some(&mut translated as *mut BOOL), false);
        if !translated.as_bool() {
            return;
        }
        bgr |= value.min(255) << (i * 8);
    }
    let next = {
        let mut picker = PICKER.lock().unwrap_or_else(|e| e.into_inner());
        let p = match picker.as_mut() {
            Some(p) if p.ready && p.last != bgr => p,
            _ => return,
        };
        p.last = bgr;
        let mut next = p.base.clone();
        next.set_role(theme::is_dark_mode(), p.index, bgr_to_rgb(bgr));
        next
    };
    window::preview_appearance(next);
    let studio = STUDIO.load(Ordering::Relaxed);
    if studio != 0 {
        let _ = RedrawWindow(
            HWND(studio as *mut _),
            None,
            None,
            RDW_INVALIDATE | RDW_ALLCHILDREN,
        );
    }
}

unsafe fn choose_color(hwnd: HWND, index: usize) {
    let original = window::appearance();
    let system_dark = theme::is_dark_mode();
    let current = original.roles_for(system_dark).get(index);
    // The custom swatches offer the presets' window colours to start from.
    let mut custom = [COLORREF(0); 16];
    let offered = PRESETS
        .iter()
        .flat_map(|p| [p.roles.weekly, p.roles.fable])
        .take(16);
    for (slot, rgb) in custom.iter_mut().zip(offered) {
        *slot = COLORREF(color(rgb).to_colorref());
    }
    let mut chooser = CHOOSECOLORW {
        lStructSize: std::mem::size_of::<CHOOSECOLORW>() as u32,
        hwndOwner: hwnd,
        rgbResult: COLORREF(color(current).to_colorref()),
        lpCustColors: custom.as_mut_ptr(),
        Flags: CC_FULLOPEN | CC_RGBINIT | CC_ENABLEHOOK,
        lpfnHook: Some(picker_hook),
        ..Default::default()
    };
    *PICKER.lock().unwrap_or_else(|e| e.into_inner()) = Some(Picker {
        base: original.clone(),
        index,
        last: chooser.rgbResult.0,
        ready: false,
    });
    // No application locks held across the modal dialog's nested message loop.
    let accepted = ChooseColorW(&mut chooser).as_bool();
    *PICKER.lock().unwrap_or_else(|e| e.into_inner()) = None;
    crate::diagnose::log(format!(
        "appearance picker accepted={accepted} color={:06X}",
        chooser.rgbResult.0
    ));
    if accepted {
        let mut appearance = original;
        appearance.set_role(system_dark, index, bgr_to_rgb(chooser.rgbResult.0));
        window::set_appearance(appearance);
    } else {
        // Re-save: another path (a drag, an update check) may have written the preview meanwhile.
        window::set_appearance(original);
    }
}

unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_SETTINGCHANGE => {
            let _ = RedrawWindow(hwnd, None, None, RDW_INVALIDATE | RDW_ALLCHILDREN);
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let dc = BeginPaint(hwnd, &mut ps);
            paint(dc);
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        WM_DRAWITEM => {
            if lp.0 != 0 {
                draw_button(&*(lp.0 as *const DRAWITEMSTRUCT));
            }
            LRESULT(1)
        }
        WM_COMMAND => {
            let id = (wp.0 & 0xffff) as u16;
            let mut appearance = window::appearance();
            match id {
                id if (THEME_ID..THEME_ID + THEME_COUNT).contains(&id) => {
                    appearance.use_theme(theme_id(id - THEME_ID));
                    window::set_appearance(appearance);
                }
                id if (ROLE_ID..ROLE_ID + ROLE_NAMES.len() as u16).contains(&id) => {
                    choose_color(hwnd, (id - ROLE_ID) as usize)
                }
                id if (WINDOWS_ID..WINDOWS_ID + 3).contains(&id) => {
                    appearance.window_colours = WINDOW_OPTIONS[(id - WINDOWS_ID) as usize].0;
                    window::set_appearance(appearance);
                }
                id if (WARNINGS_ID..WARNINGS_ID + 3).contains(&id) => {
                    appearance.warning_colours = WARNING_OPTIONS[(id - WARNINGS_ID) as usize].0;
                    window::set_appearance(appearance);
                }
                id if (MATERIAL_ID..MATERIAL_ID + 2).contains(&id) => {
                    appearance.material = MATERIAL_OPTIONS[(id - MATERIAL_ID) as usize].0;
                    window::set_appearance(appearance);
                }
                130 => window::set_appearance(Appearance::default()),
                131 => {
                    let original = ORIGINAL.lock().unwrap_or_else(|e| e.into_inner()).clone();
                    if let Some(original) = original {
                        window::set_appearance(original);
                    }
                }
                132 | 2 => {
                    let _ = DestroyWindow(hwnd);
                    return LRESULT(0);
                }
                _ => {}
            }
            sync_slider();
            let _ = RedrawWindow(hwnd, None, None, RDW_INVALIDATE | RDW_ALLCHILDREN);
            LRESULT(0)
        }
        // Every step while dragging resizes the real widget, not just on release.
        WM_HSCROLL => {
            let slider = SLIDER.load(Ordering::Relaxed);
            if slider != 0 && lp.0 == slider {
                let slider = HWND(slider as *mut _);
                let position = SendMessageW(slider, TBM_GETPOS, WPARAM(0), LPARAM(0)).0;
                let widget_size = snap_widget_size(position as f64);
                if position != widget_size as isize {
                    let snapped = LPARAM(widget_size as isize);
                    let _ = SendMessageW(slider, TBM_SETPOS, WPARAM(1), snapped);
                }
                let mut appearance = window::appearance();
                if appearance.widget_size != widget_size {
                    appearance.widget_size = widget_size;
                    window::set_appearance(appearance);
                    let readout = rect(440, 646, 56, 32);
                    let _ = InvalidateRect(hwnd, Some(&readout as *const _), false);
                }
            }
            LRESULT(0)
        }
        // The trackbar asks its parent for the brush behind its channel.
        WM_CTLCOLORSTATIC => {
            let dc = HDC(wp.0 as *mut _);
            let _ = SetBkColor(dc, COLORREF(color(0x11151e).to_colorref()));
            let _ = SetTextColor(dc, COLORREF(color(0x9ca8bc).to_colorref()));
            let mut brush = SLIDER_BRUSH.load(Ordering::Relaxed);
            if brush == 0 {
                brush = CreateSolidBrush(COLORREF(color(0x11151e).to_colorref())).0 as isize;
                SLIDER_BRUSH.store(brush, Ordering::Relaxed);
            }
            LRESULT(brush)
        }
        WM_CLOSE => {
            let _ = DestroyWindow(hwnd);
            LRESULT(0)
        }
        WM_DESTROY => {
            STUDIO.store(0, Ordering::Relaxed);
            SLIDER.store(0, Ordering::Relaxed);
            let brush = SLIDER_BRUSH.swap(0, Ordering::Relaxed);
            if brush != 0 {
                let _ = DeleteObject(HBRUSH(brush as *mut _));
            }
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

fn paint(dc: HDC) {
    draw_rounded_rect(dc, &rect(0, 0, WIDTH, HEIGHT), &color(0x11151e), 0);
    label(
        dc,
        rect(24, 20, 440, 36),
        "A little more you.",
        0xf2f4fa,
        28,
        FW_SEMIBOLD,
    );
    label(
        dc,
        rect(24, 61, 440, 20),
        "Your usage. Your colours. No restart.",
        0x9ca8bc,
        12,
        FW_NORMAL,
    );
    let heading =
        |y: i32, text: &str| label(dc, rect(24, y, 440, 18), text, 0x9ca8bc, 10, FW_SEMIBOLD);
    heading(94, "01  /  CHOOSE A THEME");
    heading(300, "02  /  MAKE IT YOURS");
    label(
        dc,
        rect(24, 458, 472, 18),
        "Click a colour to mix your own. Dimmed colours are not in use with the options below.",
        0x8e9bae,
        10,
        FW_NORMAL,
    );
    heading(482, "03  /  HOW IT READS");
    for (y, name) in LOOK_Y
        .iter()
        .zip(["Window colours", "Warning colours", "Background"])
    {
        label(
            dc,
            rect(24, *y, LOOK_X - 30, 30),
            name,
            0xe8edf6,
            11,
            FW_MEDIUM,
        );
    }
    heading(622, "04  /  WIDGET SIZE");
    label(
        dc,
        rect(24, 646, 86, 32),
        "Widget size",
        0xe8edf6,
        12,
        FW_SEMIBOLD,
    );
    label(
        dc,
        rect(440, 646, 56, 32),
        &format!("{}%", window::appearance().clamped_widget_size()),
        0xf2f4fa,
        12,
        FW_SEMIBOLD,
    );
    label(
        dc,
        rect(246, 707, 120, 18),
        "Saved as you go",
        0x9ca8bc,
        10,
        FW_NORMAL,
    );
}

/// Whether colour role `i` shows on the widget with these options.
fn role_in_use(a: &Appearance, i: usize) -> bool {
    match i {
        2..=6 if a.window_colours == WindowColours::Mono => false,
        2 | 4 if a.window_colours == WindowColours::Shaded => false,
        7 | 8 => a.warning_colours == WarningColours::Theme,
        _ => true,
    }
}

unsafe fn draw_button(item: &DRAWITEMSTRUCT) {
    let a = window::appearance();
    let roles = a.roles_for(theme::is_dark_mode());
    let id = item.CtlID as u16;
    let r = item.rcItem;
    let dc = item.hDC;
    draw_rounded_rect(dc, &r, &color(0x11151e), 0);
    let is_theme = (THEME_ID..THEME_ID + THEME_COUNT).contains(&id);
    let is_role = (ROLE_ID..ROLE_ID + ROLE_NAMES.len() as u16).contains(&id);
    let option_name = match id {
        id if (WINDOWS_ID..WINDOWS_ID + 3).contains(&id) => {
            let (v, name) = WINDOW_OPTIONS[(id - WINDOWS_ID) as usize];
            Some((a.window_colours == v, name))
        }
        id if (WARNINGS_ID..WARNINGS_ID + 3).contains(&id) => {
            let (v, name) = WARNING_OPTIONS[(id - WARNINGS_ID) as usize];
            Some((a.warning_colours == v, name))
        }
        id if (MATERIAL_ID..MATERIAL_ID + 2).contains(&id) => {
            let (v, name) = MATERIAL_OPTIONS[(id - MATERIAL_ID) as usize];
            Some((a.material == v, name))
        }
        _ => None,
    };
    let selected = if is_theme {
        a.theme == theme_id(id - THEME_ID)
    } else {
        option_name.is_some_and(|(on, _)| on)
    };
    let focus = item.itemState.0 & ODS_FOCUS.0 != 0;
    let pressed = item.itemState.0 & ODS_SELECTED.0 != 0;
    draw_rounded_rect(
        dc,
        &r,
        &color(if selected || focus {
            0xa6b9ff
        } else {
            0x343c4d
        }),
        sc(7),
    );
    let inner = RECT {
        left: r.left + sc(1),
        top: r.top + sc(1),
        right: r.right - sc(1),
        bottom: r.bottom - sc(1),
    };
    draw_rounded_rect(
        dc,
        &inner,
        &color(if pressed { 0x343e57 } else { 0x202735 }),
        sc(6),
    );
    let local = |x: i32, y: i32, w: i32, h: i32| RECT {
        left: r.left + sc(x),
        top: r.top + sc(y),
        right: r.left + sc(x + w),
        bottom: r.top + sc(y + h),
    };
    if is_theme {
        let i = id - THEME_ID;
        let chips = if i == 0 {
            let (d, l) = (PRESETS[0].roles, PRESETS[1].roles);
            [d.background, d.weekly, l.background, l.weekly]
        } else {
            let p = preset(theme_id(i)).map(|p| p.roles).unwrap_or(roles);
            [p.background, p.weekly, p.fable, p.cpu]
        };
        for (j, c) in chips.iter().enumerate() {
            draw_rounded_rect(dc, &local(10 + j as i32 * 18, 9, 14, 10), &color(*c), sc(3));
        }
        label(
            dc,
            local(10, 25, 96, 18),
            theme_name(i),
            0xe8edf6,
            11,
            FW_MEDIUM,
        );
        if selected {
            label(dc, local(90, 6, 16, 16), "\u{2713}", 0xa6b9ff, 12, FW_BOLD);
        }
    } else if is_role {
        let i = (id - ROLE_ID) as usize;
        let used = role_in_use(&a, i);
        let rgb = roles.get(i);
        let swatch = if used { rgb } else { mix(rgb, 0x202735, 0.6) };
        draw_rounded_rect(dc, &local(10, 9, 68, 15), &color(swatch), sc(3));
        label(
            dc,
            local(8, 29, 76, 16),
            ROLE_NAMES[i],
            if used { 0xe8edf6 } else { 0x6b7486 },
            10,
            FW_MEDIUM,
        );
        label(
            dc,
            local(8, 44, 76, 14),
            &format!("#{:06X}", rgb),
            if used { 0x9ca8bc } else { 0x5b6375 },
            9,
            FW_NORMAL,
        );
    } else if let Some((_, name)) = option_name {
        let _ = SetBkMode(dc, TRANSPARENT);
        let font = create_font(sc(-11), if selected { FW_SEMIBOLD } else { FW_MEDIUM });
        let old = SelectObject(dc, font);
        draw_text_in(
            dc,
            inner,
            name,
            &color(if selected { 0xc0ceff } else { 0xe8edf6 }),
            DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
        );
        SelectObject(dc, old);
        let _ = DeleteObject(font);
    } else {
        label(
            dc,
            local(12, 0, 120, 32),
            match id {
                130 => "Reset",
                131 => "Undo changes",
                _ => "Done  \u{2192}",
            },
            if id == 132 { 0xc0ceff } else { 0xe8edf6 },
            12,
            FW_SEMIBOLD,
        );
    }
    if focus {
        let focus_rect = RECT {
            left: r.left + sc(4),
            top: r.top + sc(4),
            right: r.right - sc(4),
            bottom: r.bottom - sc(4),
        };
        let _ = DrawFocusRect(dc, &focus_rect);
    }
}
