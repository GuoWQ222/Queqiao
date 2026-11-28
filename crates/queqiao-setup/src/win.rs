//! The Windows program (docs/LINUX.md: the Linux one is written beside it).
//!
//! queqiao-setup: installs Queqiao for the person using this PC (no administrator needed), and removes it again.
//!
//! - `queqiao-setup` (as downloaded): a wizard (welcome, where to install, installing, done) as UU Remote's installer
//!   (queqiao_display::SetupWizard), then Install. The Queqiao service always comes
//!   with it (the user, 2026-11-27: "Mandatory install service", as UU Remote's installer does): it is what shows the
//!   sign-in, lock and UAC screens to a viewer, so Windows asks for an administrator once; refused, nothing is
//!   installed (or the earlier version stays).
//! - `--quiet [--report <file>]`: the same without a window. (`--service` is still taken and changes nothing.) Test
//!   builds (`test-switches`) also take `--no-service [--autostart]`: the person's own installation alone, as before
//!   the service was mandatory, for the update tests.
//! - `--update [--report <file>]`: what an installed agent runs for an update it downloaded and checked: quiet, the
//!   person's choices kept, the earlier version put back if the new agent does not keep running.
//! - `--service-update [--report <file>]`: run as SYSTEM by the Queqiao service for an update it checked: its
//!   programs in %ProgramFiles%\Queqiao replaced, put back if its new agent does not keep running.
//! - `--uninstall [--quiet] [--report <file>]`: removes everything it put on this PC (Settings > Apps runs this).
//! - `--install-service`: run as an administrator (UAC) by the install: the service and its agent from this
//!   installer's own files, through a folder in Program Files (install::install_service).
//! - `--remove-service`: run as an administrator by `--uninstall` when the service is installed.
//! - `--install-display-driver [<folder>]`: run as an administrator by the install when the service is current but the
//!   virtual display driver (Super Screen) is not: the driver from this installer's own files (display_driver); with a
//!   folder, the driver files in it (an administrator's tests). Exit code 1 if it is not installed.
//! - `--remove-display-driver`: run as an administrator by `--uninstall` when the driver is there without the service:
//!   the driver, its device, packages and display records, and the certificate removed.
//! - `--ui-picture <file>`: shows the window, saves a picture of each page and leaves without installing (tests).
//! - `--ui-auto [--report <file>]`: the window goes through its pages by itself (Install, then Finish) with what it
//!   suggests: an install through the window as a person's clicks would make it (tests; pictures beside the report).
//! - `pack --output <installer> <file>...`: this program with those files as its resource (they are installed; see
//!   payload); sign the result afterwards (scripts\package.ps1).
//! - `release-key --output <file> [--dpapi]`: a new release key sealed with a passphrase the person chooses in Windows'
//!   password dialog (twice); its public half (for queqiao_update::RELEASE_KEY) is printed and saved as <file>.pub.
//!   `--dpapi`: the test key instead, encrypted for this Windows user only (for queqiao_update::TEST_KEY).
//! - `release --key <file> --installer <file> --version <x.y.z> --output <folder> [--notes <text>]`: the update feed
//!   for that installer: the installer as queqiao-setup-<x.y.z>.exe, queqiao-update.json and its signature.
//! - `release-package --key <file> --package <name> --size <bytes> --sha256 <hex> --version <x.y.z> --output <folder>`:
//!   the manifest and signature for a Linux package (queqiao-update-deb.json or -rpm.json); the package goes beside.

use crate::{display_driver, install, payload, service_update};

use std::path::PathBuf;

use queqiao_core::text::{tr, trf};
use queqiao_display::show_error;

struct Options {
    quiet: bool,
    /// Test builds only (`--no-service`): the person's own installation without the service, starting with Windows
    /// if `autostart` (`--autostart`).
    no_service: bool,
    autostart: bool,
    uninstall: bool,
    remove_service: bool,
    install_service: bool,
    /// `--install-display-driver [<folder>]`: the folder, if given (else this installer's own files).
    install_display_driver: Option<Option<PathBuf>>,
    remove_display_driver: bool,
    update: bool,
    service_update: bool,
    report: Option<PathBuf>,
    ui_picture: Option<PathBuf>,
    /// `--ui-auto`: the window goes through its pages by itself (Install with what it suggests, then Finish), as a
    /// person pressing the buttons would: for tests (it installs as `--quiet` does, through the window).
    ui_auto: bool,
}

