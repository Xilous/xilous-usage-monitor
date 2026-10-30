use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use windows::core::PCWSTR;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::{GetModuleFileNameW, GetModuleHandleW};
use windows::Win32::System::Registry::*;
use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
use windows::Win32::System::Threading::{CreateMutexW, GetSystemTimes, WaitForSingleObject};
use windows::Win32::UI::Accessibility::HWINEVENTHOOK;
use windows::Win32::UI::HiDpi::*;
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture};
use windows::Win32::UI::Shell::ExtractIconExW;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::appearance::{Appearance, Mode, WIDGET_SIZE_DEFAULT};
use crate::diagnose;
use crate::dock;
use crate::localization::{self, Strings};
use crate::models::AppUsageData;
use crate::native_interop::{
    self, Color, TIMER_ANIM, TIMER_COUNTDOWN, TIMER_MOTION, TIMER_POLL, TIMER_RAM,
    TIMER_RESET_POLL, TIMER_TOPMOST, TIMER_UPDATE_CHECK, WM_APP_TRAY, WM_APP_USAGE_UPDATED,
};
use crate::poller;
use crate::readout;
use crate::theme;
use crate::tray_icon;
use crate::updater::{self, InstallChannel, ReleaseDescriptor, UpdateCheckResult};

/// Wrapper to make HWND sendable across threads (safe for PostMessage usage)
#[derive(Clone, Copy)]
struct SendHwnd(isize);

unsafe impl Send for SendHwnd {}

impl SendHwnd {
    fn from_hwnd(hwnd: HWND) -> Self {
        Self(hwnd.0 as isize)
    }
    fn to_hwnd(self) -> HWND {
        HWND(self.0 as *mut _)
    }
}

/// Shared application state
struct AppState {
    hwnd: SendHwnd,
    taskbar_hwnd: Option<HWND>,
    tray_notify_hwnd: Option<HWND>,
    win_event_hook: Option<HWINEVENTHOOK>,
    foreground_hook: Option<HWINEVENTHOOK>,
    is_dark: bool,
    appearance: Appearance,
    embedded: bool,
    install_channel: InstallChannel,

    session_percent: f64,
    session_text: String,
    weekly_percent: f64,
    weekly_text: String,
    codex_session_percent: f64,
    codex_session_text: String,
    codex_weekly_percent: f64,
    codex_weekly_text: String,
    antigravity_session_percent: f64,
    antigravity_session_text: String,
    antigravity_weekly_percent: f64,
    antigravity_weekly_text: String,
    show_claude_code: bool,
    show_codex: bool,
    show_antigravity: bool,

    data: Option<AppUsageData>,

    poll_interval_ms: u32,
    retry_count: u32,
    force_notify_auth_error: bool,
    auth_error_paused_polling: bool,
    auth_watch_mode: poller::CredentialWatchMode,
    auth_watch_snapshot: poller::CredentialWatchSnapshot,
    last_poll_ok: bool,
    update_status: UpdateStatus,
    last_update_check_unix: Option<u64>,

    taskbar_index: usize,
    tray_offset: i32,
    dragging: bool,
    floating_position: Option<(i32, i32)>,
    dock_anchor: Option<(i32, i32)>,
    drag_start_mouse_x: i32,
    drag_start_client_x: i32,
    drag_start_offset: i32,

    widget_visible: bool,
    embed_in_taskbar: bool,
    click_through: bool,
}

#[derive(Clone, Debug)]
enum UpdateStatus {
    Idle,
    Checking,
    Applying,
    UpToDate,
    Available(ReleaseDescriptor),
}

const RETRY_BASE_MS: u32 = 30_000; // 30 seconds

const POLL_1_MIN: u32 = 60_000;
const POLL_5_MIN: u32 = 300_000;
const POLL_15_MIN: u32 = 900_000;
const POLL_1_HOUR: u32 = 3_600_000;

// Menu item IDs for update frequency
const IDM_FREQ_1MIN: u16 = 10;
const IDM_FREQ_5MIN: u16 = 11;
const IDM_FREQ_15MIN: u16 = 12;
const IDM_FREQ_1HOUR: u16 = 13;
const IDM_START_WITH_WINDOWS: u16 = 20;
const IDM_RESET_POSITION: u16 = 30;
const IDM_VERSION_ACTION: u16 = 31;
const IDM_APPEARANCE: u16 = 80;

const WM_DPICHANGED_MSG: u32 = 0x02E0;
const WM_APP_UPDATE_CHECK_COMPLETE: u32 = WM_APP + 2;
const TRAY_ICON_UPDATE_REPOSITION_SUPPRESS_MS: u64 = 750;

/// How often the watchdog thread polls for an explorer.exe restart (which
/// recreates the taskbar and wipes our tray-icon registration).
const TASKBAR_WATCH_INTERVAL_SECS: u64 = 2;

static SUPPRESS_TRAY_REPOSITION_UNTIL: Mutex<Option<Instant>> = Mutex::new(None);

/// True while the context menu is open; the topmost re-assert waits for it.
static MENU_OPEN: AtomicBool = AtomicBool::new(false);

/// Current system DPI (96 = 100% scaling, 144 = 150%, 192 = 200%, etc.)
static CURRENT_DPI: AtomicU32 = AtomicU32::new(96);

/// WIDGET SIZE in whole percent, mirrored from the appearance so the widget's
/// geometry can be scaled without taking the state lock. Anything that never
/// sets it, such as `write_preview`, renders at 100%.
static WIDGET_SIZE: AtomicU32 = AtomicU32::new(WIDGET_SIZE_DEFAULT);

/// Scale a base pixel value (designed at 96 DPI and 100% WIDGET SIZE).
fn scale_px(px: i32, dpi: u32, widget_size: u32) -> i32 {
    (px as f64 * dpi as f64 / 96.0 * widget_size as f64 / 100.0).round() as i32
}

/// Scale a base pixel value to the current DPI only. For everything that is
/// not the floating widget (the appearance studio), which WIDGET SIZE leaves
/// alone.
pub(crate) fn dpi_sc(px: i32) -> i32 {
    scale_px(px, CURRENT_DPI.load(Ordering::Relaxed), WIDGET_SIZE_DEFAULT)
}

/// Scale a base pixel value of the widget to the current DPI and WIDGET SIZE.
/// Every widget size, gap, font and hit-test goes through this.
fn widget_sc(px: i32) -> i32 {
    scale_px(
        px,
        CURRENT_DPI.load(Ordering::Relaxed),
        WIDGET_SIZE.load(Ordering::Relaxed),
    )
}

/// Re-query the monitor DPI for our window and update the cached value.
/// Uses GetDpiForWindow which returns the live DPI (unlike GetDpiForSystem
/// which is cached at process startup and never changes).
fn refresh_dpi() {
    let hwnd = {
        let state = lock_state();
        state.as_ref().map(|s| s.hwnd.to_hwnd())
    };
    if let Some(hwnd) = hwnd {
        let dpi = unsafe { GetDpiForWindow(hwnd) };
        if dpi > 0 {
            CURRENT_DPI.store(dpi, Ordering::Relaxed);
        }
    }
}

/// Spacing below which two relaunches are treated as a storm (e.g. explorer.exe
/// crash-looping); when detected we back off instead of spawning in a tight loop.
const RELAUNCH_THROTTLE_SECS: u64 = 10;
const RELAUNCH_BACKOFF_SECS: u64 = 30;
/// Environment flag set on a relaunched child so it waits for the previous
/// instance's single-instance mutex instead of exiting immediately.
const ENV_RELAUNCH: &str = "CCUM_RELAUNCH";
/// Unix timestamp (seconds) of the relaunch that spawned this process, passed to
/// the child so it can detect a relaunch storm.
const ENV_LAST_RELAUNCH_UNIX: &str = "CCUM_LAST_RELAUNCH_UNIX";

/// Relaunch the widget as a fresh process after explorer.exe has restarted.
///
/// When the shell restarts it destroys our embedded child window outright (the
/// window is gone, not merely orphaned - `IsWindow` returns false) and leaves
/// the UI thread parked in `GetMessage` with no window to recreate in place.
/// Spawning a clean new process - which re-embeds into the freshly created
/// taskbar - and exiting this one is the robust recovery. The child is flagged
/// via `ENV_RELAUNCH` so it waits for this instance's single-instance mutex to
/// be released before taking over (see the guard in `run`).
fn relaunch_self() {
    // Back off if we are relaunching very soon after the relaunch that spawned
    // us: that signals the shell is crash-looping, not a one-off restart.
    let now = now_unix_secs();
    let last = std::env::var(ENV_LAST_RELAUNCH_UNIX)
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0);
    if last != 0 && now.saturating_sub(last) < RELAUNCH_THROTTLE_SECS {
        diagnose::log("relaunch storm detected; backing off before relaunching");
        std::thread::sleep(Duration::from_secs(RELAUNCH_BACKOFF_SECS));
    }

    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(error) => {
            diagnose::log_error("watchdog: unable to resolve current executable", error);
            return;
        }
    };

    let args: Vec<String> = std::env::args().skip(1).collect();
    match std::process::Command::new(exe)
        .args(&args)
        .env(ENV_RELAUNCH, "1")
        .env(ENV_LAST_RELAUNCH_UNIX, now.to_string())
        .spawn()
    {
        Ok(_) => {
            diagnose::log("watchdog: relaunched fresh instance, exiting old one");
            std::process::exit(0);
        }
        Err(error) => {
            diagnose::log_error("watchdog: unable to spawn relaunched instance", error);
        }
    }
}

/// Detect explorer.exe restarts and recover from them.
///
/// Once explorer destroys the taskbar, our embedded child window is destroyed
/// and the UI message loop is dead, so recovery cannot happen in-process. This
/// dedicated thread (independent of the dead message loop) polls the taskbar
/// handle and, when it changes, relaunches the widget as a fresh process.
fn spawn_taskbar_watchdog() {
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_secs(TASKBAR_WATCH_INTERVAL_SECS));
        let stored = {
            let state = lock_state();
            state.as_ref().and_then(|s| s.taskbar_hwnd)
        };
        // Only relevant once we have embedded into a taskbar at least once.
        let Some(old) = stored else {
            continue;
        };
        let taskbars = native_interop::find_taskbars();
        if !taskbars.is_empty() && !taskbars.iter().any(|taskbar| taskbar.hwnd == old) {
            let new = taskbars[0].hwnd;
            diagnose::log(format!(
                "watchdog: taskbar changed old={:?} new={:?} -> relaunching",
                old.0, new.0
            ));
            relaunch_self();
        }
    });
}

fn load_embedded_app_icons() -> (HICON, HICON) {
    unsafe {
        let mut exe_buf = [0u16; 260];
        let len = GetModuleFileNameW(None, &mut exe_buf) as usize;
        if len == 0 {
            return (HICON::default(), HICON::default());
        }

        let mut large_icon = HICON::default();
        let mut small_icon = HICON::default();
        let extracted = ExtractIconExW(
            PCWSTR::from_raw(exe_buf.as_ptr()),
            0,
            Some(&mut large_icon),
            Some(&mut small_icon),
            1,
        );

        if extracted == 0 {
            (HICON::default(), HICON::default())
        } else {
            (large_icon, small_icon)
        }
    }
}

unsafe impl Send for AppState {}

static STATE: Mutex<Option<AppState>> = Mutex::new(None);

/// Lock STATE safely, recovering from poisoned mutex
fn lock_state() -> MutexGuard<'static, Option<AppState>> {
    STATE.lock().unwrap_or_else(|e| e.into_inner())
}

/// The app's name before it became Xilous Usage Monitor. Its settings folder
/// and start-with-Windows value are carried over once by the migrations below.
const LEGACY_APP_NAME: &str = "StealthyUsageMonitor";

fn settings_path() -> PathBuf {
    settings_path_in("XilousUsageMonitor")
}

fn settings_path_in(app_dir: &str) -> PathBuf {
    let appdata = std::env::var("APPDATA").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(appdata).join(app_dir).join("settings.json")
}

