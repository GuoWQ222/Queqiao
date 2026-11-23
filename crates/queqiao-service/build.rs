//! Embeds Queqiao's icon (assets\queqiao.ico): the service's program shows it in Program Files and the services list.

include!("../../assets/icon/embed.rs");

fn main() {
    embed_icon();
    println!("cargo:rerun-if-changed=build.rs");
}