pub(crate) fn main() {
    // Run as an administrator or as SYSTEM, from wherever the person put it: DLLs only from System32.
    queqiao_core::platform::system32_dlls_only();
    queqiao_core::platform::install_crash_log("queqiao-setup");
    let _ = queqiao_core::platform::prepare_process();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let tool = match args.first().map(String::as_str) {
        Some("pack") => Some(pack as fn(&[String]) -> Result<String, String>),
        Some("release-key") => Some(release_key as fn(&[String]) -> Result<String, String>),
        Some("release") => Some(release as fn(&[String]) -> Result<String, String>),
        Some("release-package") => Some(release_package as fn(&[String]) -> Result<String, String>),
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
            "usage: queqiao-setup [--quiet] | --uninstall [--quiet] [--report <file>]",
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
        || options.install_display_driver.is_some()
        || options.remove_display_driver
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
    } else if let Some(folder) = &options.install_display_driver {
        match folder {
            Some(folder) => display_driver::read_folder(folder),
            None => payload::own(),
        }
        .and_then(|entries| display_driver::install_reported(&entries, &mut lines))
    } else if options.remove_display_driver {
        display_driver::remove(&payload::own().unwrap_or_default(), &mut lines)
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
            if !options.quiet
                && !options.remove_service
                && !options.install_service
                && options.install_display_driver.is_none()
                && !options.remove_display_driver
                && !failure_shown()
            {
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
        no_service: false,
        autostart: false,
        uninstall: false,
        remove_service: false,
        install_service: false,
        install_display_driver: None,
        remove_display_driver: false,
        update: false,
        service_update: false,
        report: None,
        ui_picture: None,
        ui_auto: false,
    };
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "--quiet" => options.quiet = true,
            // Always: the service comes with every installation (taken for the scripts that still pass it).
            "--service" => {}
            "--no-service" if cfg!(feature = "test-switches") => options.no_service = true,
            // With the service it starts Queqiao itself: only for `--no-service` (taken for the scripts' sake).
            "--autostart" => options.autostart = true,
            "--uninstall" => options.uninstall = true,
            "--remove-service" => options.remove_service = true,
            "--install-service" => options.install_service = true,
            "--install-display-driver" => {
                // A folder may follow (not an option).
                let folder = rest
                    .clone()
                    .next()
                    .filter(|next| !next.starts_with("--"))
                    .map(PathBuf::from);
                if folder.is_some() {
                    rest.next();
                }
                options.install_display_driver = Some(folder);
            }
            "--remove-display-driver" => options.remove_display_driver = true,
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
            "--ui-auto" => options.ui_auto = true,
            _ => return None,
        }
    }
    Some(options)
}

fn run_install(options: &Options, lines: &mut Vec<String>) -> Result<(), String> {
    // Pictures of the window need nothing to install (a build before packing carries no programs).
    let entries = match payload::own() {
        Err(_) if options.ui_picture.is_some() => Vec::new(),
        entries => entries?,
    };
    let mut ignore = |_: u32, _: String| {};
    if options.update {
        if install::installed().is_none() {
            return Err("Queqiao is not installed for this person: nothing to update".into());
        }
        let choices = install::current_choices();
        lines.push(format!(
            "update to {} in {} (start with Windows: {})",
            queqiao_core::VERSION,
            choices.folder.display(),
            choices.autostart
        ));
        return install::install(&entries, &choices, lines, &mut ignore);
    }
    if options.quiet {
        // The service, which starts Queqiao at each sign-in by itself (no Run entry), unless a test build said not to;
        // where Queqiao is already, or the suggested folder; the Start menu's shortcut (an update keeps a desktop one).
        let current = install::current_choices();
        let choices = install::Choices {
            folder: install::target(),
            autostart: options.no_service && options.autostart,
            service: !options.no_service,
            desktop: install::installed().is_some() && current.desktop,
            start_menu: true,
        };
        return install::install(&entries, &choices, lines, &mut ignore);
    }
    run_wizard(options, entries, lines)
}

