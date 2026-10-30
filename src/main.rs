#![windows_subsystem = "windows"]

mod appearance;
mod appearance_studio;
mod diagnose;
mod dock;
mod localization;
mod models;
mod native_interop;
mod poller;
mod readout;
mod theme;
mod tray_icon;
mod updater;
mod window;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|arg| arg == "--check-codex") {
        // Read-only diagnostic. Never print credentials or raw HTTP responses.
        match poller::poll(false, true, false) {
            Ok(data) => {
                if let Some(usage) = data.codex {
                    println!(
                        "Codex: short={}, long={}",
                        poller::format_line(
                            &usage.session,
                            poller::WindowKind::Session,
                            localization::STRINGS
                        ),
                        poller::format_line(
                            &usage.weekly,
                            poller::WindowKind::Weekly,
                            localization::STRINGS
                        )
                    );
                }
                std::process::exit(0);
            }
            Err(error) => {
                eprintln!("Codex usage unavailable: {error:?}");
                std::process::exit(1);
            }
        }
    }
    // Offline UI harness: no credentials, network calls, tray icon or settings writes.
    if let Some(index) = args.iter().position(|arg| arg == "--preview") {
        let result = args
            .get(index + 1)
            .ok_or_else(|| std::io::Error::other("expected BMP path"))
            .and_then(|path| {
                window::write_preview(
                    path,
                    !args.iter().any(|a| a == "--light"),
                    args.iter().any(|a| a == "--unavailable"),
                )
            });
        std::process::exit(if result.is_ok() { 0 } else { 1 });
    }
    let diagnose_enabled = args.iter().any(|arg| arg == "--diagnose");
    if diagnose_enabled {
        match diagnose::init() {
            Ok(path) => diagnose::log(format!("startup args={args:?} log_path={}", path.display())),
            Err(error) => {
                // Logging may not be available yet, but keep startup behavior unchanged.
                let _ = error;
            }
        }
    }

    if let Some(exit_code) = updater::handle_cli_mode(&args) {
        if diagnose_enabled {
            diagnose::log(format!("cli mode exited with code {exit_code}"));
        }
        std::process::exit(exit_code);
    }

    if diagnose_enabled {
        diagnose::log("entering window::run");
    }
    window::run();
}
