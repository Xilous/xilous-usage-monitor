//! Keeps a Start menu shortcut pointing at the running executable.
//!
//! The app ships as a single portable exe with no installer, and Windows
//! search only lists apps that have a shortcut under the Start menu's
//! Programs folder. So the app adds its own shortcut on first launch, and on
//! every later launch points an existing one back at itself if the exe has
//! moved. A shortcut the user deleted stays deleted: once one has been made,
//! the settings remember it and a missing shortcut is not recreated.

use std::path::{Path, PathBuf};

use windows::core::{Interface, HSTRING};
use windows::Win32::Foundation::{MAX_PATH, RPC_E_CHANGED_MODE};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, IPersistFile,
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, STGM_READ,
};
use windows::Win32::UI::Shell::{
    FOLDERID_Programs, IShellLinkW, SHGetKnownFolderPath, ShellLink, KF_FLAG_DEFAULT,
};

use crate::diagnose;

/// File name of the shortcut in the per-user Start menu Programs folder. The
/// display name Windows search matches on is this name without `.lnk`.
const SHORTCUT_FILE_NAME: &str = "Xilous Usage Monitor.lnk";
const SHORTCUT_DESCRIPTION: &str = "Xilous Usage Monitor";

/// What is at the shortcut's path right now.
#[derive(Debug, PartialEq, Eq)]
enum Existing {
    Missing,
    PointsTo(PathBuf),
    /// The file is there but could not be read as a shortcut.
    Unreadable,
}

#[derive(Debug, PartialEq, Eq)]
enum Action {
    Nothing,
    Create,
    Repoint,
}

fn decide(existing: &Existing, current_exe: &Path, made_before: bool) -> Action {
    match existing {
        Existing::Missing if made_before => Action::Nothing,
        Existing::Missing => Action::Create,
        Existing::PointsTo(target) if same_path(target, current_exe) => Action::Nothing,
        Existing::PointsTo(_) | Existing::Unreadable => Action::Repoint,
    }
}

fn same_path(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| {
        p.to_string_lossy()
            .replace('/', "\\")
            .trim_end_matches('\\')
            .to_lowercase()
    };
    norm(a) == norm(b)
}

/// Creates or repoints the shortcut as needed. `made_before` is the saved
/// flag; the return value is the flag to save: true once a shortcut exists,
/// so a later deletion by the user is respected.
///
/// Debug builds leave the shortcut alone, so `cargo run` never points the
/// developer's own Start menu entry at `target\debug`.
pub fn sync(made_before: bool) -> bool {
    if cfg!(debug_assertions) {
        return made_before;
    }
    match sync_inner(made_before) {
        Ok(made) => made,
        Err(error) => {
            diagnose::log(format!("start menu shortcut sync failed: {error}"));
            made_before
        }
    }
}

fn sync_inner(made_before: bool) -> Result<bool, String> {
    let current_exe =
        std::env::current_exe().map_err(|e| format!("unable to locate current exe: {e}"))?;
    let _com = ComGuard::init()?;
    sync_in(&programs_folder()?, &current_exe, made_before)
}

/// The shortcut work for a given folder and exe; COM must be initialised.
fn sync_in(folder: &Path, current_exe: &Path, made_before: bool) -> Result<bool, String> {
    let shortcut = folder.join(SHORTCUT_FILE_NAME);
    let existing = read_existing(&shortcut);
    match decide(&existing, current_exe, made_before) {
        Action::Nothing => Ok(made_before || existing != Existing::Missing),
        action => {
            write_shortcut(&shortcut, current_exe)?;
            diagnose::log(format!(
                "start menu shortcut {action:?}: {} -> {}",
                shortcut.display(),
                current_exe.display()
            ));
            Ok(true)
        }
    }
}

/// Balances a successful CoInitializeEx with CoUninitialize. When the thread
/// already has COM in another mode, it is used as it is and left alone.
struct ComGuard {
    owned: bool,
}

impl ComGuard {
    fn init() -> Result<Self, String> {
        let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        if hr == RPC_E_CHANGED_MODE {
            return Ok(Self { owned: false });
        }
        hr.ok().map_err(|e| format!("CoInitializeEx: {e}"))?;
        Ok(Self { owned: true })
    }
}

impl Drop for ComGuard {
    fn drop(&mut self) {
        if self.owned {
            unsafe { CoUninitialize() };
        }
    }
}

fn programs_folder() -> Result<PathBuf, String> {
    unsafe {
        let raw = SHGetKnownFolderPath(&FOLDERID_Programs, KF_FLAG_DEFAULT, None)
            .map_err(|e| format!("SHGetKnownFolderPath(Programs): {e}"))?;
        let path = raw.to_string();
        CoTaskMemFree(Some(raw.0 as *const _));
        path.map(PathBuf::from)
            .map_err(|e| format!("Programs folder path is not valid UTF-16: {e}"))
    }
}