/// Copies settings saved under the legacy name, unless settings already exist
/// under the current one, so the rename keeps the user's appearance, position
/// and provider choices.
fn migrate_legacy_settings() {
    let path = settings_path();
    let legacy = settings_path_in(LEGACY_APP_NAME);
    if path.exists() || !legacy.exists() {
        return;
    }
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match std::fs::copy(&legacy, &path) {
        Ok(_) => diagnose::log("migrated settings from the legacy settings folder"),
        Err(error) => diagnose::log(format!("legacy settings migration failed: {error}")),
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct SettingsFile {
    #[serde(default)]
    appearance: Appearance,
    #[serde(default)]
    floating_position: Option<(i32, i32)>,
    /// The point the floating widget docks nearest to, in screen pixels.
    /// Absent until the widget is first dragged; an older floating_position
    /// stands in for it until then.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    dock_anchor: Option<(i32, i32)>,
    /// Migrate older taskbar/click-through preferences once.
    #[serde(default)]
    desktop_layout_version: u32,
    #[serde(default)]
    tray_offset: i32,
    #[serde(default)]
    taskbar_index: usize,
    #[serde(default = "default_poll_interval")]
    poll_interval_ms: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_update_check_unix: Option<u64>,
    #[serde(default = "default_widget_visible")]
    widget_visible: bool,
    #[serde(default = "default_show_claude_code")]
    show_claude_code: bool,
    #[serde(default = "default_show_codex")]
    show_codex: bool,
    #[serde(default = "default_show_antigravity")]
    show_antigravity: bool,
    /// Embed the widget into the taskbar as a WS_CHILD via SetParent.
    ///
    /// True keeps upstream behaviour. Set false to use the floating topmost
    /// popup instead: on Windows 11 the taskbar is a XAML island, and an
    /// embedded foreign child window is composited *underneath* that XAML
    /// content while still hit-testing above the taskbar buttons - so the
    /// widget can be invisible yet still swallow clicks meant for the buttons
    /// it covers. The popup path renders above everything instead.
    #[serde(default = "default_embed_in_taskbar")]
    embed_in_taskbar: bool,
    /// Let mouse input pass straight through the widget (WS_EX_TRANSPARENT).
    ///
    /// The widget is a normal interactive window by default, so it swallows
    /// clicks anywhere it overlaps - including taskbar buttons underneath it.
    /// WS_EX_NOACTIVATE alone does not help: that only stops it taking focus,
    /// it still hit-tests. Turning this on makes the widget purely decorative:
    /// clicks land on whatever is beneath it, and it can no longer be clicked
    /// or dragged, so the tray icon becomes the only way to control it.
    #[serde(default = "default_click_through")]
    click_through: bool,
}

impl Default for SettingsFile {
    fn default() -> Self {
        Self {
            appearance: Appearance::default(),
            floating_position: None,
            dock_anchor: None,
            desktop_layout_version: 1,
            tray_offset: 0,
            taskbar_index: 0,
            poll_interval_ms: default_poll_interval(),
            last_update_check_unix: None,
            widget_visible: true,
            show_claude_code: true,
            show_codex: false,
            show_antigravity: false,
            embed_in_taskbar: default_embed_in_taskbar(),
            click_through: default_click_through(),
        }
    }
}

fn default_poll_interval() -> u32 {
    POLL_15_MIN
}

fn default_widget_visible() -> bool {
    true
}

// Desktop popup by default: interactive so the entire widget can be dragged.
fn default_embed_in_taskbar() -> bool {
    false
}

fn default_click_through() -> bool {
    false
}

fn default_show_claude_code() -> bool {
    true
}

fn default_show_codex() -> bool {
    false
}

fn default_show_antigravity() -> bool {
    false
}

fn load_settings() -> SettingsFile {
    let content = match std::fs::read_to_string(settings_path()) {
        Ok(c) => c,
        Err(_) => return SettingsFile::default(),
    };
    let mut settings: SettingsFile = serde_json::from_str(&content).unwrap_or_default();
    migrate_desktop_settings(&mut settings);
    show_only_claude_code(&mut settings);
    settings
}

/// The widget shows one provider, so whatever an older version saved, only
/// Claude Code is polled and drawn. The flags stay in the file so the poller
/// and tray keep their single code path.
fn show_only_claude_code(settings: &mut SettingsFile) {
    settings.show_claude_code = true;
    settings.show_codex = false;
    settings.show_antigravity = false;
}

fn migrate_desktop_settings(settings: &mut SettingsFile) {
    if settings.desktop_layout_version == 0 {
        settings.embed_in_taskbar = false;
        settings.click_through = false;
        settings.desktop_layout_version = 1;
    }
}

fn save_settings(settings: &SettingsFile) {
    let path = settings_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string_pretty(settings) {
        let _ = std::fs::write(path, json);
    }
}

fn save_state_settings() {
    let state = lock_state();
    if let Some(s) = state.as_ref() {
        save_settings(&SettingsFile {
            appearance: s.appearance.clone(),
            floating_position: s.floating_position,
            dock_anchor: s.dock_anchor,
            desktop_layout_version: 1,
            tray_offset: s.tray_offset,
            taskbar_index: s.taskbar_index,
            poll_interval_ms: s.poll_interval_ms,
            last_update_check_unix: s.last_update_check_unix,
            widget_visible: s.widget_visible,
            show_claude_code: s.show_claude_code,
            show_codex: s.show_codex,
            show_antigravity: s.show_antigravity,
            embed_in_taskbar: s.embed_in_taskbar,
            click_through: s.click_through,
        });
    }
}

fn fable_readout_from_state(s: &AppState) -> (f64, String) {
    if !s.last_poll_ok {
        return (
            0.0,
            if s.weekly_text == "!" { "!" } else { "..." }.to_owned(),
        );
    }
    match s.data.as_ref().and_then(|data| data.claude_code.as_ref()) {
        Some(usage) => {
            let reset = poller::format_reset(
                &usage.fable,
                poller::WindowKind::Weekly,
                localization::STRINGS,
            );
            (
                usage.fable.percentage,
                if reset.is_empty() {
                    "--".to_owned()
                } else {
                    reset
                },
            )
        }
        None => (0.0, "!".to_owned()),
    }
}

fn fable_readout() -> (f64, String) {
    lock_state()
        .as_ref()
        .map(fable_readout_from_state)
        .unwrap_or_else(|| (0.0, "...".to_owned()))
}

fn tray_icon_data_from_state() -> Vec<tray_icon::TrayIconData> {
    let state = lock_state();
    match state.as_ref() {
        Some(s) if s.last_poll_ok => {
            let mut icons = Vec::new();
            if s.show_claude_code {
                icons.push(tray_icon::TrayIconData {
                    kind: tray_icon::TrayIconKind::Claude,
                    percent: readout::has_reading(&s.session_text).then_some(s.session_percent),
                    tooltip: format!(
                        "{} 5h: {} | 7d: {} | Fable: {}",
                        localization::STRINGS.claude_code_model,
                        readout::tooltip_row(s.session_percent, &s.session_text),
                        readout::tooltip_row(s.weekly_percent, &s.weekly_text),
                        {
                            let (pct, reset) = fable_readout_from_state(s);
                            readout::tooltip_row(pct, &reset)
                        }
                    ),
                });
            }
            if s.show_codex {
                icons.push(tray_icon::TrayIconData {
                    kind: tray_icon::TrayIconKind::Codex,
                    percent: readout::has_reading(&s.codex_weekly_text)
                        .then_some(s.codex_weekly_percent),
                    tooltip: format!(
                        "{} 7d: {}",
                        localization::STRINGS.codex_model,
                        readout::tooltip_row(s.codex_weekly_percent, &s.codex_weekly_text)
                    ),
                });
            }
            if s.show_antigravity {
                icons.push(tray_icon::TrayIconData {
                    kind: tray_icon::TrayIconKind::Antigravity,
                    percent: readout::has_reading(&s.antigravity_session_text)
                        .then_some(s.antigravity_session_percent),
                    tooltip: format!(
                        "{} Quota: {} | Extra: {}",
                        localization::STRINGS.antigravity_model,
                        readout::tooltip_row(
                            s.antigravity_session_percent,
                            &s.antigravity_session_text
                        ),
                        readout::tooltip_row(
                            s.antigravity_weekly_percent,
                            &s.antigravity_weekly_text
                        )
                    ),
                });
            }
            icons
        }
        Some(s) => {
            let mut icons = Vec::new();
            if s.show_claude_code {
                icons.push(tray_icon::TrayIconData {
                    kind: tray_icon::TrayIconKind::Claude,
                    percent: None,
                    tooltip: localization::STRINGS.window_title.to_string(),
                });
            }
            if s.show_codex {
                icons.push(tray_icon::TrayIconData {
                    kind: tray_icon::TrayIconKind::Codex,
                    percent: None,
                    tooltip: localization::STRINGS.codex_window_title.to_string(),
                });
            }
            if s.show_antigravity {
                icons.push(tray_icon::TrayIconData {
                    kind: tray_icon::TrayIconKind::Antigravity,
                    percent: None,
                    tooltip: localization::STRINGS.antigravity_window_title.to_string(),
                });
            }
            icons
        }
        None => Vec::new(),
    }
}

fn sync_tray_icons(hwnd: HWND) {
    let icons = tray_icon_data_from_state();
    tray_icon::sync(hwnd, &icons);
}

fn toggle_widget_visibility(hwnd: HWND) {
    let new_visible = {
        let mut state = lock_state();
        if let Some(s) = state.as_mut() {
            s.widget_visible = !s.widget_visible;
            s.widget_visible
        } else {
            return;
        }
    };
    save_state_settings();
    unsafe {
        if new_visible {
            position_at_taskbar();
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            render_layered();
            SetTimer(hwnd, TIMER_ANIM, ANIM_REFRESH_MS, None);
        } else {
            let _ = ShowWindow(hwnd, SW_HIDE);
            let _ = KillTimer(hwnd, TIMER_ANIM);
            disarm_motion(hwnd);
        }
    }
}

/// Locate the taskbar and record it in state, optionally reparenting into it.
///
/// `embed` false still does the discovery, the tray hook and the state
/// bookkeeping - it only skips the SetParent. That matters because
/// position_at_taskbar() returns early without a taskbar handle, so a widget
/// that merely skipped this function would never be positioned at all and
/// would sit at the origin. Returns whether a taskbar was found, not whether
/// it embedded; the caller reads `embed` for that.
fn attach_to_taskbar(hwnd: HWND, requested_index: usize, embed: bool) -> bool {
    let taskbars = native_interop::find_taskbars();
    if taskbars.is_empty() {
        diagnose::log("taskbar not found; using fallback popup window");
        return false;
    }

    let index = requested_index.min(taskbars.len().saturating_sub(1));
    let taskbar = taskbars[index];
    diagnose::log(format!(
        "taskbar selected index={index} count={} hwnd={:?} rect=({}, {}, {}, {})",
        taskbars.len(),
        taskbar.hwnd,
        taskbar.rect.left,
        taskbar.rect.top,
        taskbar.rect.right,
        taskbar.rect.bottom
    ));

    let old_hook = {
        let mut state = lock_state();
        state.as_mut().and_then(|s| s.win_event_hook.take())
    };
    if let Some(hook) = old_hook {
        native_interop::unhook_win_event(hook);
    }

    if embed {
        native_interop::embed_in_taskbar(hwnd, taskbar.hwnd);
    } else {
        diagnose::log("taskbar located; not reparenting (embed_in_taskbar=false)");
    }

    let tray_notify = native_interop::find_child_window(taskbar.hwnd, "TrayNotifyWnd");
    if tray_notify.is_some() {
        diagnose::log("TrayNotifyWnd found");
    } else {
        diagnose::log("TrayNotifyWnd not found");
    }

    let hook = tray_notify.and_then(|tray_hwnd| {
        let thread_id = native_interop::get_window_thread_id(tray_hwnd);
        native_interop::set_tray_event_hook(thread_id, on_tray_location_changed)
    });
    if hook.is_some() {
        diagnose::log("tray event hook installed");
    } else {
        diagnose::log("tray event hook could not be installed");
    }

    let mut state = lock_state();
    if let Some(s) = state.as_mut() {
        s.taskbar_hwnd = Some(taskbar.hwnd);
        s.tray_notify_hwnd = tray_notify;
        s.win_event_hook = hook;
        s.taskbar_index = index;
        s.embedded = embed;
    }
    true
}

fn taskbar_at_point(pt: POINT) -> Option<(usize, native_interop::TaskbarWindow)> {
    native_interop::find_taskbars()
        .into_iter()
        .enumerate()
        .find(|(_, taskbar)| {
            pt.x >= taskbar.rect.left
                && pt.x < taskbar.rect.right
                && pt.y >= taskbar.rect.top
                && pt.y < taskbar.rect.bottom
        })
}

fn tray_left_for_taskbar(taskbar_hwnd: HWND, taskbar_rect: RECT) -> i32 {
    let mut tray_left = taskbar_rect.right;
    if let Some(tray_hwnd) = native_interop::find_child_window(taskbar_hwnd, "TrayNotifyWnd") {
        if let Some(tray_rect) = native_interop::get_window_rect_safe(tray_hwnd) {
            tray_left = tray_rect.left;
        }
    }
    tray_left
}

fn clamp_offset_for_taskbar(taskbar_hwnd: HWND, taskbar_rect: RECT, offset: i32) -> i32 {
    let tray_left = tray_left_for_taskbar(taskbar_hwnd, taskbar_rect);
    let max_offset = (tray_left - taskbar_rect.left - total_widget_width()).max(0);
    offset.clamp(0, max_offset)
}

fn offset_for_drop_point(
    taskbar_hwnd: HWND,
    taskbar_rect: RECT,
    pt: POINT,
    drag_start_client_x: i32,
) -> i32 {
    let tray_left = tray_left_for_taskbar(taskbar_hwnd, taskbar_rect);
    let desired_left = pt.x - taskbar_rect.left - drag_start_client_x;
    let offset = tray_left - taskbar_rect.left - total_widget_width() - desired_left;
    clamp_offset_for_taskbar(taskbar_hwnd, taskbar_rect, offset)
}

fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn update_check_interval() -> Duration {
    Duration::from_secs(24 * 60 * 60)
}

fn auto_update_check_due(last_update_check_unix: Option<u64>) -> bool {
    let Some(last_update_check_unix) = last_update_check_unix else {
        return true;
    };

    now_unix_secs().saturating_sub(last_update_check_unix) >= update_check_interval().as_secs()
}

fn schedule_auto_update_check(hwnd: HWND) {
    let delay_ms = {
        let state = lock_state();
        let Some(s) = state.as_ref() else {
            return;
        };

        if auto_update_check_due(s.last_update_check_unix) {
            None
        } else {
            let elapsed = now_unix_secs().saturating_sub(s.last_update_check_unix.unwrap_or(0));
            let remaining_secs = update_check_interval().as_secs().saturating_sub(elapsed);
            Some((remaining_secs.saturating_mul(1000)).min(u32::MAX as u64) as u32)
        }
    };

    unsafe {
        let _ = KillTimer(hwnd, TIMER_UPDATE_CHECK);
        if let Some(delay_ms) = delay_ms {
            SetTimer(hwnd, TIMER_UPDATE_CHECK, delay_ms.max(1), None);
        }
    }
}

/// An empty countdown means no known reset time; the readout needs something
/// to draw, and the old combined line used to supply the percentage there.
fn reset_or_dash(text: String) -> String {
    if text.is_empty() {
        "--".to_string()
    } else {
        text
    }
}

fn refresh_usage_texts(state: &mut AppState) {
    if !state.last_poll_ok {
        return;
    }

    let strings = localization::STRINGS;
    let Some(data) = state.data.as_ref() else {
        return;
    };

    if let Some(claude_code) = data.claude_code.as_ref() {
        state.session_text = reset_or_dash(poller::format_reset(
            &claude_code.session,
            poller::WindowKind::Session,
            strings,
        ));
        state.weekly_text = reset_or_dash(poller::format_reset(
            &claude_code.weekly,
            poller::WindowKind::Weekly,
            strings,
        ));
    } else if state.show_claude_code {
        state.session_text = "!".to_string();
        state.weekly_text = "!".to_string();
    }

    if let Some(codex) = data.codex.as_ref() {
        state.codex_session_text = reset_or_dash(poller::format_reset(
            &codex.session,
            poller::WindowKind::Session,
            strings,
        ));
        state.codex_weekly_text = reset_or_dash(poller::format_reset(
            &codex.weekly,
            poller::WindowKind::Weekly,
            strings,
        ));
    } else if state.show_codex {
        state.codex_session_text = "!".to_string();
        state.codex_weekly_text = "!".to_string();
    }

    if let Some(antigravity) = data.antigravity.as_ref() {
        state.antigravity_session_text = reset_or_dash(poller::format_reset(
            &antigravity.session,
            poller::WindowKind::Session,
            strings,
        ));
        state.antigravity_weekly_text =
            if antigravity.weekly.resets_at.is_none() && antigravity.weekly.percentage == 0.0 {
                "--".to_string()
            } else {
                reset_or_dash(poller::format_reset(
                    &antigravity.weekly,
                    poller::WindowKind::Weekly,
                    strings,
                ))
            };
    } else if state.show_antigravity {
        state.antigravity_session_text = "!".to_string();
        state.antigravity_weekly_text = "!".to_string();
    }
}

fn show_info_message(hwnd: HWND, title: &str, message: &str) {
    unsafe {
        let title_wide = native_interop::wide_str(title);
        let message_wide = native_interop::wide_str(message);
        let _ = MessageBoxW(
            hwnd,
            PCWSTR::from_raw(message_wide.as_ptr()),
            PCWSTR::from_raw(title_wide.as_ptr()),
            MB_OK | MB_ICONINFORMATION,
        );
    }
}

fn show_error_message(hwnd: HWND, title: &str, message: &str) {
    unsafe {
        let title_wide = native_interop::wide_str(title);
        let message_wide = native_interop::wide_str(message);
        let _ = MessageBoxW(
            hwnd,
            PCWSTR::from_raw(message_wide.as_ptr()),
            PCWSTR::from_raw(title_wide.as_ptr()),
            MB_OK | MB_ICONERROR,
        );
    }
}

fn show_update_prompt(hwnd: HWND, strings: Strings, release: &ReleaseDescriptor) -> bool {
    let message = strings
        .update_prompt_now
        .replace("{version}", &release.latest_version);

    unsafe {
        let title_wide = native_interop::wide_str(strings.update_available);
        let message_wide = native_interop::wide_str(&message);
        MessageBoxW(
            hwnd,
            PCWSTR::from_raw(message_wide.as_ptr()),
            PCWSTR::from_raw(title_wide.as_ptr()),
            MB_YESNO | MB_ICONQUESTION,
        ) == IDYES
    }
}

fn version_action_label(
    strings: Strings,
    install_channel: InstallChannel,
    status: &UpdateStatus,
) -> String {
    let current = env!("CARGO_PKG_VERSION");
    match status {
        UpdateStatus::Idle => format!("v{current} - {}", strings.check_for_updates),
        UpdateStatus::Checking => format!("v{current} - {}", strings.checking_for_updates),
        UpdateStatus::Applying => format!("v{current} - {}", strings.applying_update),
        UpdateStatus::UpToDate => format!("v{current} - {}", strings.up_to_date_short),
        UpdateStatus::Available(release) => match install_channel {
            InstallChannel::Portable => {
                format!(
                    "v{current} - {} v{}",
                    strings.update_to, release.latest_version
                )
            }
            InstallChannel::Winget => format!(
                "v{current} - {} v{}",
                localization::UPDATE_VIA_WINGET_LABEL,
                release.latest_version
            ),
        },
    }
}

fn begin_update_check(hwnd: HWND, interactive: bool) {
    let send_hwnd = SendHwnd::from_hwnd(hwnd);
    let (strings, install_channel) = {
        let mut state = lock_state();
        let Some(app_state) = state.as_mut() else {
            return;
        };

        if matches!(
            app_state.update_status,
            UpdateStatus::Checking | UpdateStatus::Applying
        ) {
            if interactive {
                show_info_message(
                    hwnd,
                    localization::STRINGS.updates,
                    localization::STRINGS.update_in_progress,
                );
            }
            return;
        }

        app_state.update_status = UpdateStatus::Checking;
        (localization::STRINGS, app_state.install_channel)
    };

    std::thread::spawn(move || {
        let hwnd = send_hwnd.to_hwnd();
        let checked_at = now_unix_secs();
        match updater::check_for_updates() {
            Ok(UpdateCheckResult::UpToDate) => {
                {
                    let mut state = lock_state();
                    if let Some(s) = state.as_mut() {
                        s.update_status = UpdateStatus::UpToDate;
                        s.last_update_check_unix = Some(checked_at);
                    }
                }
                save_state_settings();
                if interactive {
                    show_info_message(hwnd, strings.updates, strings.up_to_date);
                }
                unsafe {
                    let _ = PostMessageW(hwnd, WM_APP_UPDATE_CHECK_COMPLETE, WPARAM(0), LPARAM(0));
                }
            }
            Ok(UpdateCheckResult::Available(release)) => {
                {
                    let mut state = lock_state();
                    if let Some(s) = state.as_mut() {
                        s.update_status = UpdateStatus::Available(release.clone());
                        s.last_update_check_unix = Some(checked_at);
                    }
                }
                save_state_settings();
                if interactive && show_update_prompt(hwnd, strings, &release) {
                    match install_channel {
                        InstallChannel::Portable => begin_update_apply(hwnd, release),
                        InstallChannel::Winget => begin_winget_update(hwnd),
                    }
                }
                unsafe {
                    let _ = PostMessageW(hwnd, WM_APP_UPDATE_CHECK_COMPLETE, WPARAM(0), LPARAM(0));
                }
            }
            Err(error) => {
                diagnose::log(format!("update check failed: {error}"));
                {
                    let mut state = lock_state();
                    if let Some(s) = state.as_mut() {
                        s.update_status = UpdateStatus::Idle;
                        s.last_update_check_unix = Some(checked_at);
                    }
                }
                save_state_settings();
                if interactive {
                    let message = format!("{}.\n\n{}", strings.update_failed, error);
                    show_error_message(hwnd, strings.updates, &message);
                }
                unsafe {
                    let _ = PostMessageW(hwnd, WM_APP_UPDATE_CHECK_COMPLETE, WPARAM(0), LPARAM(0));
                }
            }
        }
    });
}

fn begin_update_apply(hwnd: HWND, release: ReleaseDescriptor) {
    let send_hwnd = SendHwnd::from_hwnd(hwnd);
    let strings = {
        let mut state = lock_state();
        let Some(app_state) = state.as_mut() else {
            return;
        };

        if matches!(
            app_state.update_status,
            UpdateStatus::Checking | UpdateStatus::Applying
        ) {
            show_info_message(
                hwnd,
                localization::STRINGS.updates,
                localization::STRINGS.update_in_progress,
            );
            return;
        }

        app_state.update_status = UpdateStatus::Applying;
        localization::STRINGS
    };

    std::thread::spawn(move || {
        let hwnd = send_hwnd.to_hwnd();
        match updater::begin_self_update(&release) {
            Ok(()) => unsafe {
                let _ = PostMessageW(hwnd, WM_CLOSE, WPARAM(0), LPARAM(0));
            },
            Err(error) => {
                diagnose::log(format!("update download/apply failed: {error}"));
                {
                    let mut state = lock_state();
                    if let Some(s) = state.as_mut() {
                        s.update_status = UpdateStatus::Available(release);
                    }
                }
                let message = format!("{}.\n\n{}", strings.update_failed, error);
                show_error_message(hwnd, strings.updates, &message);
                unsafe {
                    let _ = PostMessageW(hwnd, WM_APP_UPDATE_CHECK_COMPLETE, WPARAM(0), LPARAM(0));
                }
            }
        }
    });
}

fn begin_winget_update(hwnd: HWND) {
    let strings = localization::STRINGS;

    match updater::begin_winget_update() {
        Ok(()) => unsafe {
            let _ = PostMessageW(hwnd, WM_CLOSE, WPARAM(0), LPARAM(0));
        },
        Err(error) => {
            let message = format!("{}.\n\n{}", strings.update_failed, error);
            show_error_message(hwnd, strings.updates, &message);
        }
    }
}

const STARTUP_REGISTRY_PATH: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const STARTUP_REGISTRY_KEY: &str = "XilousUsageMonitor";

/// Returns true only if the startup registry value points to this executable.
fn is_startup_enabled() -> bool {
    unsafe {
        let path = native_interop::wide_str(STARTUP_REGISTRY_PATH);
        let key_name = native_interop::wide_str(STARTUP_REGISTRY_KEY);

        let mut hkey = HKEY::default();
        let result = RegOpenKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR::from_raw(path.as_ptr()),
            0,
            KEY_READ,
            &mut hkey,
        );
        if result.is_err() {
            return false;
        }

        // Query the size of the value
        let mut data_size: u32 = 0;
        let result = RegQueryValueExW(
            hkey,
            PCWSTR::from_raw(key_name.as_ptr()),
            None,
            None,
            None,
            Some(&mut data_size),
        );
        if result.is_err() || data_size == 0 {
            let _ = RegCloseKey(hkey);
            return false;
        }

        // Read the value
        let mut buf = vec![0u8; data_size as usize];
        let result = RegQueryValueExW(
            hkey,
            PCWSTR::from_raw(key_name.as_ptr()),
            None,
            None,
            Some(buf.as_mut_ptr()),
            Some(&mut data_size),
        );
        let _ = RegCloseKey(hkey);
        if result.is_err() {
            return false;
        }

        // Convert the registry value (UTF-16) to a string
        let wide_slice =
            std::slice::from_raw_parts(buf.as_ptr() as *const u16, data_size as usize / 2);
        let reg_value = String::from_utf16_lossy(wide_slice)
            .trim_end_matches('\0')
            .to_string();

        // Get the current executable path
        let mut exe_buf = [0u16; 260];
        let len = GetModuleFileNameW(None, &mut exe_buf) as usize;
        if len == 0 {
            return false;
        }
        let current_exe = String::from_utf16_lossy(&exe_buf[..len]);

        // Case-insensitive comparison (Windows paths are case-insensitive)
        reg_value.eq_ignore_ascii_case(&current_exe)
    }
}

fn set_startup_enabled(enable: bool) {
    unsafe {
        let path = native_interop::wide_str(STARTUP_REGISTRY_PATH);

        let mut hkey = HKEY::default();
        let result = RegOpenKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR::from_raw(path.as_ptr()),
            0,
            KEY_SET_VALUE,
            &mut hkey,
        );
        if result.is_err() {
            return;
        }

        let key_name = native_interop::wide_str(STARTUP_REGISTRY_KEY);

        if enable {
            let mut exe_buf = [0u16; 260];
            let len = GetModuleFileNameW(None, &mut exe_buf) as usize;
            if len > 0 {
                // Write the wide string including null terminator
                let byte_len = ((len + 1) * 2) as u32;
                let _ = RegSetValueExW(
                    hkey,
                    PCWSTR::from_raw(key_name.as_ptr()),
                    0,
                    REG_SZ,
                    Some(std::slice::from_raw_parts(
                        exe_buf.as_ptr() as *const u8,
                        byte_len as usize,
                    )),
                );
            }
        } else {
            let _ = RegDeleteValueW(hkey, PCWSTR::from_raw(key_name.as_ptr()));
        }

        let _ = RegCloseKey(hkey);
    }
}

/// Replaces a start-with-Windows value saved under the legacy name with one
/// under the current name, pointing at this executable.
fn migrate_legacy_startup() {
    unsafe {
        let path = native_interop::wide_str(STARTUP_REGISTRY_PATH);
        let legacy_name = native_interop::wide_str(LEGACY_APP_NAME);

        let mut hkey = HKEY::default();
        let result = RegOpenKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR::from_raw(path.as_ptr()),
            0,
            KEY_QUERY_VALUE | KEY_SET_VALUE,
            &mut hkey,
        );
        if result.is_err() {
            return;
        }

        let found = RegQueryValueExW(
            hkey,
            PCWSTR::from_raw(legacy_name.as_ptr()),
            None,
            None,
            None,
            None,
        )
        .is_ok();
        if found {
            let _ = RegDeleteValueW(hkey, PCWSTR::from_raw(legacy_name.as_ptr()));
        }
        let _ = RegCloseKey(hkey);

        if found {
            set_startup_enabled(true);
            diagnose::log("migrated the legacy start-with-Windows value");
        }
    }
}

// Two ring blocks side by side: usage (5h outer, 7d inner), then the device
// (CPU outer, RAM inner), each with its figures to the right of the rings.
pub(crate) const WIDGET_HEIGHT: i32 = 62;
pub(crate) const WIDGET_W: i32 = 310;
pub(crate) const LEFT_DIVIDER_W: i32 = 3;

