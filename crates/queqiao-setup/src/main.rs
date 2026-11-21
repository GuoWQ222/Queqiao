//! queqiao-setup: installs Queqiao for the person using this PC (no administrator needed), and removes it again.
//!
//! - `queqiao-setup` (as downloaded): a window with the choices, then Install.
//! - `--quiet [--autostart] [--service] [--report <file>]`: the same without a window (tests, updates).
//! - `--uninstall [--quiet] [--report <file>]`: removes everything it put on this PC (Settings > Apps runs this).
//! - `--remove-service`: run as an administrator by `--uninstall` when the service is installed.
//! - `--ui-picture <file>`: shows the window, saves a picture of it and leaves without installing (tests).
//! - `pack --output <installer> <file>...`: this program with those files appended (they are installed); sign the
//!   result afterwards (scripts\package.ps1).

#![windows_subsystem = "windows"]

mod install;
mod payload;

use std::path::PathBuf;

use queqiao_core::text::{tr, trf};
use queqiao_display::{SetupAction, SetupForm, show_error};

struct Options {
    quiet: bool,
    autostart: bool,
    service: bool,
    uninstall: bool,
    remove_service: bool,
    report: Option<PathBuf>,
    ui_picture: Option<PathBuf>,
}

fn main() {
    queqiao_core::platform::install_crash_log("queqiao-setup");
    let _ = queqiao_core::platform::prepare_process();
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("pack") {
        let code = match pack(&args[1..]) {
            Ok(line) => {
                println!("{line}");
                0
            }
            Err(error) => {
                eprintln!("{error}");
                1
            }
        };
        std::process::exit(code);
    }
    if args.iter().any(|arg| arg == "--version") {
        println!("{}", queqiao_core::banner(env!("CARGO_PKG_NAME")));
        return;
    }
    let Some(options) = parse(&args) else {
        show_error(
            "Queqiao",
            "usage: queqiao-setup [--quiet [--autostart] [--service]] | --uninstall [--quiet] [--report <file>]",
        );
        std::process::exit(2);
    };
    let mut lines = vec![queqiao_core::banner(env!("CARGO_PKG_NAME"))];
    let result = if options.remove_service {
        install::remove_service(&mut lines)
    } else if options.uninstall {
        uninstall(&options, &mut lines)
    } else {
        run_install(&options, &mut lines)
    };
    let passed = match result {
        Ok(()) => true,
        Err(error) => {
            lines.push(format!("ERROR {error}"));
            if !options.quiet && !options.remove_service {
                show_error(tr("Queqiao"), &error);
            }
            false
        }
    };
    lines.push(format!("RESULT {}", if passed { "PASS" } else { "FAIL" }));
    if let Some(path) = &options.report {
        let _ = std::fs::write(path, lines.join("\r\n") + "\r\n");
    }
    std::process::exit(if passed { 0 } else { 1 });
}

fn parse(args: &[String]) -> Option<Options> {
    let mut options = Options {
        quiet: false,
        autostart: false,
        service: false,
        uninstall: false,
        remove_service: false,
        report: None,
        ui_picture: None,
    };
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "--quiet" => options.quiet = true,
            "--autostart" => options.autostart = true,
            "--service" => options.service = true,
            "--uninstall" => options.uninstall = true,
            "--remove-service" => options.remove_service = true,
            "--report" => options.report = Some(PathBuf::from(rest.next()?)),
            "--ui-picture" => options.ui_picture = Some(PathBuf::from(rest.next()?)),
            _ => return None,
        }
    }
    Some(options)
}

fn run_install(options: &Options, lines: &mut Vec<String>) -> Result<(), String> {
    let me = std::env::current_exe().map_err(|e| e.to_string())?;
    let data = std::fs::read(&me).map_err(|e| format!("{}: {e}", me.display()))?;
    let entries = payload::read(&data)?;
    if options.quiet {
        let choices = install::Choices {
            autostart: options.autostart,
            service: options.service,
        };
        return install::install(&entries, &choices, lines);
    }
    let text = trf(
        "Queqiao will be installed for you on this PC, in {folder}. No administrator is needed for that.",
        &[("folder", &install::target().display().to_string())],
    );
    let mut form =
        SetupForm::new(&text, true, install::service_installed()).map_err(|e| e.message())?;
    if let Some(path) = &options.ui_picture {
        let drawn = std::time::Instant::now() + std::time::Duration::from_millis(600);
        while std::time::Instant::now() < drawn {
            let _ = form.next(20);
        }
        queqiao_display::save_window_picture(form.hwnd(), path)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        lines.push(format!("window picture: {}", path.display()));
        return Ok(());
    }
    loop {
        match form.next(50) {
            Some(SetupAction::Install { autostart, service }) => {
                form.set_busy(true);
                form.set_status(tr("Installing..."));
                let choices = install::Choices { autostart, service };
                let result = install::install(&entries, &choices, lines);
                form.set_busy(false);
                match result {
                    Ok(()) => {
                        form.set_status(tr("Queqiao is installed. Find it in the Start menu."));
                        lines.push("installed from the window".into());
                    }
                    Err(error) => form.set_status(&error),
                }
            }
            Some(SetupAction::Cancel) => return Ok(()),
            None => {}
        }
    }
}

fn uninstall(options: &Options, lines: &mut Vec<String>) -> Result<(), String> {
    if !options.quiet
        && !queqiao_display::confirm(
            windows::Win32::Foundation::HWND::default(),
            tr("Queqiao"),
            tr("Remove Queqiao from this PC?"),
        )
    {
        lines.push("not removed (cancelled)".into());
        return Ok(());
    }
    install::uninstall(lines)?;
    if !options.quiet {
        queqiao_display::show_info(tr("Queqiao"), tr("Queqiao has been removed from this PC."));
    }
    Ok(())
}

/// `pack --output <installer> <file>...`
fn pack(args: &[String]) -> Result<String, String> {
    let (output, files) = match args {
        [flag, output, files @ ..] if flag == "--output" && !files.is_empty() => (output, files),
        _ => return Err("usage: queqiao-setup pack --output <installer> <file>...".into()),
    };
    let me = std::env::current_exe().map_err(|e| e.to_string())?;
    let program = std::fs::read(&me).map_err(|e| e.to_string())?;
    let mut entries = Vec::new();
    for file in files {
        let path = PathBuf::from(file);
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| format!("no file name: {file}"))?
            .to_string();
        let bytes = std::fs::read(&path).map_err(|e| format!("{file}: {e}"))?;
        entries.push(payload::Entry { name, bytes });
    }
    let packed = payload::pack(&program, &entries)?;
    std::fs::write(output, &packed).map_err(|e| format!("{output}: {e}"))?;
    Ok(format!(
        "{output}: {} files, {:.1} MB",
        entries.len(),
        packed.len() as f64 / 1e6
    ))
}
