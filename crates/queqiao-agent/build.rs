//! Embeds Queqiao's icon (assets\queqiao.ico) and a manifest asking for Common Controls 6, so the tray menu, the session banner and the consent
//! prompt have the current Windows look (without it, Windows draws the controls in the Windows 2000 style).

include!("../../assets/icon/embed.rs");

fn main() {
    embed_icon();
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        println!("cargo:rustc-link-arg-bins=/MANIFEST:EMBED");
        println!(
            "cargo:rustc-link-arg-bins=/MANIFESTDEPENDENCY:type='win32' name='Microsoft.Windows.Common-Controls' version='6.0.0.0' processorArchitecture='*' publicKeyToken='6595b64144ccf1df' language='*'"
        );
    }
    println!("cargo:rerun-if-changed=build.rs");
}
