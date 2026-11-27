//! Embeds Queqiao's icon (assets\queqiao.ico) and version information (publisher, description, version): the service's program shows it in Program Files and the services list.

include!("../../assets/icon/embed.rs");

fn main() {
    embed_icon("Queqiao Service");
    println!("cargo:rerun-if-changed=build.rs");
}
