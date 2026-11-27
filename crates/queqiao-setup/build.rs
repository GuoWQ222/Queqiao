//! Embeds Queqiao's icon (assets\queqiao.ico) and version information (publisher, description, version): the installer and the uninstaller show it, and Settings > Apps takes it from the installed copy.

include!("../../assets/icon/embed.rs");

fn main() {
    embed_icon("Queqiao Setup");
    // Common Controls 6: the wizard's folder box and Windows' folder dialog in the current Windows look.
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        println!("cargo:rustc-link-arg-bins=/MANIFEST:EMBED");
        println!(
            "cargo:rustc-link-arg-bins=/MANIFESTDEPENDENCY:type='win32' name='Microsoft.Windows.Common-Controls' version='6.0.0.0' processorArchitecture='*' publicKeyToken='6595b64144ccf1df' language='*'"
        );
    }
    println!("cargo:rerun-if-changed=build.rs");
}