/// What the install's thread tells the window.
enum Step {
    Progress(u32, String),
    Done(Result<(), String>, Vec<String>),
}

/// Set once the window has shown how the install failed (main shows no second message for it).
static FAILURE_SHOWN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub(crate) fn failure_shown() -> bool {
    FAILURE_SHOWN.load(std::sync::atomic::Ordering::Relaxed)
}

/// The wizard (queqiao_display::SetupWizard): welcome, where to install (the folder checked with
/// install::check_folder, the room it needs and has), the install on a thread of its own with each step shown, done.
fn run_wizard(
    options: &Options,
    entries: Vec<payload::Entry>,
    lines: &mut Vec<String>,
) -> Result<(), String> {
    use queqiao_display::{SetupWizard, WizardAction, WizardPage, WizardSetup};
    let required = install::required_bytes(&entries);
    let installed = install::installed().is_some();
    let current = install::current_choices();
    let folder = install::target().display().to_string();
    // A new installation gets a desktop shortcut; an update keeps what there is. The Start menu's entry (in All apps)
    // is always made: the choice went (2026-11-27, the user: "remove item 'Add to start menu'"; an installer cannot pin
    // a program to Start, Windows keeps that to the person).
    let desktop = !installed || current.desktop;
    let wizard = SetupWizard::new(&WizardSetup {
        version: queqiao_core::VERSION,
        folder: &folder,
        installed,
        desktop,
    })
    .map_err(|e| e.message())?;
    let space = |folder: &str| {
        let free = install::free_bytes(std::path::Path::new(folder))
            .map_or_else(|| tr("unknown").to_string(), install::size_text);
        trf(
            "Space needed: {needed}  ·  available: {free}",
            &[("needed", &install::size_text(required)), ("free", &free)],
        )
    };
    wizard.set_space(&space(&folder));
    if let Some(path) = &options.ui_picture {
        return wizard_pictures(&wizard, path, lines);
    }
    let mut wizard = wizard;
    let mut entries = Some(entries);
    let mut running: Option<std::sync::mpsc::Receiver<Step>> = None;
    let mut outcome: Option<Result<(), String>> = None;
    // --ui-auto: its pictures beside the report.
    let auto_picture = |wizard: &SetupWizard, name: &str, lines: &mut Vec<String>| {
        if let Some(report) = &options.report {
            let file = report.with_file_name(format!("setup-auto-{name}.bmp"));
            wizard.pump(300);
            if wizard.save_picture(&file).is_ok() {
                lines.push(format!("window picture: {}", file.display()));
            }
        }
    };
    let mut auto_pressed = false;
    loop {
        let action = if options.ui_auto && !auto_pressed {
            auto_pressed = true;
            wizard.pump(300);
            wizard.show_page(WizardPage::Location);
            wizard.pump(300);
            Some(WizardAction::Install {
                folder: wizard.folder(),
                desktop,
            })
        } else if options.ui_auto && outcome.is_some() {
            auto_picture(&wizard, "finished", lines);
            Some(WizardAction::Finish { open: false })
        } else {
            wizard.next(30)
        };
        match action {
            Some(WizardAction::Install { folder, desktop })
                if running.is_none() && outcome.is_none() =>
            {
                match install::check_folder(&folder, required) {
                    Err(error) => wizard.set_error(&error),
                    Ok(folder) => {
                        wizard.set_error("");
                        lines.push(format!(
                            "installing from the window into {}",
                            folder.display()
                        ));
                        let choices = install::Choices {
                            folder,
                            autostart: false,
                            service: true,
                            desktop,
                            start_menu: true,
                        };
                        let entries = entries.take().unwrap_or_default();
                        let (sender, receiver) = std::sync::mpsc::channel();
                        std::thread::Builder::new()
                            .name("queqiao-install".into())
                            .spawn(move || {
                                let mut lines = Vec::new();
                                let progress = sender.clone();
                                let result = install::install(
                                    &entries,
                                    &choices,
                                    &mut lines,
                                    &mut |percent, step| {
                                        let _ = progress.send(Step::Progress(percent, step));
                                    },
                                );
                                let _ = sender.send(Step::Done(result, lines));
                            })
                            .map_err(|e| e.to_string())?;
                        running = Some(receiver);
                        wizard.start_installing();
                        if options.ui_auto {
                            auto_picture(&wizard, "installing", lines);
                        }
                    }
                }
            }
            Some(WizardAction::FolderChanged) if wizard.page() == WizardPage::Location => {
                wizard.set_error("");
                wizard.set_space(&space(&wizard.folder()));
            }
            Some(WizardAction::Finish { open }) => {
                if open {
                    open_queqiao(lines);
                }
                return match outcome {
                    Some(Err(error)) => {
                        FAILURE_SHOWN.store(true, std::sync::atomic::Ordering::Relaxed);
                        Err(error)
                    }
                    _ => Ok(()),
                };
            }
            Some(WizardAction::Cancel) => {
                lines.push("not installed (cancelled)".into());
                return Ok(());
            }
            _ => {}
        }
        if let Some(receiver) = &running {
            while let Ok(step) = receiver.try_recv() {
                match step {
                    Step::Progress(percent, text) => wizard.progress(percent, &text),
                    Step::Done(result, install_lines) => {
                        lines.extend(install_lines);
                        if result.is_ok() {
                            lines.push("installed from the window".into());
                        }
                        wizard.finished(result.clone().map_err(|error| shown_error(&error)));
                        outcome = Some(result);
                    }
                }
            }
            if outcome.is_some() {
                running = None;
            }
        }
    }
}

