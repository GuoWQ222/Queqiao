//! Shared code for all Queqiao programs.
//!
//! Planned modules (docs/DESIGN.md): protocol, identity, transport, input.
//! For now: the version information every program prints at start-up, and the test switch that forces
//! fallback paths.

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

/// Test switch for fallback paths, so every path can run on any machine: the environment variable
/// `QUEQIAO_FORCE` holds `stage:choice` pairs separated by commas, for example
/// `encode:software,adapter:warp`. Returns the forced choice for `stage` (lower case), if any.
pub fn forced(stage: &str) -> Option<String> {
    forced_in(&std::env::var("QUEQIAO_FORCE").ok()?, stage)
}

fn forced_in(spec: &str, stage: &str) -> Option<String> {
    spec.split(',')
        .filter_map(|item| item.split_once(':'))
        .find(|(name, _)| name.trim().eq_ignore_ascii_case(stage))
        .map(|(_, choice)| choice.trim().to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn banner_names_program_and_versions() {
        let text = banner("queqiao-agent");
        assert!(text.starts_with("queqiao-agent "));
        assert!(text.ends_with(" (protocol 0)"));
    }

    #[test]
    fn forced_reads_stage_choices() {
        let spec = "encode:Software, adapter:warp";
        assert_eq!(forced_in(spec, "encode").as_deref(), Some("software"));
        assert_eq!(forced_in(spec, "ADAPTER").as_deref(), Some("warp"));
        assert_eq!(forced_in(spec, "capture"), None);
        assert_eq!(forced_in("", "encode"), None);
    }
}