/// Ring blocks: where each ring sits, its diameter, and the band geometry.
pub(crate) const USAGE_X: i32 = 12;
pub(crate) const SYSTEM_X: i32 = 186;
pub(crate) const RING_Y: i32 = 6;
pub(crate) const RING_D: i32 = 50;
const RING_STROKE: i32 = 5;
const RING_GAP: i32 = 3;

/// The hairline between the usage block and the device block.
pub(crate) const SPLIT_X: i32 = 178;
pub(crate) const SPLIT_Y: i32 = 14;
pub(crate) const SPLIT_H: i32 = 34;

/// The figure column beside each ring: label, percentage, then (usage only)
/// the time to reset. One row per ring, outer ring first.
pub(crate) const TEXT_DX: i32 = 58;
pub(crate) const LABEL_W: i32 = 24;
pub(crate) const FIGURE_W: i32 = 34;
pub(crate) const RESET_W: i32 = 46;
const ROW1_Y: i32 = 15;
const ROW2_Y: i32 = 33;
const ROW_H: i32 = 14;

/// How often the live CPU and RAM readings are sampled. The paint eases toward
/// whatever this last stored, so the rings drift continuously between samples.
const RAM_REFRESH_MS: u32 = 2000;

/// Ambient-animation cadence (10fps). Drives the breath, the settling
/// figures, the RAM drift and the poll-freshness hairline.
const ANIM_REFRESH_MS: u32 = 100;

const SESSION_WINDOW: Duration = Duration::from_secs(5 * 60 * 60);
const WEEKLY_WINDOW: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// Fraction of the rate-limit window already elapsed, as 0-100.
/// This is the utilization the bar would show if usage were spread evenly
/// across the whole window, i.e. the "on pace" position. None when there is
/// no reset time or the window has already expired.
fn pace_percent(resets_at: Option<SystemTime>, window: Duration) -> Option<f64> {
    let remaining = resets_at?.duration_since(SystemTime::now()).ok()?;
    let remaining = remaining.min(window);
    Some((1.0 - remaining.as_secs_f64() / window.as_secs_f64()) * 100.0)
}

fn claude_pace_markers(data: Option<&AppUsageData>) -> (Option<f64>, Option<f64>) {
    match data.and_then(|d| d.claude_code.as_ref()) {
        Some(usage) => (
            pace_percent(usage.session.resets_at, SESSION_WINDOW),
            pace_percent(usage.weekly.resets_at, WEEKLY_WINDOW),
        ),
        None => (None, None),
    }
}

fn pace_marker_color(is_dark: bool) -> Color {
    if is_dark {
        Color::from_hex("#E0E0E0")
    } else {
        Color::from_hex("#303030")
    }
}

fn is_drag_handle_point(client_x: i32, client_y: i32) -> bool {
    let divider_h = widget_sc(25);
    let divider_top = (widget_sc(WIDGET_HEIGHT) - divider_h) / 2;
    client_x >= 0
        && client_x < widget_sc(LEFT_DIVIDER_W)
        && client_y >= divider_top
        && client_y < divider_top + divider_h
}

fn cursor_is_on_drag_handle(hwnd: HWND) -> bool {
    unsafe {
        let mut pt = POINT::default();
        if GetCursorPos(&mut pt).is_err() || !ScreenToClient(hwnd, &mut pt).as_bool() {
            return false;
        }
        is_drag_handle_point(pt.x, pt.y)
    }
}

/// Live physical-memory load as a percentage, with the fraction kept.
/// `dwMemoryLoad` is only ever a whole number, so a steady machine looks frozen
/// through it; the byte counters move continuously, which is what lets the
/// column drift instead of sitting still between whole-percent steps.
fn current_ram_percent() -> f64 {
    unsafe {
        let mut status = MEMORYSTATUSEX {
            dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32,
            ..Default::default()
        };
        if GlobalMemoryStatusEx(&mut status).is_ok() && status.ullTotalPhys > 0 {
            let used = status.ullTotalPhys.saturating_sub(status.ullAvailPhys) as f64;
            (used / status.ullTotalPhys as f64 * 100.0).clamp(0.0, 100.0)
        } else {
            0.0
        }
    }
}

/// Most recent RAM sample in hundredths of a percent, written by the RAM timer
/// and eased toward by every paint.
static RAM_SAMPLE: AtomicU32 = AtomicU32::new(u32::MAX);

fn sample_ram() {
    RAM_SAMPLE.store(
        (current_ram_percent() * 100.0).round() as u32,
        Ordering::Relaxed,
    );
}

fn last_ram_sample() -> f64 {
    match RAM_SAMPLE.load(Ordering::Relaxed) {
        u32::MAX => {
            sample_ram();
            current_ram_percent()
        }
        v => v as f64 / 100.0,
    }
}

/// Total CPU load across all cores since the previous call, as a percentage.
/// GetSystemTimes counts idle time inside kernel time, so busy is everything
/// in kernel plus user that was not idle. The first call has nothing to diff
/// against and reports 0.
fn current_cpu_percent() -> f64 {
    static LAST: Mutex<Option<(u64, u64)>> = Mutex::new(None);
    let as_u64 = |t: FILETIME| (t.dwHighDateTime as u64) << 32 | t.dwLowDateTime as u64;
    let (mut idle, mut kernel, mut user) = (
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
    );
    if unsafe { GetSystemTimes(Some(&mut idle), Some(&mut kernel), Some(&mut user)) }.is_err() {
        return 0.0;
    }
    let idle = as_u64(idle);
    let total = as_u64(kernel) + as_u64(user);
    let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
    let percent = match *last {
        Some((last_idle, last_total)) if total > last_total => {
            let busy = (total - last_total).saturating_sub(idle.saturating_sub(last_idle));
            (busy as f64 / (total - last_total) as f64 * 100.0).clamp(0.0, 100.0)
        }
        _ => 0.0,
    };
    *last = Some((idle, total));
    percent
}

/// Most recent CPU sample in hundredths of a percent, written by the RAM timer
/// alongside the RAM sample and eased toward by every paint.
static CPU_SAMPLE: AtomicU32 = AtomicU32::new(u32::MAX);

fn sample_cpu() {
    CPU_SAMPLE.store(
        (current_cpu_percent() * 100.0).round() as u32,
        Ordering::Relaxed,
    );
}

fn last_cpu_sample() -> f64 {
    match CPU_SAMPLE.load(Ordering::Relaxed) {
        u32::MAX => {
            sample_cpu();
            0.0
        }
        v => v as f64 / 100.0,
    }
}

/// Widget width at a given DPI and WIDGET SIZE.
fn widget_width_at(dpi: u32, widget_size: u32) -> i32 {
    scale_px(WIDGET_W, dpi, widget_size)
}

fn total_widget_width() -> i32 {
    widget_width_at(
        CURRENT_DPI.load(Ordering::Relaxed),
        WIDGET_SIZE.load(Ordering::Relaxed),
    )
}

fn claude_accent_color() -> Color {
    Color::from_hex("#D97757")
}

fn codex_accent_color(is_dark: bool) -> Color {
    if is_dark {
        Color::from_hex("#F5F5F5")
    } else {
        Color::from_hex("#1F1F1F")
    }
}

fn antigravity_accent_color() -> Color {
    Color::from_hex("#4285F4")
}

/// A four-stop palette for an LED-glow fill: a dark bottom `edge`, the main
/// `mid` body colour, a near-white `core` used for the breathing highlight, and
/// a `glow` colour used for the soft outer halo.
#[derive(Clone, Copy)]
struct Led {
    edge: Color,
    mid: Color,
    core: Color,
    glow: Color,
}

/// Claude bars: a cool cyan, deliberately off the Anthropic terracotta so the
/// widget reads as a sleek instrument rather than a brand splash.
fn claude_led() -> Led {
    Led {
        edge: Color::from_hex("#1D4ED8"),
        mid: Color::from_hex("#22D3EE"),
        core: Color::from_hex("#EAFCFF"),
        glow: Color::from_hex("#38BDF8"),
    }
}

/// The device rings: an emerald, kept distinct from the cyan usage rings so
/// CPU and RAM still read as their own kind of metric.
fn ring_led() -> Led {
    Led {
        edge: Color::from_hex("#047857"),
        mid: Color::from_hex("#34D399"),
        core: Color::from_hex("#ECFFFB"),
        glow: Color::from_hex("#6EE7B7"),
    }
}

/// Semantic ramps, kept deliberately separate from provider identity: these
/// say how much trouble a window is in, not which product it belongs to.
fn warm_led() -> Led {
    Led {
        edge: Color::from_hex("#92400E"),
        mid: Color::from_hex("#FBBF24"),
        core: Color::from_hex("#FFF8E3"),
        glow: Color::from_hex("#F59E0B"),
    }
}

fn hot_led() -> Led {
    Led {
        edge: Color::from_hex("#7F1D1D"),
        mid: Color::from_hex("#F87171"),
        core: Color::from_hex("#FFE6E6"),
        glow: Color::from_hex("#EF4444"),
    }
}

fn lerp_led(a: &Led, b: &Led, t: f64) -> Led {
    Led {
        edge: blend(a.edge, b.edge, t),
        mid: blend(a.mid, b.mid, t),
        core: blend(a.core, b.core, t),
        glow: blend(a.glow, b.glow, t),
    }
}

/// How a usage row is coloured. `burn` is how far ahead of an even spend the
/// reading is, so a window that will run out early goes warm well before it is
/// numerically high - which is the whole point of showing pace at all.
fn state_led(percent: f64, burn: f64, base: &Led) -> Led {
    if percent >= 90.0 || burn >= 18.0 {
        hot_led()
    } else if percent >= 70.0 || burn >= 8.0 {
        warm_led()
    } else {
        *base
    }
}

/// CPU and RAM warm continuously rather than in steps, so the colour itself
/// reads as headroom: emerald while there is room, amber as it tightens, red as
/// the machine runs out.
fn load_led(percent: f64) -> Led {
    let ring = ring_led();
    if percent <= 60.0 {
        ring
    } else if percent <= 85.0 {
        lerp_led(&ring, &warm_led(), (percent - 60.0) / 25.0)
    } else {
        lerp_led(
            &warm_led(),
            &hot_led(),
            ((percent - 85.0) / 15.0).clamp(0.0, 1.0),
        )
    }
}

fn lerp_u8(a: u8, b: u8, t: f64) -> u8 {
    let t = t.clamp(0.0, 1.0);
    (a as f64 + (b as f64 - a as f64) * t)
        .round()
        .clamp(0.0, 255.0) as u8
}

/// Blend `a` toward `b` by `t` (0..1). Because the widget is composited opaque
/// over the taskbar background colour (see the alpha key in render_layered),
/// blending a glow colour toward `bg` and drawing it opaque is how we fake
/// translucency without any per-pixel alpha work.
fn blend(a: Color, b: Color, t: f64) -> Color {
    Color {
        r: lerp_u8(a.r, b.r, t),
        g: lerp_u8(a.g, b.g, t),
        b: lerp_u8(a.b, b.b, t),
    }
}

/// A slow, dramatic breathing curve in 0..1: smoothstep over a sine so it
/// lingers near the dark low and the bright high rather than gliding linearly.
/// Period is ~5.5s. Purely wall-clock driven, so every gauge breathes together.
fn anim_breath() -> f64 {
    static ANIM_START: OnceLock<Instant> = OnceLock::new();
    let start = ANIM_START.get_or_init(Instant::now);
    let t = start.elapsed().as_secs_f64();
    let s = 0.5 + 0.5 * (t * 1.15).sin();
    s * s * (3.0 - 2.0 * s)
}

/// Ink for the weekday initials. One colour in every block, lit or not: a
/// letter that changed colour at the fill edge read as an artefact rather than
/// as a scale, and the whole point of the initials is that the seven of them are
/// one row you can scan.
/// Frame-to-frame animation the paint owns.
///
/// The displayed figures ease toward whatever the poller last reported, so a
/// new reading reads as a change rather than appearing to have always been
/// there. `flash` decays after each poll, `text_tick` fires when a countdown
/// digit rolls over, and `cpu` and `ram` ease toward the last sample so a
/// steady machine still drifts. All of it is driven off the animation timer.
struct LiveAnim {
    shown: [f64; 7],
    cpu: f64,
    ram: f64,
    flash: f64,
    text_tick: f64,
    last_texts: [String; 7],
    last_poll: Option<Instant>,
    last_step: Option<Instant>,
}

#[derive(Clone, Copy)]
struct LiveFrame {
    shown: [f64; 7],
    cpu: f64,
    ram: f64,
    flash: f64,
    text_tick: f64,
    poll_frac: f64,
}

static LIVE_ANIM: Mutex<Option<LiveAnim>> = Mutex::new(None);

/// Advance the animation one frame and hand back the values to draw with.
fn step_anim(
    targets: &[f64; 7],
    texts: &[&str; 7],
    cpu_target: f64,
    ram_target: f64,
    poll_interval_ms: u32,
) -> LiveFrame {
    let mut guard = LIVE_ANIM.lock().unwrap_or_else(|e| e.into_inner());
    let now = Instant::now();
    let anim = guard.get_or_insert_with(|| LiveAnim {
        shown: *targets,
        cpu: cpu_target,
        ram: ram_target,
        flash: 0.0,
        text_tick: 0.0,
        last_texts: std::array::from_fn(|_| String::new()),
        last_poll: Some(now),
        last_step: Some(now),
    });

    // Clamped so a widget that was hidden for an hour does not jump on the
    // first frame back.
    let dt = anim
        .last_step
        .map(|t| now.duration_since(t).as_secs_f64())
        .unwrap_or(0.033)
        .clamp(0.0, 0.5);
    anim.last_step = Some(now);

    // Exponential approach: fast enough to feel like a jump, slow enough to
    // read as one.
    let k = 1.0 - (-dt / 0.18).exp();
    for i in 0..7 {
        let delta = targets[i] - anim.shown[i];
        if delta.abs() < 0.02 {
            anim.shown[i] = targets[i];
        } else {
            anim.shown[i] += delta * k;
        }
    }

    let rk = 1.0 - (-dt / 0.45).exp();
    anim.cpu += (cpu_target - anim.cpu) * rk;
    anim.ram += (ram_target - anim.ram) * rk;

    for i in 0..7 {
        if anim.last_texts[i] != texts[i] {
            if !anim.last_texts[i].is_empty() {
                anim.text_tick = 1.0;
            }
            anim.last_texts[i] = texts[i].to_string();
        }
    }

    anim.flash *= (-dt / 0.28).exp();
    anim.text_tick *= (-dt / 0.35).exp();
    if anim.flash < 0.01 {
        anim.flash = 0.0;
    }
    if anim.text_tick < 0.01 {
        anim.text_tick = 0.0;
    }

    let window = (poll_interval_ms.max(1) as f64) / 1000.0;
    let poll_frac = anim
        .last_poll
        .map(|t| (now.duration_since(t).as_secs_f64() / window).clamp(0.0, 1.0))
        .unwrap_or(0.0);

    LiveFrame {
        shown: anim.shown,
        cpu: anim.cpu,
        ram: anim.ram,
        flash: anim.flash,
        text_tick: anim.text_tick,
        poll_frac,
    }
}

/// What a poll hands `WM_APP_USAGE_UPDATED` in its wparam: whether a reading
/// landed, and if so whether it differs from the one already on screen.
const POLL_NO_READING: usize = 0;
const POLL_UNCHANGED: usize = 1;
const POLL_CHANGED: usize = 2;

/// A poll landed: the freshness hairline restarts from the left, and when the
/// reading changed the figures flash as they move to their new values.
fn mark_poll(changed: bool) {
    let mut guard = LIVE_ANIM.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(anim) = guard.as_mut() {
        if changed {
            anim.flash = 1.0;
        }
        anim.last_poll = Some(Instant::now());
    }
}

