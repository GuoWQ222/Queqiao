//! Tests the screens' resolution change (display_mode): screen `index` (default 0) listed, switched to the size given
//! (default: the second largest it offers), checked, held a moment, put back, checked again. Prints
//! DISPLAY_MODE_PASS or DISPLAY_MODE_FAIL <why>.
//! Usage: display_mode_probe [index] [WIDTHxHEIGHT] [seconds held]

use queqiao_capture::display_mode::{display_modes, restore_display_mode, set_display_mode};

fn main() {
    let mut args = std::env::args().skip(1);
    let index: usize = args.next().and_then(|v| v.parse().ok()).unwrap_or(0);
    let wanted: Option<(u32, u32)> = args.next().and_then(|v| {
        let (w, h) = v.split_once('x')?;
        Some((w.parse().ok()?, h.parse().ok()?))
    });
    let held: u64 = args.next().and_then(|v| v.parse().ok()).unwrap_or(2);
    let fail = |why: String| -> ! {
        println!("DISPLAY_MODE_FAIL {why}");
        std::process::exit(0);
    };
    let before = display_modes(index).unwrap_or_else(|e| fail(format!("modes: {e}")));
    println!(
        "screen {index}: {}x{}, sizes {:?}",
        before.current.0, before.current.1, before.sizes
    );
    let Some(target) = wanted.or_else(|| {
        before
            .sizes
            .iter()
            .copied()
            .find(|size| *size != before.current)
    }) else {
        fail("no other size offered".into())
    };
    if let Err(error) = set_display_mode(index, target.0, target.1) {
        fail(format!("set {}x{}: {error}", target.0, target.1));
    }
    std::thread::sleep(std::time::Duration::from_millis(500));
    let during =
        display_modes(index).unwrap_or_else(|e| fail(format!("modes after the change: {e}")));
    println!(
        "after the change: {}x{}",
        during.current.0, during.current.1
    );
    std::thread::sleep(std::time::Duration::from_secs(held));
    if let Err(error) = restore_display_mode(index, before.current) {
        fail(format!("restore: {error}"));
    }
    std::thread::sleep(std::time::Duration::from_millis(500));
    let after =
        display_modes(index).unwrap_or_else(|e| fail(format!("modes after putting back: {e}")));
    println!(
        "after putting back: {}x{}",
        after.current.0, after.current.1
    );
    if during.current != target {
        fail(format!(
            "the screen showed {:?}, not {target:?}",
            during.current
        ));
    }
    if after.current != before.current {
        fail(format!(
            "the screen's own size {:?} did not come back ({:?})",
            before.current, after.current
        ));
    }
    println!("DISPLAY_MODE_PASS");
}
