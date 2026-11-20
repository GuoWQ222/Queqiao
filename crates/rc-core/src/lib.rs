//! Shared code for all RemoteControl programs.
//!
//! Planned modules (docs/DESIGN.md): protocol, identity, transport, capture, codec, input.
//! M0 holds only the version information that every program prints at start-up.

/// Version of the messages exchanged between agent, viewer and server.
/// Peers with a different version refuse to connect.
pub const PROTOCOL_VERSION: u32 = 0;

/// One-line start-up banner: program name, workspace version, protocol version.
pub fn banner(program: &str) -> String {
    format!(
        "{program} {} (protocol {PROTOCOL_VERSION})",
        env!("CARGO_PKG_VERSION")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn banner_names_program_and_versions() {
        let text = banner("rc-agent");
        assert!(text.starts_with("rc-agent "));
        assert!(text.ends_with(" (protocol 0)"));
    }
}