pub fn run() {
    // Enable Per-Monitor DPI Awareness V2 for crisp rendering at any scale factor
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        CURRENT_DPI.store(GetDpiForSystem(), Ordering::Relaxed);
    }
    diagnose::log("window::run started");

    // Single-instance guard: silently exit if another instance is running.
    // Exception: when relaunched after an explorer restart (ENV_RELAUNCH set),
    // wait for the previous instance to release the mutex, then take over.
    let is_relaunch = std::env::var(ENV_RELAUNCH).is_ok();
    let mutex_name = native_interop::wide_str("Global\\XilousUsageMonitor");
    let _mutex = unsafe {
        let handle = CreateMutexW(None, true, PCWSTR::from_raw(mutex_name.as_ptr()));
        match handle {
            Ok(h) => {
                if GetLastError() == ERROR_ALREADY_EXISTS {
                    if is_relaunch {
                        diagnose::log("relaunch: waiting for previous instance to exit");
                        let wait_result = WaitForSingleObject(h, 10_000);
                        if wait_result != WAIT_OBJECT_0 && wait_result != WAIT_ABANDONED {
                            diagnose::log(format!(
                                "startup aborted: previous instance did not exit cleanly ({wait_result:?})"
                            ));
                            return;
                        }
                    } else {
                        diagnose::log("startup aborted: another instance is already running");
                        return;
                    }
                }
                h
            }
            Err(error) => {
                diagnose::log_error(
                    "startup aborted: unable to create single-instance mutex",
                    error,
                );
                return;
            }
        }
    };

    let class_name = native_interop::wide_str("XilousUsageMonitor");

    unsafe {
        let hinstance = GetModuleHandleW(PCWSTR::null()).unwrap();
        let (large_icon, small_icon) = load_embedded_app_icons();

        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wnd_proc),
            hInstance: HINSTANCE(hinstance.0),
            hIcon: large_icon,
            hIconSm: small_icon,
            hCursor: LoadCursorW(HINSTANCE::default(), IDC_ARROW).unwrap_or_default(),
            hbrBackground: HBRUSH(std::ptr::null_mut()),
            lpszClassName: PCWSTR::from_raw(class_name.as_ptr()),
            ..Default::default()
        };

        let atom = RegisterClassExW(&wc);
        if atom == 0 {
            diagnose::log("RegisterClassExW returned 0");
        }

        migrate_legacy_settings();
        migrate_legacy_startup();
        let mut settings = load_settings();
        if std::env::args().any(|arg| arg == "--codex-only") {
            settings.show_claude_code = false;
            settings.show_codex = true;
            settings.show_antigravity = false;
        }
        let install_channel = updater::current_install_channel();
        // Before the window is created, so its very first size is right.
        WIDGET_SIZE.store(settings.appearance.clamped_widget_size(), Ordering::Relaxed);

        // Create as layered popup (will be reparented into taskbar)
        let title = native_interop::wide_str(localization::STRINGS.window_title);
        // NOACTIVATE stops the widget taking focus but it still hit-tests, so
        // it swallows clicks on whatever it covers. TRANSPARENT is what
        // actually passes them through (see the click_through doc comment).
        let mut ex_style = WS_EX_TOOLWINDOW | WS_EX_LAYERED | WS_EX_NOACTIVATE;
        if settings.click_through {
            ex_style |= WS_EX_TRANSPARENT;
            diagnose::log("click_through enabled: adding WS_EX_TRANSPARENT");
        }
        let hwnd = CreateWindowExW(
            ex_style,
            PCWSTR::from_raw(class_name.as_ptr()),
            PCWSTR::from_raw(title.as_ptr()),
            WS_POPUP,
            0,
            0,
            total_widget_width(),
            widget_sc(WIDGET_HEIGHT),
            HWND::default(),
            HMENU::default(),
            hinstance,
            None,
        )
        .unwrap();

        if !large_icon.is_invalid() {
            let _ = SendMessageW(
                hwnd,
                WM_SETICON,
                WPARAM(ICON_BIG as usize),
                LPARAM(large_icon.0 as isize),
            );
        }
        if !small_icon.is_invalid() {
            let _ = SendMessageW(
                hwnd,
                WM_SETICON,
                WPARAM(ICON_SMALL as usize),
                LPARAM(small_icon.0 as isize),
            );
        }

        diagnose::log(format!("main window created hwnd={:?}", hwnd));

        let is_dark = settings.appearance.is_dark(theme::is_dark_mode());
        let mut embedded = false;

        {
            let mut state = lock_state();
            *state = Some(AppState {
                appearance: settings.appearance.clone(),
                hwnd: SendHwnd::from_hwnd(hwnd),
                taskbar_hwnd: None,
                tray_notify_hwnd: None,
                win_event_hook: None,
                foreground_hook: None,
                is_dark,
                embedded: false,
                install_channel,
                session_percent: 0.0,
                session_text: "--".to_string(),
                weekly_percent: 0.0,
                weekly_text: "--".to_string(),
                codex_session_percent: 0.0,
                codex_session_text: "--".to_string(),
                codex_weekly_percent: 0.0,
                codex_weekly_text: "--".to_string(),
                antigravity_session_percent: 0.0,
                antigravity_session_text: "--".to_string(),
                antigravity_weekly_percent: 0.0,
                antigravity_weekly_text: "--".to_string(),
                show_claude_code: settings.show_claude_code,
                show_codex: settings.show_codex,
                show_antigravity: settings.show_antigravity,
                data: None,
                poll_interval_ms: settings.poll_interval_ms,
                retry_count: 0,
                force_notify_auth_error: false,
                auth_error_paused_polling: false,
                auth_watch_mode: poller::CredentialWatchMode::ActiveSource,
                auth_watch_snapshot: Vec::new(),
                last_poll_ok: false,
                update_status: UpdateStatus::Idle,
                last_update_check_unix: settings.last_update_check_unix,
                taskbar_index: settings.taskbar_index,
                tray_offset: settings.tray_offset,
                dragging: false,
                drag_start_mouse_x: 0,
                floating_position: settings.floating_position,
                dock_anchor: settings.dock_anchor,
                drag_start_client_x: 0,
                drag_start_offset: 0,
                widget_visible: settings.widget_visible,
                embed_in_taskbar: settings.embed_in_taskbar,
                click_through: settings.click_through,
            });
        }

        // Locate the taskbar and record it either way; reparent into it only
        // when embed_in_taskbar is set (see its doc comment for why it may not
        // be). Skipping this call entirely would leave no taskbar handle and
        // the widget would never get positioned.
        if attach_to_taskbar(hwnd, settings.taskbar_index, settings.embed_in_taskbar) {
            embedded = settings.embed_in_taskbar;
        } else {
            // No taskbar existed yet - the usual cause is launching during
            // login before explorer has created the shell. attach runs once
            // here and the watchdog only recovers a taskbar that changes after
            // a successful attach, so without a retry the widget would stay
            // pinned to its (0,0) creation spot for the whole session. Poll
            // for the taskbar and attach + position once it appears.
            SetTimer(hwnd, native_interop::TIMER_TASKBAR_RETRY, 1_000, None);
            diagnose::log("taskbar not ready at startup; scheduling attach retries");
        }

        // If not embedded, fall back to topmost popup with SetLayeredWindowAttributes
        if !embedded {
            // No SetLayeredWindowAttributes here: the floating widget draws
            // with per-pixel alpha through UpdateLayeredWindow, which a
            // constant-alpha layered window would refuse.
            init_dock(settings.dock_anchor, settings.floating_position);
            native_interop::reassert_topmost(hwnd);

            // Setting HWND_TOPMOST once is not enough to stay on top: it is a
            // shared tier, so activating any other window - clicking a taskbar
            // button to raise an app, for instance - can leave the widget
            // behind it and it appears to vanish. Re-assert on every foreground
            // change (instant, event-driven) with a slow timer as a backstop
            // for z-order changes that involve no foreground transition.
            let fg_hook = native_interop::set_foreground_event_hook(on_foreground_changed);
            if fg_hook.is_some() {
                diagnose::log("foreground hook installed for topmost recovery");
            } else {
                diagnose::log("foreground hook could not be installed");
            }
            {
                let mut state = lock_state();
                if let Some(s) = state.as_mut() {
                    s.foreground_hook = fg_hook;
                }
            }
            SetTimer(hwnd, native_interop::TIMER_TOPMOST, 1_000, None);
        }

        // Register system tray icon(s)
        sync_tray_icons(hwnd);

        // Position and show (only if widget_visible preference is true).
        //
        // The hide is explicit rather than implied by "we never showed it".
        // The window is created WS_POPUP with no WS_VISIBLE, but by this point
        // embedding has reparented it into the taskbar (WS_CHILD + SetParent),
        // and it comes out of that with WS_VISIBLE set. Skipping the show call
        // therefore left a saved widget_visible=false ignored: an invisible
        // window stayed parked on the taskbar, hidden behind the Win11 XAML
        // surface that paints over foreign child windows, while still
        // hit-testing - so it silently swallowed clicks on the taskbar buttons
        // underneath it with nothing on screen to explain why.
        position_at_taskbar();
        if settings.widget_visible {
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        } else {
            let _ = ShowWindow(hwnd, SW_HIDE);
        }
        diagnose::log(if settings.widget_visible {
            "window shown"
        } else {
            "window hidden (widget_visible=false)"
        });

        // Initial render via UpdateLayeredWindow (for embedded) or InvalidateRect (fallback)
        render_layered();

        // Poll timer: 15 minutes
        let initial_poll_ms = {
            let state = lock_state();
            state
                .as_ref()
                .map(|s| s.poll_interval_ms)
                .unwrap_or(POLL_15_MIN)
        };
        SetTimer(hwnd, TIMER_POLL, initial_poll_ms, None);

        // RAM readout timer. Fires every couple of seconds but only repaints
        // when the whole-number percentage changes (see the TIMER_RAM handler).
        // Like TIMER_POLL, it is armed once here; an explorer restart relaunches
        // the whole process, which re-runs this setup.
        SetTimer(hwnd, TIMER_RAM, RAM_REFRESH_MS, None);

        // Ambient animation timer: repaints at ~30fps so the breathing LED glow
        // and RAM ring stay alive even while the numbers hold steady. Only armed
        // while the widget is visible; the visibility toggle stops and restarts
        // it so a hidden widget costs nothing.
        if settings.widget_visible {
            SetTimer(hwnd, TIMER_ANIM, ANIM_REFRESH_MS, None);
        }

        // Watch for explorer.exe restarts so we can re-embed and re-add the tray
        // icon (the shell discards tray registrations when it restarts). This
        // runs on a dedicated thread, NOT a window timer: once explorer destroys
        // the taskbar, our embedded child window stops receiving all messages
        // (WM_TIMER included), so a timer would never fire again.
        spawn_taskbar_watchdog();

        // Initial poll
        let send_hwnd = SendHwnd::from_hwnd(hwnd);
        std::thread::spawn(move || {
            diagnose::log("initial poll thread started");
            do_poll(send_hwnd);
        });

        schedule_auto_update_check(hwnd);
        let should_check_updates = {
            let state = lock_state();
            state
                .as_ref()
                .map(|s| auto_update_check_due(s.last_update_check_unix))
                .unwrap_or(false)
        };
        if should_check_updates {
            begin_update_check(hwnd, false);
        }

        // Initial theme check
        check_theme_change();

        // Message loop
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, HWND::default(), 0, 0).as_bool() {
            if crate::appearance_studio::translate(&msg) {
                continue;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

/// Render widget content and push to the layered window via UpdateLayeredWindow.
/// Renders fully opaque with the actual taskbar background colour so that
/// ClearType sub-pixel font rendering can be used for crisp, OS-native text.
fn render_layered() {
    refresh_dpi();
    let (
        hwnd_val,
        is_dark,
        embedded,
        strings,
        session_pct,
        session_pace,
        weekly_pace,
        session_text,
        weekly_pct,
        weekly_text,
        codex_session_pct,
        codex_session_text,
        codex_weekly_pct,
        codex_weekly_text,
        antigravity_session_pct,
        antigravity_session_text,
        antigravity_weekly_pct,
        antigravity_weekly_text,
        show_claude_code,
        show_codex,
        show_antigravity,
    ) = {
        let state = lock_state();
        match state.as_ref() {
            Some(s) => {
                let (session_pace, weekly_pace) = claude_pace_markers(s.data.as_ref());
                (
                    s.hwnd,
                    s.is_dark,
                    s.embedded,
                    localization::STRINGS,
                    s.session_percent,
                    session_pace,
                    weekly_pace,
                    s.session_text.clone(),
                    s.weekly_percent,
                    s.weekly_text.clone(),
                    s.codex_session_percent,
                    s.codex_session_text.clone(),
                    s.codex_weekly_percent,
                    s.codex_weekly_text.clone(),
                    s.antigravity_session_percent,
                    s.antigravity_session_text.clone(),
                    s.antigravity_weekly_percent,
                    s.antigravity_weekly_text.clone(),
                    s.show_claude_code,
                    s.show_codex,
                    s.show_antigravity,
                )
            }
            None => return,
        }
    };

    let hwnd = hwnd_val.to_hwnd();

    // The floating widget docks, moves and composites itself per frame.
    if !embedded {
        render_docked(hwnd);
        return;
    }

    let width = total_widget_width();
    let height = widget_sc(WIDGET_HEIGHT);

    let accent = claude_accent_color();
    let codex_accent = codex_accent_color(is_dark);
    let antigravity_accent = antigravity_accent_color();
    let track = if is_dark {
        Color::from_hex("#444444")
    } else {
        Color::from_hex("#AAAAAA")
    };
    let palette = appearance().palette(is_dark);
    let text_color = palette[1];
    let bg_color = palette[0];

    unsafe {
        let screen_dc = GetDC(hwnd);

        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                biHeight: -height, // top-down
                biPlanes: 1,
                biBitCount: 32,
                biCompression: 0, // BI_RGB
                ..Default::default()
            },
            ..Default::default()
        };

        let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
        let mem_dc = CreateCompatibleDC(screen_dc);
        let dib =
            CreateDIBSection(mem_dc, &bmi, DIB_RGB_COLORS, &mut bits, None, 0).unwrap_or_default();

        if dib.is_invalid() || bits.is_null() {
            let _ = DeleteDC(mem_dc);
            ReleaseDC(hwnd, screen_dc);
            return;
        }

        let old_bmp = SelectObject(mem_dc, dib);
        let pixel_count = (width * height) as usize;

        // Render once with the actual taskbar background colour.
        // Using an opaque background lets us use CLEARTYPE_QUALITY for
        // sub-pixel font rendering that matches the rest of the OS.
        paint_content(
            mem_dc,
            width,
            height,
            is_dark,
            &bg_color,
            &text_color,
            &accent,
            &track,
            strings,
            session_pct,
            session_pace,
            &session_text,
            weekly_pct,
            weekly_pace,
            &weekly_text,
            codex_session_pct,
            &codex_session_text,
            codex_weekly_pct,
            &codex_weekly_text,
            antigravity_session_pct,
            &antigravity_session_text,
            antigravity_weekly_pct,
            &antigravity_weekly_text,
            show_claude_code,
            show_codex,
            show_antigravity,
            &codex_accent,
            &antigravity_accent,
            fable_readout(),
        );

        // Background pixels → alpha 1 (nearly invisible but still hittable for right-click).
        // Content pixels → fully opaque (preserves ClearType sub-pixel rendering).
        let bg_bgr = bg_color.to_colorref();
        let pixel_data = std::slice::from_raw_parts_mut(bits as *mut u32, pixel_count);
        for px in pixel_data.iter_mut() {
            let rgb = *px & 0x00FFFFFF;
            if rgb == bg_bgr {
                *px = 0x01000000;
            } else {
                *px = rgb | 0xFF000000;
            }
        }

        // Push to window via UpdateLayeredWindow
        let pt_src = POINT { x: 0, y: 0 };
        let sz = SIZE {
            cx: width,
            cy: height,
        };
        let blend = BLENDFUNCTION {
            BlendOp: 0, // AC_SRC_OVER
            BlendFlags: 0,
            SourceConstantAlpha: 255,
            AlphaFormat: 1, // AC_SRC_ALPHA
        };

        let _ = UpdateLayeredWindow(
            hwnd,
            screen_dc,
            None,
            Some(&sz),
            mem_dc,
            Some(&pt_src),
            COLORREF(0),
            Some(&blend),
            ULW_ALPHA,
        );

        // Cleanup
        SelectObject(mem_dc, old_bmp);
        let _ = DeleteObject(dib);
        let _ = DeleteDC(mem_dc);
        ReleaseDC(hwnd, screen_dc);
    }
}

/// The floating widget's motion (see dock.rs), when it last stepped, and
/// whether a finished drag still needs its resting spot saved.
struct DockState {
    motion: dock::Motion,
    last_tick: f64,
    unsaved: bool,
}

static DOCK: Mutex<Option<DockState>> = Mutex::new(None);

/// Whether TIMER_MOTION is running. SetTimer on a live timer restarts its
/// countdown, so re-arming it on every mouse move would starve it of frames.
static MOTION_ARMED: AtomicBool = AtomicBool::new(false);

/// The motion timer's period: the shortest a window timer allows.
const MOTION_FRAME_MS: u32 = 10;

/// Corner radius of the background that shows on hover, in base pixels.
const PANEL_RADIUS: f64 = 6.0;

fn lock_dock() -> MutexGuard<'static, Option<DockState>> {
    DOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// Seconds on a monotonic clock, for the springs.
fn clock() -> f64 {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_secs_f64()
}

/// Screen pixels per 96-DPI pixel on the widget's monitor.
fn dpi_unit() -> f64 {
    CURRENT_DPI.load(Ordering::Relaxed) as f64 / 96.0
}

/// Screen pixels per base pixel of the widget: DPI and WIDGET SIZE.
fn widget_scale() -> f64 {
    dpi_unit() * WIDGET_SIZE.load(Ordering::Relaxed) as f64 / 100.0
}

fn arm_motion(hwnd: HWND) {
    if !MOTION_ARMED.swap(true, Ordering::Relaxed) {
        unsafe {
            SetTimer(hwnd, TIMER_MOTION, MOTION_FRAME_MS, None);
        }
    }
}

fn disarm_motion(hwnd: HWND) {
    if MOTION_ARMED.swap(false, Ordering::Relaxed) {
        unsafe {
            let _ = KillTimer(hwnd, TIMER_MOTION);
        }
    }
}

/// Where a widget with no saved spot docks: the primary monitor's right
/// edge, a third of the way down.
fn default_dock_anchor() -> [f64; 2] {
    unsafe {
        let monitor = MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY);
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if !GetMonitorInfoW(monitor, &mut info).as_bool() {
            return [0.0, 0.0];
        }
        let m = info.rcMonitor;
        [
            m.right as f64,
            m.top as f64 + (m.bottom - m.top) as f64 / 3.0,
        ]
    }
}

/// Start the dock at the saved spot, or where an older floating widget sat,
/// or the default.
fn init_dock(saved: Option<(i32, i32)>, legacy: Option<(i32, i32)>) {
    let anchor = saved
        .map(|(x, y)| [x as f64, y as f64])
        .or_else(|| {
            legacy.map(|(x, y)| {
                [
                    x as f64 + total_widget_width() as f64 / 2.0,
                    y as f64 + widget_sc(WIDGET_HEIGHT) as f64 / 2.0,
                ]
            })
        })
        .unwrap_or_else(default_dock_anchor);
    *lock_dock() = Some(DockState {
        motion: dock::Motion::new(anchor),
        last_tick: clock(),
        unsaved: false,
    });
}

/// Send the widget back to its default spot.
fn reset_dock(hwnd: HWND) {
    if let Some(d) = lock_dock().as_mut() {
        d.motion.set_anchor(default_dock_anchor());
        d.unsaved = true;
    }
    arm_motion(hwnd);
}

/// The button came up or capture was lost: end the press, throwing the
/// widget onto an edge if it was a drag.
fn finish_press(hwnd: HWND) {
    let pressed = match lock_dock().as_mut() {
        Some(d) if d.motion.pressed() => {
            if d.motion.release(clock(), dpi_unit()) {
                d.unsaved = true;
            }
            true
        }
        // No press: a capture change from the embedded widget's own drag,
        // which must not start the floating widget's motion timer.
        _ => false,
    };
    if pressed {
        arm_motion(hwnd);
    }
}

fn cursor_pos() -> Option<[f64; 2]> {
    let mut pt = POINT::default();
    unsafe { GetCursorPos(&mut pt) }
        .ok()
        .map(|_| [pt.x as f64, pt.y as f64])
}

/// Step the floating widget's motion and push the frame to its layered
/// window: position, size and per-pixel alpha in one UpdateLayeredWindow.
///
/// The background is transparent until the cursor is over the widget, so the
/// rings float on the desktop. Every pixel keeps at least alpha 1, which is
/// invisible but keeps the whole widget hit-testable: a fully transparent
/// pixel of a layered window passes the mouse through, and the hover could
/// never start.
fn render_docked(hwnd: HWND) {
    let (
        is_dark,
        session_pct,
        weekly_pct,
        session_text,
        weekly_text,
        paces,
        poll_interval_ms,
        visible,
        click_through,
    ) = {
        let state = lock_state();
        let Some(s) = state.as_ref() else {
            return;
        };
        let (session_pace, weekly_pace) = claude_pace_markers(s.data.as_ref());
        (
            s.is_dark,
            s.session_percent,
            s.weekly_percent,
            s.session_text.clone(),
            s.weekly_text.clone(),
            [session_pace, weekly_pace],
            s.poll_interval_ms,
            s.widget_visible,
            s.click_through,
        )
    };
    if !visible {
        disarm_motion(hwnd);
        return;
    }

    let monitors: Vec<dock::Rect> = native_interop::monitor_bounds()
        .iter()
        .map(|r| dock::Rect {
            left: r.left as f64,
            top: r.top as f64,
            right: r.right as f64,
            bottom: r.bottom as f64,
        })
        .collect();
    let now = clock();
    let over = !click_through
        && match (cursor_pos(), native_interop::get_window_rect_safe(hwnd)) {
            (Some([x, y]), Some(r)) => {
                x >= r.left as f64 && x < r.right as f64 && y >= r.top as f64 && y < r.bottom as f64
            }
            _ => false,
        };
    let (frame, save) = {
        let mut guard = lock_dock();
        let Some(d) = guard.as_mut() else {
            return;
        };
        if !d.motion.pressed() {
            d.motion.set_hovered(over, now);
        }
        let dt = now - d.last_tick;
        d.last_tick = now;
        let Some(frame) = d
            .motion
            .tick(now, dt, &monitors, widget_scale(), dpi_unit())
        else {
            return;
        };
        let save = (d.unsaved && !frame.moving).then(|| {
            d.unsaved = false;
            d.motion.anchor()
        });
        (frame, save)
    };
    if let Some([x, y]) = save {
        if let Some(s) = lock_state().as_mut() {
            s.dock_anchor = Some((x.round() as i32, y.round() as i32));
        }
        save_state_settings();
    }
    if frame.moving {
        arm_motion(hwnd);
    } else {
        disarm_motion(hwnd);
    }

    let live = step_anim(
        &[session_pct, weekly_pct, 0.0, 0.0, 0.0, 0.0, 0.0],
        &[&session_text, &weekly_text, "", "", "", "", ""],
        last_cpu_sample(),
        last_ram_sample(),
        poll_interval_ms,
    );

    let width = (frame.width.ceil() as i32).max(1);
    let height = (frame.height.ceil() as i32).max(1);
    unsafe {
        let screen_dc = GetDC(hwnd);
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                biHeight: -height,
                biPlanes: 1,
                biBitCount: 32,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
        let mem_dc = CreateCompatibleDC(screen_dc);
        let dib =
            CreateDIBSection(mem_dc, &bmi, DIB_RGB_COLORS, &mut bits, None, 0).unwrap_or_default();
        if dib.is_invalid() || bits.is_null() {
            let _ = DeleteDC(mem_dc);
            ReleaseDC(hwnd, screen_dc);
            return;
        }
        let old_bmp = SelectObject(mem_dc, dib);

        compose_docked(
            mem_dc,
            std::slice::from_raw_parts_mut(bits as *mut u32, (width * height) as usize),
            width,
            height,
            &frame,
            &live,
            is_dark,
            [&session_text, &weekly_text],
            paces,
        );

        let dst = POINT {
            x: frame.left.round() as i32,
            y: frame.top.round() as i32,
        };
        let size = SIZE {
            cx: width,
            cy: height,
        };
        let src = POINT { x: 0, y: 0 };
        let blend_fn = BLENDFUNCTION {
            BlendOp: 0,
            BlendFlags: 0,
            SourceConstantAlpha: 255,
            AlphaFormat: 1, // AC_SRC_ALPHA
        };
        let _ = UpdateLayeredWindow(
            hwnd,
            screen_dc,
            Some(&dst),
            Some(&size),
            mem_dc,
            Some(&src),
            COLORREF(0),
            Some(&blend_fn),
            ULW_ALPHA,
        );

        SelectObject(mem_dc, old_bmp);
        let _ = DeleteObject(dib);
        let _ = DeleteDC(mem_dc);
        ReleaseDC(hwnd, screen_dc);
    }
}

