//! Native, keyboard-accessible appearance controls with an immediate color preview.
use crate::{
    appearance::{
        color, snap_widget_size, Appearance, Mode, PRESETS, WIDGET_SIZE_MAX, WIDGET_SIZE_MIN,
        WIDGET_SIZE_STEP,
    },
    native_interop::{wide_str, Color},
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
const MODES: [&str; 4] = ["System", "Light", "Dark", "Custom"];
const FIELDS: [&str; 5] = ["Background", "Text", "Claude", "Codex", "Antigravity"];
const MODE_VALUES: [Mode; 4] = [Mode::System, Mode::Light, Mode::Dark, Mode::Custom];

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
        let mut bounds = rect(0, 0, 488, 680);
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
        for (i, text) in MODES.iter().enumerate() {
            button(
                hwnd,
                100 + i as u16,
                text,
                rect(24 + i as i32 * 112, 120, 104, 64),
            );
        }
        for (i, (name, _)) in PRESETS.iter().enumerate() {
            button(
                hwnd,
                110 + i as u16,
                name,
                rect(24 + i as i32 * 112, 346, 104, 54),
            );
        }
        for (i, text) in FIELDS.iter().enumerate() {
            button(
                hwnd,
                120 + i as u16,
                text,
                rect(24 + i as i32 * 90, 442, 80, 68),
            );
        }
        slider(hwnd, rect(112, 572, 288, 32));
        button(hwnd, 130, "Reset", rect(24, 626, 76, 32));
        button(hwnd, 131, "Undo changes", rect(110, 626, 124, 32));
        button(hwnd, 132, "Done", rect(356, 626, 108, 32));
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
        next.colors[p.index] = bgr_to_rgb(bgr);
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
    let mut appearance = original.clone();
    if appearance.mode != Mode::Custom {
        let palette = appearance.palette(appearance.is_dark(theme::is_dark_mode()));
        appearance.colors = palette.map(|c| (c.r as u32) << 16 | (c.g as u32) << 8 | c.b as u32);
    }
    let mut custom = [COLORREF(0); 16];
    for (i, (_, colors)) in PRESETS.iter().enumerate() {
        for j in 0..4 {
            custom[i * 4 + j] = COLORREF(color(colors[j]).to_colorref());
        }
    }
    let mut chooser = CHOOSECOLORW {
        lStructSize: std::mem::size_of::<CHOOSECOLORW>() as u32,
        hwndOwner: hwnd,
        rgbResult: COLORREF(color(appearance.colors[index]).to_colorref()),
        lpCustColors: custom.as_mut_ptr(),
        Flags: CC_FULLOPEN | CC_RGBINIT | CC_ENABLEHOOK,
        lpfnHook: Some(picker_hook),
        ..Default::default()
    };
    let mut base = appearance.clone();
    base.mode = Mode::Custom;
    *PICKER.lock().unwrap_or_else(|e| e.into_inner()) = Some(Picker {
        base,
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
        appearance.colors[index] = bgr_to_rgb(chooser.rgbResult.0);
        appearance.mode = Mode::Custom;
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
                100..=103 => {
                    appearance.mode = MODE_VALUES[(id - 100) as usize];
                    window::set_appearance(appearance);
                }
                110..=113 => {
                    appearance.mode = Mode::Custom;
                    appearance.colors = PRESETS[(id - 110) as usize].1;
                    window::set_appearance(appearance);
                }
                120..=124 => choose_color(hwnd, (id - 120) as usize),
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
                    let readout = rect(408, 572, 56, 32);
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
    draw_rounded_rect(dc, &rect(0, 0, 488, 680), &color(0x11151e), 0);
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
        "Your usage. Your colors. No restart.",
        0x9ca8bc,
        12,
        FW_NORMAL,
    );
    label(
        dc,
        rect(24, 94, 320, 18),
        "01  /  CHOOSE YOUR MODE",
        0x9ca8bc,
        10,
        FW_SEMIBOLD,
    );
    let appearance = window::appearance();
    let p = appearance.palette(appearance.is_dark(theme::is_dark_mode()));
    draw_rounded_rect(dc, &rect(24, 204, 440, 102), &p[0], sc(10));
    label(
        dc,
        rect(40, 214, 400, 16),
        "COLOR PREVIEW  /  sample usage",
        rgb(p[1]),
        9,
        FW_MEDIUM,
    );
    for (i, (name, usage, reset)) in [
        ("CLAUDE", "32%", "resets in 3h 12m"),
        ("CODEX", "58%", "resets in 4d 8h"),
    ]
    .iter()
    .enumerate()
    {
        let x = 40 + i as i32 * 214;
        label(
            dc,
            rect(x, 239, 180, 14),
            name,
            rgb(p[i + 2]),
            10,
            FW_SEMIBOLD,
        );
        label(dc, rect(x, 258, 64, 28), usage, rgb(p[1]), 24, FW_SEMIBOLD);
        label(
            dc,
            rect(x + 66, 263, 130, 20),
            reset,
            rgb(p[1]),
            10,
            FW_NORMAL,
        );
        draw_rounded_rect(dc, &rect(x, 292, 192, 2), &color(0x515966), sc(1));
        draw_rounded_rect(
            dc,
            &rect(x, 292, if i == 0 { 61 } else { 111 }, 2),
            &p[i + 2],
            sc(1),
        );
    }
    label(
        dc,
        rect(24, 319, 430, 18),
        "02  /  START WITH A PALETTE",
        0x9ca8bc,
        10,
        FW_SEMIBOLD,
    );
    label(
        dc,
        rect(24, 415, 440, 18),
        "03  /  MAKE IT YOURS",
        0x9ca8bc,
        10,
        FW_SEMIBOLD,
    );
    label(
        dc,
        rect(24, 516, 440, 18),
        "Click a color to mix your own. Warning colors stay meaningful.",
        0x8e9bae,
        10,
        FW_NORMAL,
    );
    label(
        dc,
        rect(24, 548, 440, 18),
        "04  /  WIDGET SIZE",
        0x9ca8bc,
        10,
        FW_SEMIBOLD,
    );
    label(
        dc,
        rect(24, 572, 86, 32),
        "Widget size",
        0xe8edf6,
        12,
        FW_SEMIBOLD,
    );
    label(
        dc,
        rect(408, 572, 56, 32),
        &format!("{}%", appearance.clamped_widget_size()),
        0xf2f4fa,
        12,
        FW_SEMIBOLD,
    );
    label(
        dc,
        rect(246, 633, 100, 18),
        "Saved as you go",
        0x9ca8bc,
        10,
        FW_NORMAL,
    );
}

fn rgb(c: Color) -> u32 {
    (c.r as u32) << 16 | (c.g as u32) << 8 | c.b as u32
}

unsafe fn draw_button(item: &DRAWITEMSTRUCT) {
    let a = window::appearance();
    let dark = a.is_dark(theme::is_dark_mode());
    let palette = a.palette(dark);
    let id = item.CtlID as u16;
    let r = item.rcItem;
    let dc = item.hDC;
    draw_rounded_rect(dc, &r, &color(0x11151e), 0);
    let selected = match id {
        100..=103 => a.mode == MODE_VALUES[(id - 100) as usize],
        110..=113 => a.mode == Mode::Custom && a.colors == PRESETS[(id - 110) as usize].1,
        _ => false,
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
    match id {
        100..=103 => {
            let i = (id - 100) as usize;
            let chip = match i {
                0 => [0xb9c4dd, 0xf3f3f3, 0x343b4d],
                1 => [0xf3f3f3, 0xb8c5df, 0xe3c4ad],
                2 => [0x161c28, 0x778aad, 0x434f66],
                _ => [rgb(palette[2]), rgb(palette[3]), rgb(palette[4])],
            };
            for (j, c) in chip.iter().enumerate() {
                draw_rounded_rect(
                    dc,
                    &local(12 + j as i32 * 18, 12, 13, 10),
                    &color(*c),
                    sc(3),
                );
            }
            label(
                dc,
                local(12, 34, 82, 20),
                MODES[i],
                0xe8edf6,
                12,
                FW_SEMIBOLD,
            );
            if selected {
                label(dc, local(80, 9, 16, 16), "✓", 0xa6b9ff, 12, FW_BOLD);
            }
        }
        110..=113 => {
            let (name, colors) = PRESETS[(id - 110) as usize];
            for j in 0..3 {
                draw_rounded_rect(
                    dc,
                    &local(12 + j * 22, 10, 18, 10),
                    &color(colors[(j + 2) as usize]),
                    sc(3),
                );
            }
            label(dc, local(12, 28, 88, 18), name, 0xe8edf6, 11, FW_MEDIUM);
        }
        120..=124 => {
            let i = (id - 120) as usize;
            draw_rounded_rect(dc, &local(10, 9, 60, 15), &palette[i], sc(3));
            label(dc, local(8, 29, 68, 16), FIELDS[i], 0xe8edf6, 10, FW_MEDIUM);
            label(
                dc,
                local(8, 46, 68, 14),
                &format!("#{:06X}", rgb(palette[i])),
                0x9ca8bc,
                9,
                FW_NORMAL,
            );
        }
        _ => label(
            dc,
            local(12, 0, 120, 32),
            match id {
                130 => "Reset",
                131 => "Undo changes",
                _ => "Done  →",
            },
            if id == 132 { 0xc0ceff } else { 0xe8edf6 },
            12,
            FW_SEMIBOLD,
        ),
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
