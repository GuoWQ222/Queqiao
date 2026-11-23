//! Embeds Queqiao's icon (assets\queqiao.ico): the installer and the uninstaller show it, and Settings > Apps takes it from the installed copy.

include!("../../assets/icon/embed.rs");

fn main() {
    embed_icon();
    println!("cargo:rerun-if-changed=build.rs");
}