/// Draw one docked frame into `hdc`, whose 32-bit pixels are `pixels`, and
/// leave them premultiplied for UpdateLayeredWindow.
///
/// The background, the hairlines and the figures are drawn with GDI onto an
/// opaque background (ClearType needs one), then faded in with the hover; the
/// rings are rasterised with their own alpha and laid over the top.
#[allow(clippy::too_many_arguments)]
fn compose_docked(
    hdc: HDC,
    pixels: &mut [u32],
    width: i32,
    height: i32,
    frame: &dock::Frame,
    live: &LiveFrame,
    is_dark: bool,
    texts: [&str; 2],
    paces: [Option<f64>; 2],
) {
    let palette = appearance().palette(is_dark);
    let bg = palette[0];
    let ink = palette[1];
    let l = &frame.layout;
    let s = frame.scale;
    let px = |v: f64| (v * s).round() as i32;
    let thin = px(1.0).max(1);
    let a = frame.hover;

    let strings = localization::STRINGS;
    let readings = ring_readings(
        live,
        texts,
        paces,
        [strings.session_window, strings.weekly_window],
        is_dark,
        bg,
        anim_breath(),
    );

    unsafe {
        fill_box(hdc, 0, 0, width, height, &bg);
        let _ = SetBkMode(hdc, TRANSPARENT);

        if a > 0.003 {
            // The drag handle: two hairlines along the grab edge, on the top
            // when the widget is stacked.
            let (div_left, div_right) = if is_dark {
                (Color::new(80, 80, 80), Color::new(40, 40, 40))
            } else {
                (Color::new(160, 160, 160), Color::new(230, 230, 230))
            };
            let [hx, hy, hw, hh] = l.handle;
            if frame.stacked > 0.5 {
                fill_box(hdc, px(hx), px(hy), px(hw), px(2.0), &div_left);
                fill_box(hdc, px(hx), px(hy + 2.0), px(hw), px(1.0), &div_right);
            } else {
                fill_box(hdc, px(hx), px(hy), px(2.0), px(hh), &div_left);
                fill_box(hdc, px(hx + 2.0), px(hy), px(1.0), px(hh), &div_right);
            }

            let [sx, sy, sw, sh] = l.split;
            fill_box(
                hdc,
                px(sx),
                px(sy),
                px(sw).max(thin),
                px(sh).max(thin),
                &blend(bg, ink, 0.12),
            );

            // Poll freshness: a hairline along the side facing into the
            // screen that fills over the poll interval.
            let face = if frame.docked {
                frame.side
            } else {
                dock::Side::Bottom
            };
            let inset = px(6.0);
            let track = blend(bg, ink, 0.18);
            let lit = blend(bg, ink, 0.45);
            match face {
                dock::Side::Bottom | dock::Side::Top => {
                    let y = if face == dock::Side::Bottom {
                        0
                    } else {
                        height - thin
                    };
                    let len = width - 2 * inset;
                    fill_box(hdc, inset, y, len, thin, &track);
                    let filled = (len as f64 * live.poll_frac).round() as i32;
                    fill_box(hdc, inset, y, filled, thin, &lit);
                }
                dock::Side::Left | dock::Side::Right => {
                    let x = if face == dock::Side::Right {
                        0
                    } else {
                        width - thin
                    };
                    let len = height - 2 * inset;
                    fill_box(hdc, x, inset, thin, len, &track);
                    let filled = (len as f64 * live.poll_frac).round() as i32;
                    fill_box(hdc, x, inset, thin, filled, &lit);
                }
            }
        }

        // The figures fade in over the back half of the expansion and drift
        // out of their ring as they appear, each block clipped to its own side
        // of the hairline so neither runs over the other's ring mid-morph.
        let text_a = ((frame.expanded - 0.45) / 0.55).clamp(0.0, 1.0);
        if text_a > 0.01 {
            let figure_font = make_font(-12, FW_SEMIBOLD);
            let time_font = make_font(-10, FW_MEDIUM);
            let label_font = make_font(-9, FW_SEMIBOLD);
            let old_font = SelectObject(hdc, figure_font);
            let label_ink = blend(bg, ink, 0.65);
            let slide = (1.0 - text_a) * 10.0 * if frame.mirror { 1.0 } else { -1.0 };
            let t = frame.stacked;
            let mix = |a: f64, b: f64| a + (b - a) * t;
            let (sx, sy) = (l.split[0], l.split[1]);
            let clips = [
                [0.0, 0.0, mix(sx - 3.0, l.w), mix(l.h, sy)],
                [mix(sx + 1.0, 0.0), mix(0.0, sy + 1.0), l.w, l.h],
            ];
            let blocks = [
                (l.usage_text_x, l.usage[1], &readings[0], clips[0]),
                (l.device_text_x, l.device[1], &readings[1], clips[1]),
            ];
            for (text_x, ring_y, rows, clip) in blocks {
                let saved = SaveDC(hdc);
                let _ = IntersectClipRect(hdc, px(clip[0]), px(clip[1]), px(clip[2]), px(clip[3]));
                let tx = px(text_x + slide);
                for (_, row) in rows.iter() {
                    let top = px(ring_y + (row.y - RING_Y) as f64);
                    let bottom = top + px(ROW_H as f64);
                    SelectObject(hdc, label_font);
                    draw_text_in(
                        hdc,
                        RECT {
                            left: tx,
                            top,
                            right: tx + px(LABEL_W as f64),
                            bottom,
                        },
                        row.label,
                        &blend(bg, label_ink, text_a),
                        DT_LEFT | DT_VCENTER | DT_SINGLELINE,
                    );
                    let fx = tx + px(LABEL_W as f64);
                    SelectObject(hdc, figure_font);
                    draw_text_in(
                        hdc,
                        RECT {
                            left: fx,
                            top,
                            right: fx + px(FIGURE_W as f64),
                            bottom,
                        },
                        &row.figure,
                        &blend(bg, row.figure_color, text_a),
                        DT_LEFT | DT_VCENTER | DT_SINGLELINE,
                    );
                    if let Some((reset, color)) = row.reset {
                        let rx = fx + px(FIGURE_W as f64);
                        SelectObject(hdc, time_font);
                        draw_text_in(
                            hdc,
                            RECT {
                                left: rx,
                                top,
                                right: rx + px(RESET_W as f64),
                                bottom,
                            },
                            reset,
                            &blend(bg, color, text_a),
                            DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS,
                        );
                    }
                }
                let _ = RestoreDC(hdc, saved);
            }
            SelectObject(hdc, old_font);
            let _ = DeleteObject(figure_font);
            let _ = DeleteObject(time_font);
            let _ = DeleteObject(label_font);
        }
        let _ = GdiFlush();
    }

    // The rings, with their own alpha. Over a transparent background the
    // tracks and pace ticks are the ink at low opacity, so they read the same
    // over the panel and over whatever is behind the widget.
    let mut rings = vec![Premul::default(); pixels.len()];
    let d = px(RING_D as f64);
    let shadow = 0.34 * (1.0 - a);
    for (origin, block) in [(l.usage, &readings[0]), (l.device, &readings[1])] {
        let bands = block.each_ref().map(|(band, _)| RingBand {
            percent: band.percent,
            fill: band.fill,
            track: ink,
            track_alpha: 0.16 + 0.04 * (1.0 - a),
            tick: band
                .tick
                .map(|(pace, _)| (pace, pace_marker_color(is_dark))),
            tick_alpha: 0.65,
        });
        let ring = ring_pixels(
            d,
            RING_STROKE as f64 * s,
            RING_GAP as f64 * s,
            [&bands[0], &bands[1]],
            shadow,
        );
        let (ox, oy) = (px(origin[0]), px(origin[1]));
        for ry in 0..d {
            let y = oy + ry;
            if y < 0 || y >= height {
                continue;
            }
            for rx in 0..d {
                let x = ox + rx;
                if x < 0 || x >= width {
                    continue;
                }
                let p = ring[(ry * d + rx) as usize];
                if p.a > 0.0 {
                    let i = (y * width + x) as usize;
                    rings[i] = p.over(rings[i]);
                }
            }
        }
    }

    // Composite: the panel, rounded and faded by the hover, under the rings.
    let r = PANEL_RADIUS * s;
    let (w, h) = (width as f64, height as f64);
    for y in 0..height {
        for x in 0..width {
            let i = (y * width + x) as usize;
            let (cx, cy) = (x as f64 + 0.5, y as f64 + 0.5);
            let corner = (cx - cx.clamp(r, w - r)).hypot(cy - cy.clamp(r, h - r));
            let pa = (a * coverage(r - corner)) as f32;
            let rgb = pixels[i];
            let panel = Premul {
                r: ((rgb >> 16) & 0xFF) as f32 * pa,
                g: ((rgb >> 8) & 0xFF) as f32 * pa,
                b: (rgb & 0xFF) as f32 * pa,
                a: pa,
            };
            let out = rings[i].over(panel);
            let alpha = (out.a * 255.0).round().clamp(0.0, 255.0) as u32;
            pixels[i] = if alpha == 0 {
                0x0100_0000
            } else {
                let c = |v: f32| (v.round().max(0.0) as u32).min(alpha);
                alpha << 24 | c(out.r) << 16 | c(out.g) << 8 | c(out.b)
            };
        }
    }
}

