//! queqiao-setup: installs Queqiao for the person using this PC (no administrator needed), and removes it again.
//!
//! - `queqiao-setup` (as downloaded): a window with the choices, then Install.
//! - `--quiet [--autostart] [--service] [--report <file>]`: the same without a window (tests, updates).
//! - `--update [--report <file>]`: what an installed agent runs for an update it downloaded and checked: quiet, the
//!   person's choices kept, the earlier version put back if the new agent does not keep running.
//! - `--service-update [--report <file>]`: run as SYSTEM by the Queqiao service for an update it checked: its
//!   programs in %ProgramFiles%\Queqiao replaced, put back if its new agent does not keep running.
//! - `--uninstall [--quiet] [--report <file>]`: removes everything it put on this PC (Settings > Apps runs this).
//! - `--install-service`: run as an administrator (UAC) by the install when the service is chosen: the service and
//!   its agent from this installer's own files, through a folder in Program Files (install::install_service).
//! - `--remove-service`: run as an administrator by `--uninstall` when the service is installed.
//! - `--ui-picture <file>`: shows the window, saves a picture of it and leaves without installing (tests).
//! - `pack --output <installer> <file>...`: this program with those files as its resource (they are installed; see
//!   payload); sign the result afterwards (scripts\package.ps1).
//! - `release-key --output <file>`: a new release key, DPAPI-encrypted for the person running this; prints its
//!   public half (for queqiao_update::RELEASE_KEY).
//! - `release --key <file> --installer <file> --version <x.y.z> --output <folder> [--notes <text>]`: the update feed
//!   for that installer: the installer as queqiao-setup-<x.y.z>.exe, queqiao-update.json and its signature.

#![windows_subsystem = "windows"]

mod install;
mod payload;
mod service_update;

use std::path::PathBuf;

use queqiao_core::text::{tr, trf};
use queqiao_display::{SetupAction, SetupForm, show_error};

struct Options {
    quiet: bool,
    autostart: bool,
    service: bool,
    uninstall: bool,
    remove_service: bool,
    install_service: bool,
    update: bool,
    service_update: bool,
    report: Option<PathBuf>,
    ui_picture: Option<PathBuf>,
}

