//! The desktop helper's kernel input on this PC (run as root): steps given on the command line, one after another.
//! Usage: uinput_probe <step>...  with steps move:<x>,<y> (0..65535 across the screen), click, type:<text> (US
//! keyboard: letters, digits, '-', ' '), enter, wait:<ms>.

#[cfg(target_os = "linux")]
fn main() {
    use queqiao_core::protocol::{InputEvent, MouseButton};
    let mut injector = match queqiao_input::UinputInjector::new() {
        Ok(injector) => injector,
        Err(error) => {
            println!("UINPUT_FAIL {error}");
            std::process::exit(1);
        }
    };
    // The compositor takes in a new device before its first events count.
    std::thread::sleep(std::time::Duration::from_millis(1500));
    let key = |injector: &mut queqiao_input::UinputInjector, scancode: u16| {
        for down in [true, false] {
            injector
                .inject(&InputEvent::Key {
                    scancode,
                    extended: false,
                    down,
                })
                .unwrap();
            std::thread::sleep(std::time::Duration::from_millis(40));
        }
    };
    let scancode = |c: char| -> Option<u16> {
        let rows = ["1234567890-", "qwertyuiop", "asdfghjkl", "zxcvbnm"];
        let starts = [0x02u16, 0x10, 0x1E, 0x2C];
        if c == ' ' {
            return Some(0x39);
        }
        rows.iter()
            .zip(starts)
            .find_map(|(row, start)| row.find(c).map(|at| start + at as u16))
    };
    for step in std::env::args().skip(1) {
        let (name, value) = step.split_once(':').unwrap_or((&step, ""));
        match name {
            "move" => {
                let (x, y) = value.split_once(',').expect("move:x,y");
                injector
                    .inject(&InputEvent::MouseMove {
                        x: x.parse().unwrap(),
                        y: y.parse().unwrap(),
                    })
                    .unwrap();
            }
            "click" => {
                for down in [true, false] {
                    injector
                        .inject(&InputEvent::MouseButton {
                            button: MouseButton::Left,
                            down,
                        })
                        .unwrap();
                    std::thread::sleep(std::time::Duration::from_millis(60));
                }
            }
            "type" => value
                .chars()
                .for_each(|c| key(&mut injector, scancode(c).expect("a typable character"))),
            "enter" => key(&mut injector, 0x1C),
            "wait" => std::thread::sleep(std::time::Duration::from_millis(value.parse().unwrap())),
            other => panic!("unknown step {other}"),
        }
        println!("done: {step}");
        std::thread::sleep(std::time::Duration::from_millis(150));
    }
    std::thread::sleep(std::time::Duration::from_millis(500));
    println!("UINPUT_PASS");
}

#[cfg(not(target_os = "linux"))]
fn main() {
    println!("uinput_probe: Linux only");
}