/// The ring bands and their figure rows: usage (the five-hour window outside,
/// the weekly one inside), then the device (CPU outside, RAM inside).
fn ring_readings<'a>(
    frame: &LiveFrame,
    texts: [&'a str; 2],
    paces: [Option<f64>; 2],
    labels: [&'a str; 2],
    is_dark: bool,
    bg: Color,
    breath: f64,
) -> [[(RingBand, RingRow<'a>); 2]; 2] {
    let appearance = appearance();
    let palette = appearance.palette(is_dark);
    let custom = appearance.mode == Mode::Custom;
    let ink = palette[1];
    let ring_track = blend(bg, ink, 0.14);
    let lift = if is_dark {
        Color::from_hex("#FFFFFF")
    } else {
        Color::from_hex("#000000")
    };
    // The figure, flashed toward the state's core colour for a beat after a
    // reading lands so the change is what draws the eye, not the motion.
    let figure_ink = |led: &Led, flash: f64| {
        if custom {
            ink
        } else if is_dark {
            blend(ink, led.core, 0.30 + 0.60 * flash)
        } else {
            blend(ink, led.edge, 0.45 + 0.40 * flash)
        }
    };

    // Usage: the five-hour window on the outer ring, the weekly one inside.
    let base = if custom {
        let ident = palette[2];
        Led {
            edge: blend(ident, bg, 0.4),
            mid: ident,
            core: blend(ident, Color::new(255, 255, 255), 0.25),
            glow: ident,
        }
    } else {
        claude_led()
    };
    let usage = [
        (0usize, labels[0], paces[0], ROW1_Y),
        (1usize, labels[1], paces[1], ROW2_Y),
    ];
    let usage_bands = usage.map(|(slot, label, pace, row_y)| {
        let pct = frame.shown[slot].clamp(0.0, 100.0);
        let reading = readout::has_reading(texts[slot]);
        let burn = pace.map(|p| pct - p).unwrap_or(0.0);
        let led = state_led(pct, burn, &base);
        let row = RingRow {
            y: row_y,
            label,
            figure: readout::figure(pct, texts[slot]),
            figure_color: figure_ink(&led, frame.flash),
            reset: Some((
                readout::reset_label(texts[slot]),
                blend(ink, lift, 0.55 * frame.text_tick),
            )),
        };
        let band = RingBand {
            percent: if reading { pct } else { 0.0 },
            fill: blend(led.mid, led.core, 0.10 + 0.25 * breath),
            track: ring_track,
            tick: pace
                .filter(|_| reading)
                .map(|p| (p, blend(pace_marker_color(is_dark), bg, 0.35))),
            track_alpha: 1.0,
            tick_alpha: 1.0,
        };
        (band, row)
    });

    // The device: CPU on the outer ring, RAM inside it.
    let system = [("CPU", frame.cpu, ROW1_Y), ("RAM", frame.ram, ROW2_Y)];
    let system_bands = system.map(|(label, value, row_y)| {
        let value = value.clamp(0.0, 100.0);
        let led = load_led(value);
        let row = RingRow {
            y: row_y,
            label,
            figure: format!("{value:.0}%"),
            figure_color: figure_ink(&led, 0.0),
            reset: None,
        };
        let band = RingBand {
            percent: value,
            fill: blend(led.mid, led.core, 0.10 + 0.25 * breath),
            track: ring_track,
            tick: None,
            track_alpha: 1.0,
            tick_alpha: 1.0,
        };
        (band, row)
    });
    [usage_bands, system_bands]
}

/// Paint all widget content onto a DC with a given background color.
fn paint_content(
    hdc: HDC,
    width: i32,
    height: i32,
    is_dark: bool,
    bg: &Color,
    text_color: &Color,
    accent: &Color,
    track: &Color,
    strings: Strings,
    session_pct: f64,
    session_pace: Option<f64>,
    session_text: &str,
    weekly_pct: f64,
    weekly_pace: Option<f64>,
    weekly_text: &str,
    codex_session_pct: f64,
    codex_session_text: &str,
    codex_weekly_pct: f64,
    codex_weekly_text: &str,
    antigravity_session_pct: f64,
    antigravity_session_text: &str,
    antigravity_weekly_pct: f64,
    antigravity_weekly_text: &str,
    show_claude_code: bool,
    show_codex: bool,
    show_antigravity: bool,
    codex_accent: &Color,
    antigravity_accent: &Color,
    fable: (f64, String),
) {
    let poll_interval_ms = {
        let state = lock_state();
        state
            .as_ref()
            .map(|s| s.poll_interval_ms)
            .unwrap_or(POLL_1_MIN)
    };

    // The widget shows one provider's 5h and 7d windows and the device rings.
    // The bar-era accents, the other providers, the Fable row and the day band
    // drive nothing any more; the paint entrypoints still compute and pass them.
    let _ = (accent, codex_accent, antigravity_accent, track);
    let _ = (show_claude_code, show_codex, show_antigravity);

    let breath = anim_breath();
    let appearance = appearance();
    let palette = appearance.palette(is_dark);

    let targets = [
        session_pct,
        weekly_pct,
        codex_session_pct,
        codex_weekly_pct,
        antigravity_session_pct,
        antigravity_weekly_pct,
        fable.0,
    ];
    let texts = [
        session_text,
        weekly_text,
        codex_session_text,
        codex_weekly_text,
        antigravity_session_text,
        antigravity_weekly_text,
        fable.1.as_str(),
    ];
    let frame = step_anim(
        &targets,
        &texts,
        last_cpu_sample(),
        last_ram_sample(),
        poll_interval_ms,
    );

    unsafe {
        let client_rect = RECT {
            left: 0,
            top: 0,
            right: width,
            bottom: height,
        };
        let bg_brush = CreateSolidBrush(COLORREF(bg.to_colorref()));
        FillRect(hdc, &client_rect, bg_brush);
        let _ = DeleteObject(bg_brush);

        // Drag handle, unchanged: two hairlines the width of the grab area.
        let divider_h = widget_sc(25);
        let divider_top = (height - divider_h) / 2;
        let (div_left, div_right) = if is_dark {
            ((80, 80, 80), (40, 40, 40))
        } else {
            ((160, 160, 160), (230, 230, 230))
        };
        let left_brush = CreateSolidBrush(COLORREF(native_interop::colorref(
            div_left.0, div_left.1, div_left.2,
        )));
        let left_rect = RECT {
            left: 0,
            top: divider_top,
            right: widget_sc(2),
            bottom: divider_top + divider_h,
        };
        FillRect(hdc, &left_rect, left_brush);
        let _ = DeleteObject(left_brush);
        let right_brush = CreateSolidBrush(COLORREF(native_interop::colorref(
            div_right.0,
            div_right.1,
            div_right.2,
        )));
        let right_rect = RECT {
            left: widget_sc(2),
            top: divider_top,
            right: widget_sc(3),
            bottom: divider_top + divider_h,
        };
        FillRect(hdc, &right_rect, right_brush);
        let _ = DeleteObject(right_brush);

        let _ = SetBkMode(hdc, TRANSPARENT);

        let ink = palette[1];
        let label_ink = blend(*bg, ink, 0.65);
        let stroke = widget_sc(RING_STROKE) as f64;
        let gap = widget_sc(RING_GAP) as f64;
        let ring_d = widget_sc(RING_D);

        let figure_font = make_font(-12, FW_SEMIBOLD);
        let time_font = make_font(-10, FW_MEDIUM);
        let label_font = make_font(-9, FW_SEMIBOLD);
        let old_font = SelectObject(hdc, figure_font);

        let [usage_bands, system_bands] = ring_readings(
            &frame,
            [texts[0], texts[1]],
            [session_pace, weekly_pace],
            [strings.session_window, strings.weekly_window],
            is_dark,
            *bg,
            breath,
        );

        for (ring_x, bands) in [(USAGE_X, &usage_bands), (SYSTEM_X, &system_bands)] {
            let x = widget_sc(ring_x);
            draw_rings(
                hdc,
                x,
                widget_sc(RING_Y),
                ring_d,
                stroke,
                gap,
                [&bands[0].0, &bands[1].0],
                *bg,
            );
            let tx = x + widget_sc(TEXT_DX);
            for (_, row) in bands.iter() {
                let top = widget_sc(row.y);
                let bottom = top + widget_sc(ROW_H);
                SelectObject(hdc, label_font);
                draw_text_in(
                    hdc,
                    RECT {
                        left: tx,
                        top,
                        right: tx + widget_sc(LABEL_W),
                        bottom,
                    },
                    row.label,
                    &label_ink,
                    DT_LEFT | DT_VCENTER | DT_SINGLELINE,
                );
                let fx = tx + widget_sc(LABEL_W);
                SelectObject(hdc, figure_font);
                draw_text_in(
                    hdc,
                    RECT {
                        left: fx,
                        top,
                        right: fx + widget_sc(FIGURE_W),
                        bottom,
                    },
                    &row.figure,
                    &row.figure_color,
                    DT_LEFT | DT_VCENTER | DT_SINGLELINE,
                );
                if let Some((reset, color)) = row.reset {
                    let rx = fx + widget_sc(FIGURE_W);
                    SelectObject(hdc, time_font);
                    draw_text_in(
                        hdc,
                        RECT {
                            left: rx,
                            top,
                            right: rx + widget_sc(RESET_W),
                            bottom,
                        },
                        reset,
                        &color,
                        DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS,
                    );
                }
            }
        }

        fill_box(
            hdc,
            widget_sc(SPLIT_X),
            widget_sc(SPLIT_Y),
            widget_sc(1).max(1),
            widget_sc(SPLIT_H),
            &blend(*bg, ink, 0.12),
        );

        SelectObject(hdc, old_font);
        let _ = DeleteObject(figure_font);
        let _ = DeleteObject(time_font);
        let _ = DeleteObject(label_font);

        // Poll freshness: a hairline along the top edge that fills over the poll
        // interval and restarts when a reading lands. It is always moving, it
        // says how stale the figures below it are, and it puts your eye on the
        // row a moment before the count-up fires. It sits at the top because
        // the bottom of the widget belongs to the day band.
        let thin = widget_sc(1).max(1);
        let x0 = widget_sc(USAGE_X);
        let x1 = width - thin;
        if x1 > x0 {
            let y = 0;
            fill_box(hdc, x0, y, x1 - x0, thin, &blend(*bg, *text_color, 0.18));
            let filled = (((x1 - x0) as f64) * frame.poll_frac).round() as i32;
            fill_box(hdc, x0, y, filled, thin, &blend(*bg, *text_color, 0.45));
        }
    }
}

fn do_poll(send_hwnd: SendHwnd) {
    let hwnd = send_hwnd.to_hwnd();
    let (show_claude_code, show_codex, show_antigravity) = {
        let state = lock_state();
        state
            .as_ref()
            .map(|s| (s.show_claude_code, s.show_codex, s.show_antigravity))
            .unwrap_or((true, false, false))
    };

    match poller::poll(show_claude_code, show_codex, show_antigravity) {
        Ok(mut data) => {
            let mut state = lock_state();
            let mut landed = POLL_NO_READING;
            if let Some(s) = state.as_mut() {
                // A provider whose request failed this round keeps its last
                // reading rather than blanking while the others update.
                let previous = s.data.take();
                if let Some(previous) = previous.clone() {
                    if s.show_claude_code && data.claude_code.is_none() {
                        data.claude_code = previous.claude_code;
                    }
                    if s.show_codex && data.codex.is_none() {
                        data.codex = previous.codex;
                    }
                    if s.show_antigravity && data.antigravity.is_none() {
                        data.antigravity = previous.antigravity;
                    }
                }
                landed = if s.last_poll_ok && previous.as_ref() == Some(&data) {
                    POLL_UNCHANGED
                } else {
                    POLL_CHANGED
                };

                if let Some(claude_code) = data.claude_code.as_ref() {
                    s.session_percent = claude_code.session.percentage;
                    s.weekly_percent = claude_code.weekly.percentage;
                } else if s.show_claude_code {
                    s.session_percent = 0.0;
                    s.weekly_percent = 0.0;
                }
                if let Some(codex) = data.codex.as_ref() {
                    s.codex_session_percent = codex.session.percentage;
                    s.codex_weekly_percent = codex.weekly.percentage;
                } else if s.show_codex {
                    s.codex_session_percent = 0.0;
                    s.codex_weekly_percent = 0.0;
                }
                if let Some(antigravity) = data.antigravity.as_ref() {
                    s.antigravity_session_percent = antigravity.session.percentage;
                    s.antigravity_weekly_percent = antigravity.weekly.percentage;
                } else if s.show_antigravity {
                    s.antigravity_session_percent = 0.0;
                    s.antigravity_weekly_percent = 0.0;
                }
                // Stop fast-poll if reset data is now fresh
                if !poller::app_is_past_reset(&data) {
                    unsafe {
                        let _ = KillTimer(hwnd, TIMER_RESET_POLL);
                    }
                }

                s.data = Some(data);
                s.last_poll_ok = true;
                refresh_usage_texts(s);

                // Recovered from errors — restore normal poll interval
                if s.retry_count > 0 {
                    s.retry_count = 0;
                    let interval = s.poll_interval_ms;
                    unsafe {
                        SetTimer(hwnd, TIMER_POLL, interval, None);
                    }
                }
                s.force_notify_auth_error = false;
                s.auth_error_paused_polling = false;
                s.auth_watch_mode = poller::CredentialWatchMode::ActiveSource;
                s.auth_watch_snapshot.clear();
            }

            unsafe {
                let _ = PostMessageW(hwnd, WM_APP_USAGE_UPDATED, WPARAM(landed), LPARAM(0));
            }
        }
        Err(e) => {
            let auth_watch = match e {
                poller::PollError::AuthRequired
                | poller::PollError::TokenExpired
                | poller::PollError::NoCredentials
                    if show_codex && !show_claude_code && !show_antigravity =>
                {
                    Some((
                        poller::CredentialWatchMode::Codex,
                        poller::credential_watch_snapshot(poller::CredentialWatchMode::Codex),
                    ))
                }
                poller::PollError::AuthRequired | poller::PollError::TokenExpired
                    if show_antigravity && !show_claude_code && !show_codex =>
                {
                    Some((
                        poller::CredentialWatchMode::Antigravity,
                        poller::credential_watch_snapshot(poller::CredentialWatchMode::Antigravity),
                    ))
                }
                poller::PollError::AuthRequired | poller::PollError::TokenExpired => Some((
                    poller::CredentialWatchMode::ActiveSource,
                    poller::credential_watch_snapshot(poller::CredentialWatchMode::ActiveSource),
                )),
                poller::PollError::NoCredentials => Some((
                    poller::CredentialWatchMode::AllSources,
                    poller::credential_watch_snapshot(poller::CredentialWatchMode::AllSources),
                )),
                poller::PollError::RequestFailed => None,
            };
            // Distinguish auth-required errors from transient errors.
            let notify_auth_error = {
                let mut state = lock_state();
                let mut should_notify = false;
                if let Some(s) = state.as_mut() {
                    match auth_watch {
                        Some((watch_mode, watch_snapshot)) => {
                            s.last_poll_ok = false;
                            // Only show the balloon on the first failure so it doesn't spam.
                            if s.retry_count == 0 || s.force_notify_auth_error {
                                should_notify = true;
                            }
                            s.force_notify_auth_error = false;
                            s.auth_error_paused_polling = true;
                            s.auth_watch_mode = watch_mode;
                            s.auth_watch_snapshot = watch_snapshot;
                            s.session_text = "!".to_string();
                            s.weekly_text = "!".to_string();
                            s.codex_session_text = "!".to_string();
                            s.codex_weekly_text = "!".to_string();
                            s.antigravity_session_text = "!".to_string();
                            s.antigravity_weekly_text = "!".to_string();
                            s.retry_count = s.retry_count.saturating_add(1);
                            unsafe {
                                let _ = KillTimer(hwnd, TIMER_POLL);
                                let _ = KillTimer(hwnd, TIMER_RESET_POLL);
                                let _ = KillTimer(hwnd, TIMER_COUNTDOWN);
                                SetTimer(hwnd, TIMER_POLL, s.poll_interval_ms, None);
                            }
                        }
                        _ => {
                            // Transient network / credential-missing errors: exponential backoff.
                            s.force_notify_auth_error = false;
                            s.auth_error_paused_polling = false;
                            s.auth_watch_mode = poller::CredentialWatchMode::ActiveSource;
                            s.auth_watch_snapshot.clear();
                            // The last reading stays on screen through a failed
                            // request; only a widget that has never had one waits.
                            if s.data.is_none() {
                                s.last_poll_ok = false;
                                s.session_text = "...".to_string();
                                s.weekly_text = "...".to_string();
                                s.codex_session_text = "...".to_string();
                                s.codex_weekly_text = "...".to_string();
                                s.antigravity_session_text = "...".to_string();
                                s.antigravity_weekly_text = "...".to_string();
                            }
                            s.retry_count = s.retry_count.saturating_add(1);
                            let backoff = RETRY_BASE_MS.saturating_mul(
                                1u32.checked_shl(s.retry_count - 1).unwrap_or(u32::MAX),
                            );
                            let retry_ms = backoff.min(s.poll_interval_ms);
                            unsafe {
                                let _ = KillTimer(hwnd, TIMER_RESET_POLL);
                                SetTimer(hwnd, TIMER_POLL, retry_ms, None);
                            }
                        }
                    }
                }
                should_notify
            };

            if notify_auth_error {
                let balloon = {
                    let state = lock_state();
                    state.as_ref().map(|s| {
                        if s.show_claude_code {
                            (
                                localization::STRINGS,
                                tray_icon::TrayIconKind::Claude,
                                localization::STRINGS.token_expired_title,
                                localization::STRINGS.token_expired_body,
                            )
                        } else if s.show_codex {
                            (
                                localization::STRINGS,
                                tray_icon::TrayIconKind::Codex,
                                localization::STRINGS.codex_token_expired_title,
                                localization::STRINGS.codex_token_expired_body,
                            )
                        } else {
                            (
                                localization::STRINGS,
                                tray_icon::TrayIconKind::Antigravity,
                                localization::STRINGS.antigravity_token_expired_title,
                                localization::STRINGS.antigravity_token_expired_body,
                            )
                        }
                    })
                };
                if let Some((_strings, kind, title, body)) = balloon {
                    tray_icon::notify_balloon(hwnd, kind, title, body);
                }
            }

            unsafe {
                let _ = PostMessageW(hwnd, WM_APP_USAGE_UPDATED, WPARAM(0), LPARAM(0));
            }
        }
    }
}

fn schedule_countdown_timer() {
    let state = lock_state();
    let s = match state.as_ref() {
        Some(s) => s,
        None => return,
    };

    let hwnd = s.hwnd.to_hwnd();
    if !s.last_poll_ok {
        unsafe {
            let _ = KillTimer(hwnd, TIMER_COUNTDOWN);
            let _ = KillTimer(hwnd, TIMER_RESET_POLL);
        }
        return;
    }

    let data = match &s.data {
        Some(d) => d,
        None => return,
    };

    // If a reset time has passed, poll every 5s to pick up fresh data
    if poller::app_is_past_reset(data) {
        unsafe {
            SetTimer(hwnd, TIMER_RESET_POLL, 5_000, None);
        }
    }

    let session_change = |usage: &crate::models::UsageData| {
        poller::time_until_display_change(usage.session.resets_at, poller::WindowKind::Session)
    };
    let weekly_change = |usage: &crate::models::UsageData| {
        poller::time_until_display_change(usage.weekly.resets_at, poller::WindowKind::Weekly)
    };
    let delays = [
        data.claude_code.as_ref().and_then(session_change),
        data.claude_code.as_ref().and_then(weekly_change),
        data.claude_code.as_ref().and_then(|usage| {
            poller::time_until_display_change(usage.fable.resets_at, poller::WindowKind::Weekly)
        }),
        data.codex.as_ref().and_then(session_change),
        data.codex.as_ref().and_then(weekly_change),
        data.antigravity.as_ref().and_then(session_change),
        data.antigravity.as_ref().and_then(weekly_change),
    ];
    let min_delay = delays.into_iter().flatten().min();

    // While a pace marker is live, tick at least once a minute so the needle
    // keeps moving between countdown display changes (which can be hours
    // apart for long windows).
    let now = SystemTime::now();
    let live = |resets_at: Option<SystemTime>| matches!(resets_at, Some(t) if t.duration_since(now).is_ok());
    let pace_marker_live = s.show_claude_code
        && data.claude_code.as_ref().map_or(false, |usage| {
            live(usage.session.resets_at) || live(usage.weekly.resets_at)
        });

    let mut delay = min_delay.unwrap_or(Duration::from_secs(60));
    if pace_marker_live {
        delay = delay.min(Duration::from_secs(60));
    }
    let ms = delay.as_millis().max(1000) as u32;

    unsafe {
        SetTimer(hwnd, TIMER_COUNTDOWN, ms, None);
    }
}

fn check_theme_change() {
    let new_dark = appearance().is_dark(theme::is_dark_mode());
    let changed = {
        let mut state = lock_state();
        if let Some(s) = state.as_mut() {
            if s.is_dark != new_dark {
                s.is_dark = new_dark;
                true
            } else {
                false
            }
        } else {
            false
        }
    };
    if changed {
        render_layered();
    }
}

pub(crate) fn appearance() -> Appearance {
    lock_state()
        .as_ref()
        .map(|s| s.appearance.clone())
        .unwrap_or_default()
}

fn store_appearance(value: Appearance) {
    let dark = value.is_dark(theme::is_dark_mode());
    let mut state = lock_state();
    if let Some(s) = state.as_mut() {
        s.appearance = value;
        s.is_dark = dark;
    }
}

pub(crate) fn set_appearance(mut value: Appearance) {
    value.widget_size = value.clamped_widget_size();
    let resized = WIDGET_SIZE.swap(value.widget_size, Ordering::Relaxed) != value.widget_size;
    store_appearance(value);
    save_state_settings();
    if resized {
        // The floating widget stays docked where it is and re-lays out at the
        // new size on the next frame.
        position_at_taskbar();
    }
    render_layered();
}

/// Applies an appearance in memory and on screen without writing the settings file.
pub(crate) fn preview_appearance(value: Appearance) {
    store_appearance(value);
    render_layered();
}

fn update_display() {
    let mut state = lock_state();
    let s = match state.as_mut() {
        Some(s) => s,
        None => return,
    };

    // Don't overwrite error text with stale cached data
    if !s.last_poll_ok {
        return;
    }

    refresh_usage_texts(s);
}

fn suppress_tray_reposition_for(duration: Duration) {
    let mut until = SUPPRESS_TRAY_REPOSITION_UNTIL
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    *until = Some(Instant::now() + duration);
}

fn tray_reposition_is_suppressed() -> bool {
    let now = Instant::now();
    let mut until = SUPPRESS_TRAY_REPOSITION_UNTIL
        .lock()
        .unwrap_or_else(|e| e.into_inner());

    match *until {
        Some(deadline) if now < deadline => true,
        Some(_) => {
            *until = None;
            false
        }
        None => false,
    }
}

fn position_at_taskbar() {
    refresh_dpi();
    let floating = lock_state().as_ref().map(|s| !s.embedded).unwrap_or(false);
    if floating {
        position_floating_widget();
        return;
    }
    // Drop the app-state lock before any Win32 call that may synchronously
    // re-enter our window procedure.
    let (hwnd, embedded, tray_offset, taskbar_hwnd) = {
        let state = lock_state();
        let s = match state.as_ref() {
            Some(s) => s,
            None => return,
        };

        // Don't fight the user's drag
        if s.dragging {
            return;
        }

        let taskbar_hwnd = match s.taskbar_hwnd {
            Some(h) => h,
            None => {
                diagnose::log("position_at_taskbar skipped: no taskbar handle");
                return;
            }
        };

        (s.hwnd.to_hwnd(), s.embedded, s.tray_offset, taskbar_hwnd)
    };

    let taskbar_rect = match native_interop::get_taskbar_rect(taskbar_hwnd) {
        Some(r) => r,
        None => {
            diagnose::log("position_at_taskbar skipped: unable to query taskbar rect");
            return;
        }
    };

    let taskbar_height = taskbar_rect.bottom - taskbar_rect.top;
    let mut tray_left = taskbar_rect.right;
    let anchor_top = taskbar_rect.top;
    let anchor_height = taskbar_height;

    if let Some(tray_hwnd) = native_interop::find_child_window(taskbar_hwnd, "TrayNotifyWnd") {
        if let Some(tray_rect) = native_interop::get_window_rect_safe(tray_hwnd) {
            tray_left = tray_rect.left;
        }
    }

    let widget_width = total_widget_width();
    let max_offset = (tray_left - taskbar_rect.left - widget_width).max(0);
    let tray_offset = tray_offset.clamp(0, max_offset);
    let offset_changed = {
        let mut state = lock_state();
        if let Some(s) = state.as_mut() {
            if s.tray_offset != tray_offset {
                s.tray_offset = tray_offset;
                true
            } else {
                false
            }
        } else {
            false
        }
    };
    if offset_changed {
        save_state_settings();
    }

    let widget_height = widget_sc(WIDGET_HEIGHT);
    let y = compute_anchor_y(anchor_top, anchor_height, widget_height);
    if embedded {
        // Child window: coordinates relative to parent (taskbar)
        let x = tray_left - taskbar_rect.left - widget_width - tray_offset;
        native_interop::move_window(hwnd, x, y - taskbar_rect.top, widget_width, widget_height);
        diagnose::log(format!(
            "positioned embedded widget at x={x} y={} w={widget_width} h={widget_height}",
            y - taskbar_rect.top
        ));
    } else {
        // Topmost popup: screen coordinates
        let x = tray_left - widget_width - tray_offset;
        native_interop::move_window(hwnd, x, y, widget_width, widget_height);
        diagnose::log(format!(
            "positioned fallback widget at x={x} y={y} w={widget_width} h={widget_height}"
        ));
    }
}

fn compute_anchor_y(anchor_top: i32, anchor_height: i32, widget_height: i32) -> i32 {
    let anchor_bottom = anchor_top + anchor_height;
    (anchor_bottom - widget_height).max(anchor_top)
}

fn clamp_desktop_position(x: i32, y: i32, width: i32, height: i32, work: RECT) -> (i32, i32) {
    (
        x.clamp(work.left, (work.right - width).max(work.left)),
        y.clamp(work.top, (work.bottom - height).max(work.top)),
    )
}

/// Whether the pixel at (px, py) lies in some work area. A work area covers
/// left..right and top..bottom as half-open ranges, as Win32 RECTs do.
fn point_on_screen(px: i32, py: i32, works: &[RECT]) -> bool {
    works
        .iter()
        .any(|w| px >= w.left && px < w.right && py >= w.top && py < w.bottom)
}

/// Whether all four corner pixels of the rect are each inside some work area.
/// The corners are the rect's outermost pixels, (x, y) to (x + w - 1, y + h - 1),
/// so a widget sitting flush against a right or bottom edge still counts.
fn corners_on_screen(x: i32, y: i32, width: i32, height: i32, works: &[RECT]) -> bool {
    let right = x + width.max(1) - 1;
    let bottom = y + height.max(1) - 1;
    point_on_screen(x, y, works)
        && point_on_screen(right, y, works)
        && point_on_screen(x, bottom, works)
        && point_on_screen(right, bottom, works)
}

/// Position closest to the proposed top-left at which every corner of the
/// widget lies inside some monitor's work area. A proposal that already
/// satisfies that is returned unchanged, so the widget may straddle monitors.
fn keep_widget_on_screen(x: i32, y: i32, width: i32, height: i32, works: &[RECT]) -> (i32, i32) {
    if works.is_empty() || corners_on_screen(x, y, width, height, works) {
        return (x, y);
    }
    let distance = |(cx, cy): (i32, i32)| {
        let dx = (cx - x) as i64;
        let dy = (cy - y) as i64;
        dx * dx + dy * dy
    };
    let mut best: Option<(i32, i32)> = None;
    let mut fallback: Option<(i32, i32)> = None;
    for work in works {
        let inside = clamp_desktop_position(x, y, width, height, *work);
        if fallback.map_or(true, |f| distance(inside) < distance(f)) {
            fallback = Some(inside);
        }
        for candidate in [inside, (x, inside.1), (inside.0, y)] {
            if corners_on_screen(candidate.0, candidate.1, width, height, works)
                && best.map_or(true, |b| distance(candidate) < distance(b))
            {
                best = Some(candidate);
            }
        }
    }
    // Only a widget larger than every work area has no valid candidate; it is
    // then top-left aligned in the nearest work area.
    best.or(fallback).unwrap_or((x, y))
}

#[cfg(test)]
mod desktop_tests {
    use super::*;

    #[test]
    fn positions_are_kept_inside_the_monitor_work_area() {
        let work = RECT {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1040,
        };
        assert_eq!(clamp_desktop_position(300, 200, 384, 46, work), (300, 200));
        assert_eq!(clamp_desktop_position(2100, -40, 384, 46, work), (1536, 0));
        assert_eq!(
            clamp_desktop_position(1900, 1100, 384, 46, work),
            (1536, 994)
        );
    }

    #[test]
    fn negative_monitor_coordinates_and_small_displays_are_supported() {
        let work = RECT {
            left: -1280,
            top: -900,
            right: 0,
            bottom: 0,
        };
        assert_eq!(
            clamp_desktop_position(-900, -700, 384, 46, work),
            (-900, -700)
        );
        assert_eq!(clamp_desktop_position(-10, -10, 384, 46, work), (-384, -46));
        assert_eq!(
            clamp_desktop_position(0, 0, 2000, 1000, work),
            (-1280, -900)
        );
    }

    fn work(left: i32, top: i32, right: i32, bottom: i32) -> RECT {
        RECT {
            left,
            top,
            right,
            bottom,
        }
    }

    #[test]
    fn single_monitor_keeps_the_widget_inside_and_pulls_it_back_from_each_edge() {
        let works = [work(0, 0, 1920, 1040)];
        assert_eq!(keep_widget_on_screen(300, 200, 384, 46, &works), (300, 200));
        assert_eq!(
            keep_widget_on_screen(1536, 994, 384, 46, &works),
            (1536, 994)
        );
        assert_eq!(keep_widget_on_screen(-50, 200, 384, 46, &works), (0, 200));
        assert_eq!(
            keep_widget_on_screen(1700, 200, 384, 46, &works),
            (1536, 200)
        );
        assert_eq!(keep_widget_on_screen(300, -20, 384, 46, &works), (300, 0));
        assert_eq!(
            keep_widget_on_screen(300, 1020, 384, 46, &works),
            (300, 994)
        );
        assert_eq!(keep_widget_on_screen(-90, -90, 384, 46, &works), (0, 0));
    }

    #[test]
    fn widget_may_straddle_side_by_side_monitors_but_not_leave_the_top() {
        let works = [work(0, 0, 1920, 1040), work(1920, 0, 3840, 1040)];
        assert_eq!(
            keep_widget_on_screen(1800, 300, 384, 46, &works),
            (1800, 300)
        );
        assert_eq!(keep_widget_on_screen(1800, -30, 384, 46, &works), (1800, 0));
        assert_eq!(
            keep_widget_on_screen(1000, -10, 2500, 46, &works),
            (1000, 0)
        );
    }

    #[test]
    fn corners_in_the_gap_between_offset_monitors_are_moved_onto_a_screen() {
        let works = [work(0, 0, 1920, 1040), work(1920, 200, 3840, 1240)];
        let top = keep_widget_on_screen(1800, 100, 384, 46, &works);
        assert!(corners_on_screen(top.0, top.1, 384, 46, &works));
        assert_eq!(top, (1800, 200));
        let bottom = keep_widget_on_screen(1800, 1020, 384, 46, &works);
        assert!(corners_on_screen(bottom.0, bottom.1, 384, 46, &works));
        assert_eq!(bottom, (1800, 994));
    }

    #[test]
    fn widget_larger_than_every_monitor_is_top_left_aligned_in_the_nearest() {
        let works = [work(0, 0, 1920, 1040), work(1920, 0, 3840, 1040)];
        assert_eq!(keep_widget_on_screen(100, 100, 2500, 1200, &works), (0, 0));
        assert_eq!(
            keep_widget_on_screen(3000, 500, 2500, 1200, &works),
            (1920, 0)
        );
    }

    #[test]
    fn no_monitors_leaves_the_proposal_alone() {
        assert_eq!(
            keep_widget_on_screen(5000, 5000, 384, 46, &[]),
            (5000, 5000)
        );
    }

    #[test]
    fn widget_size_scales_widget_dimensions_on_top_of_dpi() {
        assert_eq!(scale_px(WIDGET_HEIGHT, 96, 100), 62);
        assert_eq!(scale_px(WIDGET_HEIGHT, 96, 200), 124);
        assert_eq!(scale_px(WIDGET_HEIGHT, 144, 200), 186);
        assert_eq!(widget_width_at(96, 100), 310);
        assert_eq!(widget_width_at(96, 200), 620);
        assert_eq!(widget_width_at(192, 100), widget_width_at(96, 200));
    }

    #[test]
    fn old_settings_migrate_without_losing_provider_choices() {
        let mut settings: SettingsFile = serde_json::from_str(r#"{"embed_in_taskbar":true,"click_through":true,"show_claude_code":false,"show_codex":true}"#).unwrap();
        migrate_desktop_settings(&mut settings);
        assert!(!settings.embed_in_taskbar && !settings.click_through);
        assert!(!settings.show_claude_code && settings.show_codex);
        settings.floating_position = Some((-800, 240));
        let saved = serde_json::to_string(&settings).unwrap();
        let restored: SettingsFile = serde_json::from_str(&saved).unwrap();
        assert_eq!(restored.floating_position, Some((-800, 240)));
        assert_eq!(restored.desktop_layout_version, 1);
    }

    #[test]
    fn saved_provider_choices_load_as_claude_code_only() {
        let mut settings: SettingsFile = serde_json::from_str(
            r#"{"show_claude_code":false,"show_codex":true,"show_antigravity":true}"#,
        )
        .unwrap();
        show_only_claude_code(&mut settings);
        assert!(settings.show_claude_code);
        assert!(!settings.show_codex && !settings.show_antigravity);
    }

    #[test]
    fn old_settings_with_a_language_key_still_load_and_drop_it_on_save() {
        let settings: SettingsFile = serde_json::from_str(
            r#"{"poll_interval_ms":60000,"language":"de","show_codex":false}"#,
        )
        .unwrap();
        assert_eq!(settings.poll_interval_ms, 60000);
        assert!(!settings.show_codex);
        let saved = serde_json::to_string(&settings).unwrap();
        assert!(!saved.contains("language"));
    }
}

/// The floating widget docks itself on every frame (see render_docked), so a
/// display or layout change only needs a fresh frame.
fn position_floating_widget() {
    render_layered();
}

/// WinEvent callback for tray icon location changes
/// Put the widget back on top after another window is brought to the front.
///
/// Only meaningful in the non-embedded (floating topmost) mode; an embedded
/// child window's z-order is resolved inside the taskbar's own hierarchy and
/// is not affected by foreground changes.
unsafe extern "system" fn on_foreground_changed(
    _hook: HWINEVENTHOOK,
    _event: u32,
    _hwnd: HWND,
    _id_object: i32,
    _id_child: i32,
    _thread: u32,
    _time: u32,
) {
    let target = {
        let state = lock_state();
        match state.as_ref() {
            Some(s) if !s.embedded && s.widget_visible && !MENU_OPEN.load(Ordering::Relaxed) => {
                Some(s.hwnd.to_hwnd())
            }
            _ => None,
        }
    };
    if let Some(hwnd) = target {
        native_interop::reassert_topmost(hwnd);
    }
}

unsafe extern "system" fn on_tray_location_changed(
    _hook: HWINEVENTHOOK,
    _event: u32,
    hwnd: HWND,
    _id_object: i32,
    _id_child: i32,
    _thread: u32,
    _time: u32,
) {
    static LAST_REPOSITION: Mutex<Option<std::time::Instant>> = Mutex::new(None);

    let is_tray = {
        let state = lock_state();
        state
            .as_ref()
            .and_then(|s| s.tray_notify_hwnd)
            .map(|h| h == hwnd)
            .unwrap_or(false)
    };

    if is_tray {
        if tray_reposition_is_suppressed() {
            return;
        }

        let should_reposition = {
            let mut last = LAST_REPOSITION.lock().unwrap_or_else(|e| e.into_inner());
            let now = std::time::Instant::now();
            if last
                .map(|t| now.duration_since(t).as_millis() > 500)
                .unwrap_or(true)
            {
                *last = Some(now);
                true
            } else {
                false
            }
        };
        if should_reposition {
            position_at_taskbar();
            render_layered();
        }
    }
}

/// Main window procedure
unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_PAINT => {
            // Both the embedded and the floating widget draw through
            // UpdateLayeredWindow, which does not use WM_PAINT; just validate.
            let mut ps = PAINTSTRUCT::default();
            let _ = BeginPaint(hwnd, &mut ps);
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_DISPLAYCHANGE | WM_DPICHANGED_MSG | WM_SETTINGCHANGE => {
            if msg == WM_DPICHANGED_MSG {
                let new_dpi = (wparam.0 & 0xFFFF) as u32;
                CURRENT_DPI.store(new_dpi, Ordering::Relaxed);
            }
            if msg == WM_SETTINGCHANGE {
                check_theme_change();
            }
            refresh_dpi();
            position_at_taskbar();
            render_layered();
            LRESULT(0)
        }
        WM_TIMER => {
            let timer_id = wparam.0;
            match timer_id {
                TIMER_POLL => {
                    let auth_watch = {
                        let state = lock_state();
                        state.as_ref().map(|s| {
                            (
                                s.auth_error_paused_polling,
                                s.auth_watch_mode,
                                s.auth_watch_snapshot.clone(),
                            )
                        })
                    };
                    match auth_watch {
                        Some((true, watch_mode, previous_snapshot)) => {
                            let current_snapshot = poller::credential_watch_snapshot(watch_mode);
                            if current_snapshot != previous_snapshot {
                                let mut state = lock_state();
                                if let Some(s) = state.as_mut() {
                                    if s.auth_error_paused_polling
                                        && s.auth_watch_mode == watch_mode
                                    {
                                        s.auth_watch_snapshot = current_snapshot;
                                    }
                                }
                                drop(state);
                                let sh = SendHwnd::from_hwnd(hwnd);
                                std::thread::spawn(move || {
                                    do_poll(sh);
                                });
                            }
                        }
                        Some((false, _, _)) => {
                            let sh = SendHwnd::from_hwnd(hwnd);
                            std::thread::spawn(move || {
                                do_poll(sh);
                            });
                        }
                        None => {}
                    }
                }
                TIMER_COUNTDOWN => {
                    update_display();
                    render_layered();
                    schedule_countdown_timer();
                }
                TIMER_RAM => {
                    // Sample only. TIMER_ANIM already repaints at 10fps and
                    // the paint eases toward this value, so the column drifts
                    // continuously instead of stepping on whole percents.
                    let visible = {
                        let state = lock_state();
                        state.as_ref().map(|s| s.widget_visible).unwrap_or(false)
                    };
                    if visible {
                        sample_cpu();
                        sample_ram();
                    }
                }
                TIMER_ANIM => {
                    // Ambient repaint that keeps the breathing glow and RAM ring
                    // moving. Skip while hidden so a hidden widget stays idle.
                    let visible = {
                        let state = lock_state();
                        state.as_ref().map(|s| s.widget_visible).unwrap_or(false)
                    };
                    if visible {
                        render_layered();
                    }
                }
                TIMER_MOTION => {
                    // Frames while the floating widget moves; render_docked
                    // stops this timer once everything settles.
                    render_layered();
                }
                TIMER_RESET_POLL => {
                    let should_poll = {
                        let state = lock_state();
                        state
                            .as_ref()
                            .map(|s| !s.auth_error_paused_polling)
                            .unwrap_or(false)
                    };
                    if should_poll {
                        let sh = SendHwnd::from_hwnd(hwnd);
                        std::thread::spawn(move || {
                            do_poll(sh);
                        });
                    }
                }
                TIMER_UPDATE_CHECK => {
                    begin_update_check(hwnd, false);
                }
                TIMER_TOPMOST => {
                    // Backstop for on_foreground_changed; a no-op when the
                    // widget is already on top.
                    let target = {
                        let state = lock_state();
                        match state.as_ref() {
                            Some(s)
                                if !s.embedded
                                    && s.widget_visible
                                    && !MENU_OPEN.load(Ordering::Relaxed) =>
                            {
                                Some(s.hwnd.to_hwnd())
                            }
                            _ => None,
                        }
                    };
                    if let Some(h) = target {
                        native_interop::reassert_topmost(h);
                    }
                }
                native_interop::TIMER_TASKBAR_RETRY => {
                    // The shell had no taskbar when we started. Keep trying to
                    // attach until it appears, then position against it and stop
                    // retrying. Reads the saved index/embed preference back from
                    // state so a retry behaves exactly like the startup attach.
                    let (index, embed, attached) = {
                        let state = lock_state();
                        match state.as_ref() {
                            Some(s) => (
                                s.taskbar_index,
                                s.embed_in_taskbar,
                                s.taskbar_hwnd.is_some(),
                            ),
                            None => (0, default_embed_in_taskbar(), false),
                        }
                    };
                    if attached || attach_to_taskbar(hwnd, index, embed) {
                        let _ = KillTimer(hwnd, native_interop::TIMER_TASKBAR_RETRY);
                        diagnose::log("taskbar retry: attached, positioning widget");
                        position_at_taskbar();
                        render_layered();
                    }
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_APP_USAGE_UPDATED => {
            match wparam.0 {
                POLL_NO_READING => {}
                landed => mark_poll(landed == POLL_CHANGED),
            }
            check_theme_change();
            render_layered();
            schedule_countdown_timer();
            suppress_tray_reposition_for(Duration::from_millis(
                TRAY_ICON_UPDATE_REPOSITION_SUPPRESS_MS,
            ));
            sync_tray_icons(hwnd);
            LRESULT(0)
        }
        WM_APP_UPDATE_CHECK_COMPLETE => {
            schedule_auto_update_check(hwnd);
            LRESULT(0)
        }
        WM_SETCURSOR => {
            let floating = lock_state().as_ref().map(|s| !s.embedded).unwrap_or(false);
            if floating && (lparam.0 & 0xffff) as u32 == HTCLIENT {
                SetCursor(LoadCursorW(HINSTANCE::default(), IDC_SIZEALL).unwrap_or_default());
                return LRESULT(1);
            }
            let is_dragging = {
                let state = lock_state();
                state.as_ref().map(|s| s.dragging).unwrap_or(false)
            };
            if is_dragging {
                let cursor = LoadCursorW(HINSTANCE::default(), IDC_SIZEWE).unwrap_or_default();
                SetCursor(cursor);
                return LRESULT(1);
            }
            if cursor_is_on_drag_handle(hwnd) {
                let cursor = LoadCursorW(HINSTANCE::default(), IDC_SIZEWE).unwrap_or_default();
                SetCursor(cursor);
                return LRESULT(1);
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_ENTERSIZEMOVE => {
            if let Some(s) = lock_state().as_mut() {
                s.dragging = true;
            }
            LRESULT(0)
        }
        WM_MOVING => {
            let floating = lock_state().as_ref().map(|s| !s.embedded).unwrap_or(false);
            if !floating || lparam.0 == 0 {
                return DefWindowProcW(hwnd, msg, wparam, lparam);
            }
            // Correct the proposed rect on every mouse move so no corner of the
            // widget can be dragged off the usable area of every display.
            let rect = &mut *(lparam.0 as *mut RECT);
            let width = rect.right - rect.left;
            let height = rect.bottom - rect.top;
            let works = native_interop::monitor_work_areas();
            let (x, y) = keep_widget_on_screen(rect.left, rect.top, width, height, &works);
            *rect = RECT {
                left: x,
                top: y,
                right: x + width,
                bottom: y + height,
            };
            LRESULT(1)
        }
        WM_EXITSIZEMOVE => {
            let rect = native_interop::get_window_rect_safe(hwnd);
            {
                let mut state = lock_state();
                if let Some(s) = state.as_mut() {
                    s.dragging = false;
                    if !s.embedded {
                        if let Some(rect) = rect {
                            s.floating_position = Some((rect.left, rect.top));
                        }
                    }
                }
            }
            position_at_taskbar();
            save_state_settings();
            render_layered();
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            let floating = lock_state().as_ref().map(|s| !s.embedded).unwrap_or(false);
            if floating {
                // The drag is the dock's own rather than the system move loop,
                // which could not magnet, throw or reshape the widget.
                if let Some(cursor) = cursor_pos() {
                    if let Some(d) = lock_dock().as_mut() {
                        d.motion.press(cursor, clock(), widget_scale());
                    }
                    SetCapture(hwnd);
                    arm_motion(hwnd);
                }
                return LRESULT(0);
            }
            let client_x = (lparam.0 & 0xFFFF) as i16 as i32;
            let client_y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
            if !is_drag_handle_point(client_x, client_y) {
                return LRESULT(0);
            }

            let mut pt = POINT::default();
            let _ = GetCursorPos(&mut pt);
            let mut state = lock_state();
            if let Some(s) = state.as_mut() {
                s.dragging = true;
                s.drag_start_mouse_x = pt.x;
                s.drag_start_client_x = client_x;
                s.drag_start_offset = s.tray_offset;
            }
            SetCapture(hwnd);
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            if lock_state().as_ref().map(|s| !s.embedded).unwrap_or(false) {
                if let Some(cursor) = cursor_pos() {
                    if let Some(d) = lock_dock().as_mut() {
                        if d.motion.pressed() {
                            d.motion.drag(cursor, clock(), dpi_unit());
                        }
                    }
                }
                // Also how a hover starts: the next frame sees the cursor.
                arm_motion(hwnd);
                return LRESULT(0);
            }
            let is_dragging = {
                let state = lock_state();
                state.as_ref().map(|s| s.dragging).unwrap_or(false)
            };
            if is_dragging {
                let mut pt = POINT::default();
                let _ = GetCursorPos(&mut pt);
                let move_target = {
                    let mut state = lock_state();
                    let s = match state.as_mut() {
                        Some(s) => s,
                        None => return LRESULT(0),
                    };

                    // Moving mouse left = positive delta = larger offset (further left)
                    let delta = s.drag_start_mouse_x - pt.x;
                    let mut new_offset = s.drag_start_offset + delta;

                    // Clamp: offset >= 0 (can't go right of default)
                    if new_offset < 0 {
                        new_offset = 0;
                    }

                    let taskbar_hwnd = s.taskbar_hwnd;
                    let embedded = s.embedded;
                    let hwnd_val = s.hwnd.to_hwnd();

                    // Clamp: don't go past left edge of taskbar
                    if let Some(taskbar_hwnd) = taskbar_hwnd {
                        if let Some(taskbar_rect) = native_interop::get_taskbar_rect(taskbar_hwnd) {
                            let mut tray_left = taskbar_rect.right;
                            if let Some(tray_hwnd) =
                                native_interop::find_child_window(taskbar_hwnd, "TrayNotifyWnd")
                            {
                                if let Some(tray_rect) =
                                    native_interop::get_window_rect_safe(tray_hwnd)
                                {
                                    tray_left = tray_rect.left;
                                }
                            }
                            let widget_width = total_widget_width();
                            let max_offset = (tray_left - taskbar_rect.left - widget_width).max(0);
                            if new_offset > max_offset {
                                new_offset = max_offset;
                            }

                            s.tray_offset = new_offset;

                            let taskbar_height = taskbar_rect.bottom - taskbar_rect.top;
                            let anchor_top = taskbar_rect.top;
                            let anchor_height = taskbar_height;
                            let widget_height = widget_sc(WIDGET_HEIGHT);
                            let y = compute_anchor_y(anchor_top, anchor_height, widget_height);
                            let x = if embedded {
                                tray_left - taskbar_rect.left - widget_width - new_offset
                            } else {
                                tray_left - widget_width - new_offset
                            };
                            Some((
                                hwnd_val,
                                embedded,
                                x,
                                y,
                                taskbar_rect.top,
                                widget_width,
                                widget_height,
                            ))
                        } else {
                            s.tray_offset = new_offset;
                            None
                        }
                    } else {
                        s.tray_offset = new_offset;
                        None
                    }
                };

                if let Some((hwnd_val, embedded, x, y, taskbar_top, widget_width, widget_height)) =
                    move_target
                {
                    if embedded {
                        native_interop::move_window(
                            hwnd_val,
                            x,
                            y - taskbar_top,
                            widget_width,
                            widget_height,
                        );
                    } else {
                        native_interop::move_window(hwnd_val, x, y, widget_width, widget_height);
                    }
                }
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            if lock_state().as_ref().map(|s| !s.embedded).unwrap_or(false) {
                finish_press(hwnd);
                let _ = ReleaseCapture();
                return LRESULT(0);
            }
            let mut pt = POINT::default();
            let _ = GetCursorPos(&mut pt);
            let drag_result = {
                let mut state = lock_state();
                if let Some(s) = state.as_mut() {
                    if s.dragging {
                        s.dragging = false;
                        Some((s.taskbar_index, s.drag_start_client_x))
                    } else {
                        None
                    }
                } else {
                    None
                }
            };
            if let Some((current_taskbar_index, drag_start_client_x)) = drag_result {
                let _ = ReleaseCapture();
                if let Some((target_index, target_taskbar)) = taskbar_at_point(pt) {
                    if target_index != current_taskbar_index {
                        let new_offset = offset_for_drop_point(
                            target_taskbar.hwnd,
                            target_taskbar.rect,
                            pt,
                            drag_start_client_x,
                        );
                        {
                            let mut state = lock_state();
                            if let Some(s) = state.as_mut() {
                                s.tray_offset = new_offset;
                            }
                        }
                        // Scoped so the state lock is released before
                        // attach_to_taskbar takes it again.
                        let embed = {
                            let state = lock_state();
                            state
                                .as_ref()
                                .map(|s| s.embed_in_taskbar)
                                .unwrap_or_else(default_embed_in_taskbar)
                        };
                        if attach_to_taskbar(hwnd, target_index, embed) {
                            position_at_taskbar();
                            render_layered();
                        }
                    }
                }
                save_state_settings();
            }
            LRESULT(0)
        }
        WM_CAPTURECHANGED => {
            // Capture taken away mid-drag (a menu, a focus change) ends the
            // drag where it is rather than leaving it stuck to the cursor.
            finish_press(hwnd);
            LRESULT(0)
        }
        WM_RBUTTONUP => {
            show_context_menu(hwnd);
            LRESULT(0)
        }
        WM_COMMAND => {
            let id = wparam.0 as u16;
            match id {
                IDM_APPEARANCE => crate::appearance_studio::open(hwnd),
                1 => {
                    {
                        let mut state = lock_state();
                        if let Some(s) = state.as_mut() {
                            s.force_notify_auth_error = true;
                        }
                    }
                    render_layered();
                    let sh = SendHwnd::from_hwnd(hwnd);
                    std::thread::spawn(move || {
                        do_poll(sh);
                    });
                }
                IDM_VERSION_ACTION => {
                    let (install_channel, release) = {
                        let state = lock_state();
                        match state.as_ref() {
                            Some(s) => (
                                s.install_channel,
                                match &s.update_status {
                                    UpdateStatus::Available(release) => Some(release.clone()),
                                    _ => None,
                                },
                            ),
                            None => (InstallChannel::Portable, None),
                        }
                    };

                    match install_channel {
                        InstallChannel::Winget => {
                            if release.is_some() {
                                begin_winget_update(hwnd);
                            } else {
                                begin_update_check(hwnd, true);
                            }
                        }
                        InstallChannel::Portable => {
                            if let Some(release) = release {
                                begin_update_apply(hwnd, release);
                            } else {
                                begin_update_check(hwnd, true);
                            }
                        }
                    }
                }
                2 => {
                    let hook = {
                        let state = lock_state();
                        state.as_ref().and_then(|s| s.win_event_hook)
                    };
                    if let Some(h) = hook {
                        native_interop::unhook_win_event(h);
                    }
                    PostQuitMessage(0);
                }
                IDM_RESET_POSITION => {
                    {
                        let mut state = lock_state();
                        if let Some(s) = state.as_mut() {
                            s.tray_offset = 0;
                            s.floating_position = None;
                            s.dock_anchor = None;
                        }
                    }
                    save_state_settings();
                    reset_dock(hwnd);
                    position_at_taskbar();
                }
                IDM_START_WITH_WINDOWS => {
                    set_startup_enabled(!is_startup_enabled());
                }
                IDM_FREQ_1MIN | IDM_FREQ_5MIN | IDM_FREQ_15MIN | IDM_FREQ_1HOUR => {
                    let new_interval = match id {
                        IDM_FREQ_1MIN => POLL_1_MIN,
                        IDM_FREQ_5MIN => POLL_5_MIN,
                        IDM_FREQ_15MIN => POLL_15_MIN,
                        IDM_FREQ_1HOUR => POLL_1_HOUR,
                        _ => POLL_15_MIN,
                    };
                    {
                        let mut state = lock_state();
                        if let Some(s) = state.as_mut() {
                            s.poll_interval_ms = new_interval;
                        }
                    }
                    save_state_settings();
                    // Reset the poll timer with the new interval
                    SetTimer(hwnd, TIMER_POLL, new_interval, None);
                }
                id if id == tray_icon::IDM_TOGGLE_WIDGET => {
                    toggle_widget_visibility(hwnd);
                }
                _ => {}
            }
            LRESULT(0)
        }
        _ if msg == WM_APP_TRAY => {
            match tray_icon::handle_message(lparam) {
                tray_icon::TrayAction::ToggleWidget => {
                    toggle_widget_visibility(hwnd);
                }
                tray_icon::TrayAction::ShowContextMenu => {
                    show_context_menu(hwnd);
                }
                tray_icon::TrayAction::None => {}
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            let (hook, fg_hook) = {
                let state = lock_state();
                match state.as_ref() {
                    Some(s) => (s.win_event_hook, s.foreground_hook),
                    None => (None, None),
                }
            };
            if let Some(h) = hook {
                native_interop::unhook_win_event(h);
            }
            // System-wide hook: leaving it installed would keep delivering
            // events to a dead callback.
            if let Some(h) = fg_hook {
                native_interop::unhook_win_event(h);
            }
            tray_icon::remove_all(hwnd);
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

fn show_context_menu(hwnd: HWND) {
    unsafe {
        let strings = localization::STRINGS;
        let (current_interval, install_channel, update_status, widget_visible) = {
            let state = lock_state();
            match state.as_ref() {
                Some(s) => (
                    s.poll_interval_ms,
                    s.install_channel,
                    s.update_status.clone(),
                    s.widget_visible,
                ),
                None => (
                    POLL_15_MIN,
                    InstallChannel::Portable,
                    UpdateStatus::Idle,
                    true,
                ),
            }
        };

        let menu = CreatePopupMenu().unwrap();
        let appearance_label = native_interop::wide_str("Appearance...");
        let _ = AppendMenuW(
            menu,
            MF_STRING,
            IDM_APPEARANCE as usize,
            PCWSTR::from_raw(appearance_label.as_ptr()),
        );
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());

        let refresh_str = native_interop::wide_str(strings.refresh);
        let _ = AppendMenuW(
            menu,
            MENU_ITEM_FLAGS(0),
            1,
            PCWSTR::from_raw(refresh_str.as_ptr()),
        );

        // Update Frequency submenu
        let freq_menu = CreatePopupMenu().unwrap();
        let freq_items: [(u16, u32, &str); 4] = [
            (IDM_FREQ_1MIN, POLL_1_MIN, strings.one_minute),
            (IDM_FREQ_5MIN, POLL_5_MIN, strings.five_minutes),
            (IDM_FREQ_15MIN, POLL_15_MIN, strings.fifteen_minutes),
            (IDM_FREQ_1HOUR, POLL_1_HOUR, strings.one_hour),
        ];
        for (id, interval, label) in freq_items {
            let label_str = native_interop::wide_str(label);
            let flags = if interval == current_interval {
                MF_CHECKED
            } else {
                MENU_ITEM_FLAGS(0)
            };
            let _ = AppendMenuW(
                freq_menu,
                flags,
                id as usize,
                PCWSTR::from_raw(label_str.as_ptr()),
            );
        }

        let freq_label = native_interop::wide_str(strings.update_frequency);
        let _ = AppendMenuW(
            menu,
            MF_POPUP,
            freq_menu.0 as usize,
            PCWSTR::from_raw(freq_label.as_ptr()),
        );

        // Settings submenu
        let settings_menu = CreatePopupMenu().unwrap();

        let startup_str = native_interop::wide_str(strings.start_with_windows);
        let startup_flags = if is_startup_enabled() {
            MF_CHECKED
        } else {
            MENU_ITEM_FLAGS(0)
        };
        let _ = AppendMenuW(
            settings_menu,
            startup_flags,
            IDM_START_WITH_WINDOWS as usize,
            PCWSTR::from_raw(startup_str.as_ptr()),
        );

        let reset_pos_str = native_interop::wide_str(strings.reset_position);
        let _ = AppendMenuW(
            settings_menu,
            MENU_ITEM_FLAGS(0),
            IDM_RESET_POSITION as usize,
            PCWSTR::from_raw(reset_pos_str.as_ptr()),
        );

        let _ = AppendMenuW(settings_menu, MF_SEPARATOR, 0, PCWSTR::null());

        let version_label = version_action_label(strings, install_channel, &update_status);
        let version_str = native_interop::wide_str(&version_label);
        let version_flags = if matches!(
            update_status,
            UpdateStatus::Checking | UpdateStatus::Applying
        ) {
            MF_GRAYED
        } else {
            MENU_ITEM_FLAGS(0)
        };
        let _ = AppendMenuW(
            settings_menu,
            version_flags,
            IDM_VERSION_ACTION as usize,
            PCWSTR::from_raw(version_str.as_ptr()),
        );

        let settings_label = native_interop::wide_str(strings.settings);
        let _ = AppendMenuW(
            menu,
            MF_POPUP,
            settings_menu.0 as usize,
            PCWSTR::from_raw(settings_label.as_ptr()),
        );

        let widget_label = native_interop::wide_str(strings.show_widget);
        let widget_flags = if widget_visible {
            MF_CHECKED
        } else {
            MENU_ITEM_FLAGS(0)
        };
        let _ = AppendMenuW(
            menu,
            widget_flags,
            tray_icon::IDM_TOGGLE_WIDGET as usize,
            PCWSTR::from_raw(widget_label.as_ptr()),
        );

        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());

        let exit_str = native_interop::wide_str(strings.exit);
        let _ = AppendMenuW(
            menu,
            MENU_ITEM_FLAGS(0),
            2,
            PCWSTR::from_raw(exit_str.as_ptr()),
        );

        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        // The menu is a topmost window too, so re-asserting the widget's
        // topmost place while it is open would lift the widget over the menu
        // and hide the items it overlaps. Hold the re-assert until it closes.
        MENU_OPEN.store(true, Ordering::Relaxed);
        let _ = SetForegroundWindow(hwnd);
        let _ = TrackPopupMenu(menu, TPM_RIGHTBUTTON, pt.x, pt.y, 0, hwnd, None);
        MENU_OPEN.store(false, Ordering::Relaxed);
        let _ = DestroyMenu(menu);
    }
}

/// Paint for non-embedded fallback (normal WM_PAINT path)
/// Fill a plain rectangle. Most of the numeric layout is one- and two-pixel
/// boxes, which do not want the rounded-rect machinery.
fn fill_box(hdc: HDC, x: i32, y: i32, w: i32, h: i32, color: &Color) {
    if w <= 0 || h <= 0 {
        return;
    }
    unsafe {
        let brush = CreateSolidBrush(COLORREF(color.to_colorref()));
        let rect = RECT {
            left: x,
            top: y,
            right: x + w,
            bottom: y + h,
        };
        FillRect(hdc, &rect, brush);
        let _ = DeleteObject(brush);
    }
}

/// A widget font, `px` designed at 96 DPI and 100% WIDGET SIZE.
fn make_font(px: i32, weight: FONT_WEIGHT) -> HFONT {
    create_font(widget_sc(px), weight)
}

/// A Segoe UI font at an already-scaled height, for callers that scale
/// themselves (the appearance studio uses `dpi_sc`).
pub(crate) fn create_font(height: i32, weight: FONT_WEIGHT) -> HFONT {
    let name = native_interop::wide_str("Segoe UI");
    unsafe {
        CreateFontW(
            height,
            0,
            0,
            0,
            weight.0 as i32,
            0,
            0,
            0,
            DEFAULT_CHARSET.0 as u32,
            OUT_TT_PRECIS.0 as u32,
            CLIP_DEFAULT_PRECIS.0 as u32,
            CLEARTYPE_QUALITY.0 as u32,
            (DEFAULT_PITCH.0 | FF_DONTCARE.0) as u32,
            PCWSTR::from_raw(name.as_ptr()),
        )
    }
}

pub(crate) fn draw_text_in(
    hdc: HDC,
    rect: RECT,
    text: &str,
    color: &Color,
    format: DRAW_TEXT_FORMAT,
) {
    // DrawTextW may inspect the pointer before honoring a zero character count.
    if text.is_empty() {
        return;
    }
    let mut wide: Vec<u16> = text.encode_utf16().collect();
    let mut r = rect;
    unsafe {
        let _ = SetTextColor(hdc, COLORREF(color.to_colorref()));
        let _ = DrawTextW(hdc, &mut wide, &mut r, format);
    }
}

/// One band of a ring gauge: how full it is, its colours, and an optional pace
/// tick across the band at the percentage an even spend would have reached.
/// `track_alpha` and `tick_alpha` are the opacities of the track and the tick:
/// 1 where they are already blended onto an opaque background, less over a
/// transparent one.
struct RingBand {
    percent: f64,
    fill: Color,
    track: Color,
    track_alpha: f64,
    tick: Option<(f64, Color)>,
    tick_alpha: f64,
}

/// The figures beside a ring, one row per band: a label, the percentage, and
/// on usage rows the time to reset.
struct RingRow<'a> {
    y: i32,
    label: &'a str,
    figure: String,
    figure_color: Color,
    reset: Option<(&'a str, Color)>,
}

/// Pixel coverage for a distance inside an edge, ramped over one pixel.
fn coverage(inside: f64) -> f64 {
    (inside + 0.5).clamp(0.0, 1.0)
}

/// A premultiplied pixel: the colour channels are already scaled by `a`.
#[derive(Clone, Copy, Default)]
struct Premul {
    r: f32,
    g: f32,
    b: f32,
    a: f32,
}

impl Premul {
    /// Paint `color` at `alpha` over this pixel.
    fn paint(&mut self, color: Color, alpha: f64) {
        let a = alpha.clamp(0.0, 1.0) as f32;
        if a <= 0.0 {
            return;
        }
        let k = 1.0 - a;
        self.r = color.r as f32 * a + self.r * k;
        self.g = color.g as f32 * a + self.g * k;
        self.b = color.b as f32 * a + self.b * k;
        self.a = a + self.a * k;
    }

    /// Lay this pixel over `below`.
    fn over(self, below: Premul) -> Premul {
        let k = 1.0 - self.a;
        Premul {
            r: self.r + below.r * k,
            g: self.g + below.g * k,
            b: self.b + below.b * k,
            a: self.a + below.a * k,
        }
    }
}

/// Two concentric ring gauges in a `d`-pixel square, outer band first, each
/// filling clockwise from twelve o'clock with round caps, as premultiplied
/// pixels with nothing behind them. `shadow` lays a soft dark band of that
/// opacity under each ring, which keeps them legible over a busy desktop.
///
/// GDI's arcs are not anti-aliased, so the rings are rasterised here with
/// per-pixel coverage.
fn ring_pixels(d: i32, stroke: f64, gap: f64, bands: [&RingBand; 2], shadow: f64) -> Vec<Premul> {
    let mut pixels = vec![Premul::default(); (d.max(0) * d.max(0)) as usize];
    if d <= 0 {
        return pixels;
    }
    let tau = std::f64::consts::TAU;
    let c = d as f64 / 2.0;
    let half = stroke / 2.0;
    let shadow_half = half + stroke * 0.26;
    // Per band, outer first: its centre-line radius, how far round it fills,
    // where the fill's end cap sits, and the pace tick's direction.
    let mids = [c - half, c - half - stroke - gap];
    let geometry = [0, 1].map(|i| {
        let mid = mids[i];
        let sweep = bands[i].percent.clamp(0.0, 100.0) / 100.0 * tau;
        let end_cap = (mid * sweep.sin(), -mid * sweep.cos());
        let tick = bands[i].tick.map(|(pace, color)| {
            let a = pace.clamp(0.0, 100.0) / 100.0 * tau;
            (a.sin(), -a.cos(), color)
        });
        (mid, sweep, end_cap, tick)
    });
    // Nothing is drawn inside the inner band or outside the outer one.
    let inner_edge = geometry[1].0 - shadow_half - 2.0;
    for py in 0..d {
        for px in 0..d {
            let dx = px as f64 + 0.5 - c;
            let dy = py as f64 + 0.5 - c;
            let dist = dx.hypot(dy);
            if dist < inner_edge || dist > c + 2.0 {
                continue;
            }
            let pixel = &mut pixels[(py * d + px) as usize];
            if shadow > 0.0 {
                for (mid, ..) in geometry {
                    let off = (dist - mid).abs();
                    pixel.paint(Color::new(0, 0, 0), shadow * coverage(shadow_half - off));
                }
            }
            // Clockwise from twelve o'clock, 0..TAU.
            let angle = dx.atan2(-dy).rem_euclid(tau);
            for (band, (mid, sweep, end_cap, tick)) in bands.iter().zip(geometry) {
                let off = (dist - mid).abs();
                pixel.paint(band.track, band.track_alpha * coverage(half - off));
                if sweep > 0.0 {
                    let edge = if sweep >= tau || angle <= sweep {
                        off
                    } else {
                        // Round caps: the distance to the band's centre line
                        // at either end of the arc.
                        let start = dx.hypot(dy + mid);
                        let end = (dx - end_cap.0).hypot(dy - end_cap.1);
                        start.min(end)
                    };
                    pixel.paint(band.fill, coverage(half - edge));
                }
                if let Some((ux, uy, tick)) = tick {
                    let along = dx * ux + dy * uy;
                    let across = (dx * uy - dy * ux).abs();
                    let cov = coverage(stroke * 0.14 - across)
                        * coverage(half + 1.0 - (along - mid).abs());
                    pixel.paint(tick, band.tick_alpha * cov);
                }
            }
        }
    }
    pixels
}

/// The ring gauges drawn into `hdc` at (x, y) over the opaque `bg`, which is
/// the same fake translucency `blend` documents.
fn draw_rings(
    hdc: HDC,
    x: i32,
    y: i32,
    d: i32,
    stroke: f64,
    gap: f64,
    bands: [&RingBand; 2],
    bg: Color,
) {
    if d <= 0 {
        return;
    }
    let ground = Premul {
        r: bg.r as f32,
        g: bg.g as f32,
        b: bg.b as f32,
        a: 1.0,
    };
    let pixels: Vec<u32> = ring_pixels(d, stroke, gap, bands, 0.0)
        .into_iter()
        .map(|p| {
            let p = p.over(ground);
            (p.r.round() as u32) << 16 | (p.g.round() as u32) << 8 | p.b.round() as u32
        })
        .collect();
    let info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: d,
            biHeight: -d,
            biPlanes: 1,
            biBitCount: 32,
            ..Default::default()
        },
        ..Default::default()
    };
    unsafe {
        SetDIBitsToDevice(
            hdc,
            x,
            y,
            d as u32,
            d as u32,
            0,
            0,
            0,
            d as u32,
            pixels.as_ptr() as *const _,
            &info,
            DIB_RGB_COLORS,
        );
    }
}

/// Render the real GDI widget with fixtures, without starting any live services.
pub fn write_preview(path: &str, dark: bool, unavailable: bool) -> std::io::Result<()> {
    CURRENT_DPI.store(192, Ordering::Relaxed);
    WIDGET_SIZE.store(WIDGET_SIZE_DEFAULT, Ordering::Relaxed);
    RAM_SAMPLE.store(4200, Ordering::Relaxed);
    CPU_SAMPLE.store(1800, Ordering::Relaxed);
    let width = total_widget_width();
    let height = widget_sc(WIDGET_HEIGHT);
    let bg = Color::from_hex(if dark { "#1C1C1C" } else { "#F3F3F3" });
    let ink = Color::from_hex(if dark { "#D7DEE6" } else { "#252B32" });
    let track = blend(bg, ink, 0.14);
    let pixels = unsafe {
        let dc = CreateCompatibleDC(None);
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                biHeight: -height,
                biPlanes: 1,
                biBitCount: 32,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits = std::ptr::null_mut();
        let bitmap = match CreateDIBSection(dc, &info, DIB_RGB_COLORS, &mut bits, None, 0) {
            Ok(bitmap) => bitmap,
            Err(_) => {
                let _ = DeleteDC(dc);
                return Err(std::io::Error::other("DIB allocation failed"));
            }
        };
        let old = SelectObject(dc, bitmap);
        paint_content(
            dc,
            width,
            height,
            dark,
            &bg,
            &ink,
            &claude_accent_color(),
            &track,
            localization::STRINGS,
            32.0,
            Some(45.0),
            "3h 12m",
            58.0,
            Some(65.0),
            "4d 8h",
            76.0,
            if unavailable { "!" } else { "1h 42m" },
            91.0,
            if unavailable { "n/a" } else { "2d 6h" },
            0.0,
            "n/a",
            0.0,
            "n/a",
            true,
            true,
            false,
            &codex_accent_color(dark),
            &antigravity_accent_color(),
            (38.0, if unavailable { "n/a" } else { "4d 8h" }.to_owned()),
        );
        let _ = GdiFlush();
        let pixels =
            std::slice::from_raw_parts(bits as *const u8, (width * height * 4) as usize).to_vec();
        SelectObject(dc, old);
        let _ = DeleteObject(bitmap);
        let _ = DeleteDC(dc);
        pixels
    };
    // BITMAPFILEHEADER + BITMAPINFOHEADER, explicitly encoded to avoid struct padding.
    let mut bmp = Vec::with_capacity(54 + pixels.len());
    bmp.extend_from_slice(b"BM");
    bmp.extend_from_slice(&((54 + pixels.len()) as u32).to_le_bytes());
    bmp.extend_from_slice(&[0; 4]);
    bmp.extend_from_slice(&54u32.to_le_bytes());
    bmp.extend_from_slice(&40u32.to_le_bytes());
    bmp.extend_from_slice(&width.to_le_bytes());
    bmp.extend_from_slice(&(-height).to_le_bytes());
    bmp.extend_from_slice(&1u16.to_le_bytes());
    bmp.extend_from_slice(&32u16.to_le_bytes());
    bmp.extend_from_slice(&[0; 24]);
    bmp.extend_from_slice(&pixels);
    std::fs::write(path, bmp)
}

pub(crate) fn draw_rounded_rect(hdc: HDC, rect: &RECT, color: &Color, radius: i32) {
    unsafe {
        let brush = CreateSolidBrush(COLORREF(color.to_colorref()));
        let rgn = CreateRoundRectRgn(
            rect.left,
            rect.top,
            rect.right + 1,
            rect.bottom + 1,
            radius * 2,
            radius * 2,
        );
        let _ = FillRgn(hdc, rgn, brush);
        let _ = DeleteObject(rgn);
        let _ = DeleteObject(brush);
    }
}