fn read_existing(shortcut: &Path) -> Existing {
    if !shortcut.exists() {
        return Existing::Missing;
    }
    let target = unsafe {
        (|| -> windows::core::Result<String> {
            let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
            link.cast::<IPersistFile>()?
                .Load(&HSTRING::from(shortcut.as_os_str()), STGM_READ)?;
            let mut buf = [0u16; MAX_PATH as usize];
            link.GetPath(&mut buf, std::ptr::null_mut(), 0)?;
            let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
            Ok(String::from_utf16_lossy(&buf[..len]))
        })()
    };
    match target {
        Ok(path) if !path.is_empty() => Existing::PointsTo(PathBuf::from(path)),
        _ => Existing::Unreadable,
    }
}

fn write_shortcut(shortcut: &Path, exe: &Path) -> Result<(), String> {
    unsafe {
        (|| -> windows::core::Result<()> {
            let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
            let exe_str = HSTRING::from(exe.as_os_str());
            link.SetPath(&exe_str)?;
            if let Some(dir) = exe.parent() {
                link.SetWorkingDirectory(&HSTRING::from(dir.as_os_str()))?;
            }
            link.SetIconLocation(&exe_str, 0)?;
            link.SetDescription(&HSTRING::from(SHORTCUT_DESCRIPTION))?;
            link.cast::<IPersistFile>()?
                .Save(&HSTRING::from(shortcut.as_os_str()), true)
        })()
    }
    .map_err(|e| format!("unable to save {}: {e}", shortcut.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXE: &str = r"C:\Users\me\Documents\xilous-usage-monitor.exe";

    #[test]
    fn first_launch_creates_the_shortcut() {
        assert_eq!(
            decide(&Existing::Missing, Path::new(EXE), false),
            Action::Create
        );
    }

    #[test]
    fn a_shortcut_the_user_deleted_stays_deleted() {
        assert_eq!(
            decide(&Existing::Missing, Path::new(EXE), true),
            Action::Nothing
        );
    }

    #[test]
    fn a_shortcut_to_this_exe_is_left_alone_whatever_the_case() {
        let existing = Existing::PointsTo(PathBuf::from(
            r"c:\users\ME\documents\Xilous-Usage-Monitor.EXE",
        ));
        for made_before in [false, true] {
            assert_eq!(
                decide(&existing, Path::new(EXE), made_before),
                Action::Nothing
            );
        }
    }

    #[test]
    fn a_shortcut_to_a_moved_exe_is_repointed() {
        let existing = Existing::PointsTo(PathBuf::from(
            r"C:\Users\me\Downloads\xilous-usage-monitor.exe",
        ));
        assert_eq!(decide(&existing, Path::new(EXE), true), Action::Repoint);
    }

    #[test]
    fn an_unreadable_shortcut_is_rewritten() {
        assert_eq!(
            decide(&Existing::Unreadable, Path::new(EXE), true),
            Action::Repoint
        );
    }

    /// Writes and reads real .lnk files through COM, in a temp folder rather
    /// than the Start menu.
    #[test]
    fn shortcut_is_created_read_back_repointed_and_deletion_respected() {
        let dir = std::env::temp_dir().join(format!("xum-start-menu-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let old_exe = dir.join("old").join("xilous-usage-monitor.exe");
        let new_exe = dir.join("new").join("xilous-usage-monitor.exe");
        for exe in [&old_exe, &new_exe] {
            std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
            std::fs::write(exe, b"").unwrap();
        }
        let shortcut = dir.join(SHORTCUT_FILE_NAME);
        let _com = ComGuard::init().unwrap();

        assert!(sync_in(&dir, &old_exe, false).unwrap());
        assert_eq!(
            read_existing(&shortcut),
            Existing::PointsTo(old_exe.clone())
        );

        assert!(sync_in(&dir, &new_exe, true).unwrap());
        assert_eq!(
            read_existing(&shortcut),
            Existing::PointsTo(new_exe.clone())
        );

        // A shortcut that was already there before the flag existed counts.
        assert!(sync_in(&dir, &new_exe, false).unwrap());

        std::fs::write(&shortcut, b"not a shortcut").unwrap();
        assert_eq!(read_existing(&shortcut), Existing::Unreadable);
        assert!(sync_in(&dir, &new_exe, true).unwrap());
        assert_eq!(
            read_existing(&shortcut),
            Existing::PointsTo(new_exe.clone())
        );

        std::fs::remove_file(&shortcut).unwrap();
        assert!(sync_in(&dir, &new_exe, true).unwrap());
        assert!(!shortcut.exists());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
