//! What the system's resolver and `resolve_server` give for each "host:port" named: on a PC whose proxy answers with
//! made-up addresses (Clash/Mihomo's fake IP, 198.18.x.x), the second must still give addresses on the internet.
//! Usage: resolve_probe <host:port>... Prints one line per name and RESOLVE_PROBE_DONE.

use std::net::ToSocketAddrs;

fn main() {
    for server in std::env::args().skip(1) {
        let system: Vec<String> = server
            .to_socket_addrs()
            .map(|found| found.map(|a| a.ip().to_string()).collect())
            .unwrap_or_default();
        let started = std::time::Instant::now();
        let resolved = match queqiao_transport::resolve_server(&server) {
            Ok(found) => found
                .iter()
                .map(|a| {
                    let public = queqiao_transport::public_address(a.ip());
                    format!("{}{}", a.ip(), if public { "" } else { " (not public)" })
                })
                .collect::<Vec<_>>()
                .join(", "),
            Err(error) => format!("error: {error}"),
        };
        println!(
            "{server}: system [{}]; resolve_server [{resolved}] in {} ms",
            system.join(", "),
            started.elapsed().as_millis()
        );
    }
    println!("RESOLVE_PROBE_DONE");
}