/// How a failed install reads in the window: Windows' permission for the service not given (its prompt declined,
/// the commonest) in the person's words with what to do; the installer's own text below as the details.
fn shown_error(error: &str) -> String {
    let details = trf("Details: {error}", &[("error", error)]);
    if error.starts_with("the service was not installed") {
        format!(
            "{}\n\n{details}",
            tr(
                "Windows did not allow the Queqiao service (the administrator's permission was not given), so nothing was installed. Run the installer again and choose Yes when Windows asks."
            )
        )
    } else {
        details
    }
}

/// `--ui-picture <file>`: each page drawn as it would be (nothing installed): the welcome page in `file`, the others
/// beside it as <name>-location, -installing, -finished and -failed.
fn wizard_pictures(
    wizard: &queqiao_display::SetupWizard,
    path: &std::path::Path,
    lines: &mut Vec<String>,
) -> Result<(), String> {
    use queqiao_display::WizardPage;
    let settle = |ms: u64| {
        let until = std::time::Instant::now() + std::time::Duration::from_millis(ms);
        while std::time::Instant::now() < until {
            wizard.pump(20);
        }
    };
    let mut save = |suffix: &str| -> Result<(), String> {
        let file = if suffix.is_empty() {
            path.to_path_buf()
        } else {
            let stem = path.file_stem().map_or_else(
                || "setup".into(),
                |stem| stem.to_string_lossy().into_owned(),
            );
            path.with_file_name(format!("{stem}-{suffix}.bmp"))
        };
        settle(500);
        wizard
            .save_picture(&file)
            .map_err(|e| format!("{}: {e}", file.display()))?;
        lines.push(format!("window picture: {}", file.display()));
        Ok(())
    };
    save("")?;
    wizard.show_page(WizardPage::Location);
    save("location")?;
    wizard.start_installing();
    for (percent, step) in [
        (2, tr("Closing Queqiao...").to_string()),
        (
            12,
            trf("Copying {file}...", &[("file", "queqiao-viewer.exe")]),
        ),
        (
            30,
            trf("Copying {file}...", &[("file", "queqiao-agent.exe")]),
        ),
        (
            46,
            trf("Copying {file}...", &[("file", "queqiao-service.exe")]),
        ),
        (64, tr("Installing the Queqiao service...").to_string()),
    ] {
        wizard.progress(percent, &step);
    }
    save("installing")?;
    wizard.finished(Ok(()));
    save("finished")?;
    wizard.finished(Err(shown_error(
        "the service was not installed: The operation was canceled by the user: nothing was installed",
    )));
    save("failed")?;
    Ok(())
}

