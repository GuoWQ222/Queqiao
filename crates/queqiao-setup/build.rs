//! Embeds Queqiao's icon (assets\queqiao.ico) and version information (publisher, description, version): the installer and the uninstaller show it, and Settings > Apps takes it from the installed copy.

include!("../../assets/icon/embed.rs");

fn main() {
    embed_icon("Queqiao Setup");
    println!("cargo:rerun-if-changed=build.rs");
}