fn main() {
    // Run as an administrator or as SYSTEM, from wherever the person put it: DLLs only from System32.
    queqiao_core::platform::system32_dlls_only();
    queqiao_core::platform::install_crash_log("queqiao-setup");
    let _ = queqiao_core::platform::prepare_process();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let tool = match args.first().map(String::as_str) {
        Some("pack") => Some(pack as fn(&[String]) -> Result<String, String>),
        Some("release-key") => Some(release_key as fn(&[String]) -> Result<String, String>),
        Some("release") => Some(release as fn(&[String]) -> Result<String, String>),
        _ => None,
    };
    if let Some(tool) = tool {
        let code = match tool(&args[1..]) {
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
    // One installer at a time in this session: an update started while an install still finishes waits for it. Not
    // for --remove-service: --uninstall, which holds it, runs that and waits for it. Not for --service-update either:
    // run by the service in session 0, where "Local\" names are the machine's own, any user's program could take the
    // name first and stop every update of the service (the security review of 2026-11-22). The service makes its
    // update folder new for each update, which fails while an earlier installer still runs from it: one at a time.
    let _turn = if options.remove_service
        || options.install_service
        || options.service_update
        || options.ui_picture.is_some()
    {
        None
    } else {
        match one_at_a_time() {
            Ok(turn) => Some(turn),
            Err(error) => {
                lines.push(format!("ERROR {error}"));
                lines.push("RESULT FAIL".into());
                if let Some(path) = &options.report {
                    let _ = std::fs::write(path, lines.join("\r\n") + "\r\n");
                }
                std::process::exit(1);
            }
        }
    };
    let result = if options.service_update {
        payload::own().and_then(|entries| service_update::service_update(&entries, &mut lines))
    } else if options.install_service {
        payload::own().and_then(|entries| install::install_service(&entries, &mut lines))
    } else if options.remove_service {
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
            if !options.quiet && !options.remove_service && !options.install_service {
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

/// Waits for any other installer of this session to finish (at most 3 minutes); held until this one ends.
fn one_at_a_time() -> Result<windows::Win32::Foundation::HANDLE, String> {
    use windows::Win32::Foundation::{WAIT_ABANDONED, WAIT_OBJECT_0};
    use windows::Win32::System::Threading::{CreateMutexW, WaitForSingleObject};
    let mutex = unsafe { CreateMutexW(None, false, windows::core::w!("Local\\QueqiaoSetup")) }
        .map_err(|e| format!("installer lock: {}", e.message()))?;
    let waited = unsafe { WaitForSingleObject(mutex, 180_000) };
    if waited == WAIT_OBJECT_0 || waited == WAIT_ABANDONED {
        Ok(mutex)
    } else {
        Err("another Queqiao installer is still running".into())
    }
}

fn parse(args: &[String]) -> Option<Options> {
    let mut options = Options {
        quiet: false,
        autostart: false,
        service: false,
        uninstall: false,
        remove_service: false,
        install_service: false,
        update: false,
        service_update: false,
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
            "--install-service" => options.install_service = true,
            "--update" => {
                options.update = true;
                options.quiet = true;
            }
            "--service-update" => {
                options.service_update = true;
                options.quiet = true;
            }
            "--report" => options.report = Some(PathBuf::from(rest.next()?)),
            "--ui-picture" => options.ui_picture = Some(PathBuf::from(rest.next()?)),
            _ => return None,
        }
    }
    Some(options)
}

fn run_install(options: &Options, lines: &mut Vec<String>) -> Result<(), String> {
    let entries = payload::own()?;
    if options.update {
        if !install::target().exists() {
            return Err("Queqiao is not installed for this person: nothing to update".into());
        }
        let choices = install::current_choices();
        lines.push(format!(
            "update to {} (start with Windows: {})",
            queqiao_core::VERSION,
            choices.autostart
        ));
        return install::install(&entries, &choices, lines);
    }
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

/// `--name value` pairs of a tool's command line.
fn named(args: &[String], names: &[&str]) -> Result<Vec<Option<String>>, String> {
    let mut values = vec![None; names.len()];
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        let at = names
            .iter()
            .position(|name| arg == name)
            .ok_or_else(|| format!("unknown option {arg}"))?;
        values[at] = Some(
            rest.next()
                .ok_or_else(|| format!("{arg} needs a value"))?
                .clone(),
        );
    }
    Ok(values)
}

/// `release-key --output <file>`
fn release_key(args: &[String]) -> Result<String, String> {
    let [output] = &named(args, &["--output"])?[..] else {
        unreachable!()
    };
    let output = output
        .as_ref()
        .ok_or("usage: queqiao-setup release-key --output <file>")?;
    if std::path::Path::new(output).exists() {
        return Err(format!("{output} exists: a release key is not replaced"));
    }
    let (key, public) = queqiao_update::release::generate_key()?;
    let sealed = queqiao_update::release::protect(&key)?;
    std::fs::write(output, sealed).map_err(|e| format!("{output}: {e}"))?;
    Ok(format!(
        "release key in {output} (DPAPI, this Windows user only); public key: {public}"
    ))
}

/// `release --key <file> --installer <file> --version <x.y.z> --output <folder> [--notes <text>]`
fn release(args: &[String]) -> Result<String, String> {
    let usage = "usage: queqiao-setup release --key <file> --installer <file> --version <x.y.z> --output <folder> [--notes <text>]";
    let values = named(
        args,
        &["--key", "--installer", "--version", "--output", "--notes"],
    )?;
    let [
        Some(key),
        Some(installer),
        Some(version),
        Some(output),
        notes,
    ] = &values[..]
    else {
        return Err(usage.into());
    };
    let sealed = std::fs::read(key).map_err(|e| format!("{key}: {e}"))?;
    let key = queqiao_update::release::unprotect(&sealed)?;
    let public = queqiao_update::release::public_key(&key)?;
    let bytes = std::fs::read(installer).map_err(|e| format!("{installer}: {e}"))?;
    let (manifest, signature) =
        queqiao_update::release::sign(&key, version, &bytes, notes.as_deref().unwrap_or(""))?;
    let folder = PathBuf::from(output);
    std::fs::create_dir_all(&folder).map_err(|e| format!("{output}: {e}"))?;
    let name = format!("queqiao-setup-{version}.exe");
    for (file, data) in [
        (name.as_str(), bytes.as_slice()),
        (queqiao_update::MANIFEST, manifest.as_slice()),
        (queqiao_update::SIGNATURE, signature.as_bytes()),
    ] {
        std::fs::write(folder.join(file), data).map_err(|e| format!("{file}: {e}"))?;
    }
    let matches = if public == queqiao_update::RELEASE_KEY {
        "this build's release key"
    } else {
        "NOT this build's release key"
    };
    Ok(format!(
        "feed for {version} in {output} (signed by {public}: {matches})"
    ))
}

/// `pack --output <installer> <file>...`
fn pack(args: &[String]) -> Result<String, String> {
    let (output, files) = match args {
        [flag, output, files @ ..] if flag == "--output" && !files.is_empty() => (output, files),
        _ => return Err("usage: queqiao-setup pack --output <installer> <file>...".into()),
    };
    let me = std::env::current_exe().map_err(|e| e.to_string())?;
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
    let size = payload::pack(&me, std::path::Path::new(output), &entries)?;
    Ok(format!(
        "{output}: {} files, {:.1} MB",
        entries.len(),
        size as f64 / 1e6
    ))
}