/// Queqiao's main window: the agent's own (`--show`), from the person's installation (the running agent, the
/// service's, shows itself when asked).
fn open_queqiao(lines: &mut Vec<String>) {
    use std::os::windows::process::CommandExt;
    let agent = install::target().join(install::PROGRAMS[1]);
    let started = std::process::Command::new(&agent)
        .arg("--show")
        .current_dir(agent.parent().unwrap_or(&agent))
        .creation_flags(0x0000_0008) // DETACHED_PROCESS
        .spawn();
    lines.push(match started {
        Ok(_) => "Queqiao opened".into(),
        Err(error) => format!("Queqiao not opened: {error}"),
    });
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

/// `release-key --output <file> [--dpapi]`: the release key sealed with a passphrase (security review of 2026-11-27:
/// encrypted only for its Windows user, any program running as that user could have signed an update with it), or
/// with `--dpapi` a test key for the update tests on the build PC.
fn release_key(args: &[String]) -> Result<String, String> {
    let dpapi = args.iter().any(|arg| arg == "--dpapi");
    let rest: Vec<String> = args
        .iter()
        .filter(|arg| *arg != "--dpapi")
        .cloned()
        .collect();
    let [output] = &named(&rest, &["--output"])?[..] else {
        unreachable!()
    };
    let output = output
        .as_ref()
        .ok_or("usage: queqiao-setup release-key --output <file> [--dpapi]")?;
    if std::path::Path::new(output).exists() {
        return Err(format!("{output} exists: a release key is not replaced"));
    }
    let (key, public) = queqiao_update::release::generate_key()?;
    let (sealed, how) = if dpapi {
        (
            queqiao_update::release::protect(&key)?,
            "DPAPI, this Windows user only: a test key",
        )
    } else {
        let passphrase = new_passphrase()?;
        (
            queqiao_update::release::seal(&key, &passphrase)?,
            "sealed with your passphrase",
        )
    };
    let write = |path: &str, data: &[u8]| {
        use std::io::Write;
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .and_then(|mut file| file.write_all(data))
            .map_err(|e| format!("{path}: {e}"))
    };
    write(output, &sealed)?;
    let public_file = format!("{output}.pub");
    let _ = std::fs::remove_file(&public_file);
    write(&public_file, format!("{public}\n").as_bytes())?;
    if !dpapi {
        queqiao_display::show_info(
            tr("Queqiao release key"),
            &trf(
                "The release key is in {file}, sealed with your passphrase. Its public half is in {file}.pub.\n\nKeep the key file and the passphrase safe, best on a USB stick away from this PC: with both, anyone can publish updates to every Queqiao, and neither can be recovered.",
                &[("file", output.as_str())],
            ),
        );
    }
    Ok(format!(
        "release key in {output} ({how}); public key: {public} (also in {public_file})"
    ))
}

/// The release key in `path`: opened with its passphrase (asked in Windows' password dialog) when sealed, else the
/// test key (DPAPI).
fn read_release_key(path: &str) -> Result<Vec<u8>, String> {
    let data = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
    if !queqiao_update::release::is_sealed(&data) {
        return queqiao_update::release::unprotect(&data);
    }
    let passphrase = ask_passphrase(&trf(
        "The passphrase of the release key {file}:",
        &[("file", path)],
    ))?;
    queqiao_update::release::open(&data, &passphrase)
}

/// A new passphrase for the release key: asked twice, at least MIN_PASSPHRASE characters.
fn new_passphrase() -> Result<String, String> {
    let first = ask_passphrase(tr(
        "Choose a passphrase for the new release key (at least 12 characters). Anyone who has the key file and this passphrase can publish updates to every Queqiao. It cannot be recovered: keep it safe.",
    ))?;
    if first.chars().count() < queqiao_update::release::MIN_PASSPHRASE {
        return Err(tr("The passphrase needs at least 12 characters.").into());
    }
    let second = ask_passphrase(tr("Type the passphrase again."))?;
    if first != second {
        return Err(tr("The two passphrases differ: no release key was made.").into());
    }
    Ok(first)
}

/// A passphrase typed into Windows' own password dialog (CredUI; nothing is kept).
fn ask_passphrase(message: &str) -> Result<String, String> {
    use windows::Win32::Foundation::{ERROR_CANCELLED, ERROR_INVALID_FLAGS, NO_ERROR};
    use windows::Win32::Security::Credentials::{
        CREDUI_FLAGS_ALWAYS_SHOW_UI, CREDUI_FLAGS_DO_NOT_PERSIST, CREDUI_FLAGS_GENERIC_CREDENTIALS,
        CREDUI_FLAGS_KEEP_USERNAME, CREDUI_INFOW, CredUIPromptForCredentialsW,
    };
    use windows::core::PCWSTR;
    let caption: Vec<u16> = tr("Queqiao release key")
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let text: Vec<u16> = message.encode_utf16().chain(Some(0)).collect();
    let target: Vec<u16> = "Queqiao release key"
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let info = CREDUI_INFOW {
        cbSize: size_of::<CREDUI_INFOW>() as u32,
        pszMessageText: PCWSTR(text.as_ptr()),
        pszCaptionText: PCWSTR(caption.as_ptr()),
        ..Default::default()
    };
    let base = CREDUI_FLAGS_GENERIC_CREDENTIALS
        | CREDUI_FLAGS_ALWAYS_SHOW_UI
        | CREDUI_FLAGS_DO_NOT_PERSIST;
    for flags in [base | CREDUI_FLAGS_KEEP_USERNAME, base] {
        let mut user = [0u16; 514];
        for (at, unit) in "release key".encode_utf16().enumerate() {
            user[at] = unit;
        }
        let mut password = [0u16; 257];
        let result = unsafe {
            CredUIPromptForCredentialsW(
                Some(&info),
                PCWSTR(target.as_ptr()),
                None,
                0,
                &mut user,
                &mut password,
                None,
                flags,
            )
        };
        let length = password
            .iter()
            .position(|&unit| unit == 0)
            .unwrap_or(password.len());
        let typed = String::from_utf16(&password[..length]);
        password.fill(0);
        match result {
            NO_ERROR => return typed.map_err(|_| "the passphrase is not valid text".to_string()),
            ERROR_CANCELLED => return Err("cancelled".into()),
            ERROR_INVALID_FLAGS => continue,
            other => return Err(format!("the password dialog failed ({})", other.0)),
        }
    }
    Err("the password dialog failed".into())
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
    let key = read_release_key(key)?;
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
    let matches = if public == queqiao_update::TRUSTED_KEY {
        "the key this build trusts"
    } else {
        "NOT the key this build trusts"
    };
    Ok(format!(
        "feed for {version} in {output} (signed by {public}: {matches})"
    ))
}

/// `release-package --key <file> --package <name> --size <bytes> --sha256 <hex> --version <x.y.z> --output <folder>
/// [--notes <text>]`: the manifest and signature for a Linux package built elsewhere (only its name, size and SHA-256
/// come here); the package goes into the feed beside them.
fn release_package(args: &[String]) -> Result<String, String> {
    let usage = "usage: queqiao-setup release-package --key <file> --package <name> --size <bytes> --sha256 <hex> --version <x.y.z> --output <folder> [--notes <text>]";
    let values = named(
        args,
        &[
            "--key",
            "--package",
            "--size",
            "--sha256",
            "--version",
            "--output",
            "--notes",
        ],
    )?;
    let [
        Some(key),
        Some(package),
        Some(size),
        Some(sha256),
        Some(version),
        Some(output),
        notes,
    ] = &values[..]
    else {
        return Err(usage.into());
    };
    let size: u64 = size
        .parse()
        .map_err(|_| format!("{size}: not a size in bytes"))?;
    let key = read_release_key(key)?;
    let public = queqiao_update::release::public_key(&key)?;
    let (name, manifest, signature) = queqiao_update::release::sign_package(
        &key,
        version,
        package,
        size,
        sha256,
        notes.as_deref().unwrap_or(""),
    )?;
    let folder = PathBuf::from(output);
    std::fs::create_dir_all(&folder).map_err(|e| format!("{output}: {e}"))?;
    std::fs::write(folder.join(name), &manifest).map_err(|e| format!("{name}: {e}"))?;
    std::fs::write(folder.join(format!("{name}.sig")), signature.as_bytes())
        .map_err(|e| format!("{name}.sig: {e}"))?;
    let matches = if public == queqiao_update::TRUSTED_KEY {
        "the key this build trusts"
    } else {
        "NOT the key this build trusts"
    };
    Ok(format!(
        "{name} for {package} {version} in {output} (signed by {public}: {matches})"
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
