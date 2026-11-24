//! Linux (docs/LINUX.md step 5): opens the session's forms that no session test reaches by itself (the transfer list
//! and the password form), fills them as a session would, and saves their pictures; prints FORMS_PASS or FORMS_FAIL.
//! Usage: DISPLAY=:92 linux_forms_probe <folder for the pictures>. On Windows it does nothing.

#[cfg(target_os = "linux")]
fn main() {
    use std::time::Duration;

    let folder = std::path::PathBuf::from(std::env::args().nth(1).unwrap_or_else(|| ".".into()));
    let mut failed = Vec::new();
    match queqiao_display::PickList::new(
        "Queqiao - transfers",
        "Files on their way (sent and received):",
        &[(1, "Pause / resume"), (2, "Cancel")],
    ) {
        Ok(mut list) => {
            list.set_items(vec![
                "to that PC: report.pdf, 12.4 of 20.0 MB (62 %)".into(),
                "from that PC: photos.zip, paused at 3.1 of 48.0 MB".into(),
            ]);
            std::thread::sleep(Duration::from_millis(800));
            if let Err(error) = list.save_picture(&folder.join("transfers.bmp")) {
                failed.push(format!("transfer list picture: {error}"));
            }
            if list.next(0).is_some() {
                failed.push("the transfer list gave an action nobody took".into());
            }
        }
        Err(error) => failed.push(format!("transfer list: {error}")),
    }
    match queqiao_display::PasswordForm::new(
        "Queqiao - that PC is locked",
        "That PC is locked. Its Windows password unlocks it (it goes there encrypted and is checked there).",
    ) {
        Ok(mut form) => {
            form.set_status("Wrong password. Try again.");
            std::thread::sleep(Duration::from_millis(800));
            if let Err(error) = form.save_picture(&folder.join("password.bmp")) {
                failed.push(format!("password form picture: {error}"));
            }
            if form.next(0).is_some() {
                failed.push("the password form gave an action nobody took".into());
            }
        }
        Err(error) => failed.push(format!("password form: {error}")),
    }
    for failure in &failed {
        println!("{failure}");
    }
    println!(
        "{}",
        if failed.is_empty() {
            "FORMS_PASS"
        } else {
            "FORMS_FAIL"
        }
    );
}

#[cfg(not(target_os = "linux"))]
fn main() {}
