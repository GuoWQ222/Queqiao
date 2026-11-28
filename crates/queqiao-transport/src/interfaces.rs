//! This PC's uplinks: the IPv4 addresses of the network cards that are up and have a default gateway (Ethernet,
//! Wi-Fi, a mobile connection). A session of the account's own devices offers a host candidate on each, on a
//! socket bound to that address. Windows sends from a bound socket only out of the card that has the address, so
//! a session can go past a VPN or a proxy's TUN adapter holding the default route (Clash/Mihomo and the like are
//! common): through such a tunnel the answers came back from the proxy's address and nothing matched (found on
//! the test machines 2026-11-21). Tunnels have no gateway of their own and are left out here; the address the
//! server connection uses is offered anyway (it may be one). The account's own devices also get this PC's public
//! IPv6 addresses ([`ipv6_uplinks`]): no NAT there, so two PCs with IPv6 connect directly.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream};
use std::time::Duration;

// Android's kernel is Linux: the same tables (an app may be refused /proc/net there, and then has no uplinks of its own
// to offer: the session goes as the routes say).
#[cfg(any(target_os = "linux", target_os = "android"))]
mod linux;
#[cfg(windows)]
mod win;

#[cfg(any(target_os = "linux", target_os = "android"))]
pub use linux::{ipv6_uplinks, uplinks};
#[cfg(windows)]
pub use win::{ipv6_uplinks, uplinks};

/// Elsewhere (iOS, macOS): no uplinks of this program's own; the session goes as the routes say.
#[cfg(not(any(windows, target_os = "linux", target_os = "android")))]
pub fn uplinks() -> Vec<Ipv4Addr> {
    Vec::new()
}

/// As `uplinks`: none of this program's own.
#[cfg(not(any(windows, target_os = "linux", target_os = "android")))]
pub fn ipv6_uplinks(_any: bool) -> Vec<Ipv6Addr> {
    Vec::new()
}

/// Whether `address` (the server connection's) is a tunnel: not one of this PC's `uplinks` (a VPN, Tailscale or a
/// proxy's TUN adapter such as Clash's Mihomo has no gateway of its own), while there are uplinks (`others` of them
/// besides it) for the session to go on instead.
pub fn is_tunnel(address: Ipv4Addr, uplinks: &[Ipv4Addr], others: usize) -> bool {
    // The loopback is no tunnel: a server on this PC (M4 took its sessions through Wi-Fi to itself, 2026-11-23).
    others > 0 && !address.is_unspecified() && !address.is_loopback() && !uplinks.contains(&address)
}

/// The uplinks a session offers besides the server connection's `address`: all of them for the account's own devices
/// (`all`); for someone of another account none, so that they do not learn this PC's other networks, unless
/// `address` is a tunnel: then the uplink this PC itself would use (the first: `uplinks` are in route order), with
/// the tunnel after it. Through the tunnel alone a help session went where the proxy sends it: Clash Verge carried
/// the test laptop's UDP to the PC in 318 ms, Wi-Fi in 3-4 ms (2026-11-23). The other account sees what it would see
/// without the tunnel.
pub fn offered(address: Ipv4Addr, uplinks: &[Ipv4Addr], all: bool) -> Vec<Ipv4Addr> {
    let others: Vec<Ipv4Addr> = uplinks
        .iter()
        .copied()
        .filter(|ip| *ip != address)
        .collect();
    if all {
        return others;
    }
    if is_tunnel(address, uplinks, others.len()) {
        others.into_iter().take(1).collect()
    } else {
        Vec::new()
    }
}

/// A global unicast IPv6 address another network can reach: 2000::/3, not Teredo (2001::/32) or 6to4 (2002::/16).
fn public_ipv6(ip: Ipv6Addr) -> bool {
    let s = ip.segments();
    (s[0] & 0xe000) == 0x2000 && !(s[0] == 0x2001 && s[1] == 0) && s[0] != 0x2002
}

/// A unique local address (fc00::/7): reachable within one site or VPN only.
fn private_ipv6(ip: Ipv6Addr) -> bool {
    (ip.segments()[0] & 0xfe00) == 0xfc00
}

/// An address on the internet: not private, loopback, link-local, shared (100.64/10: carrier NAT and Tailscale),
/// benchmarking (198.18/15: the fake addresses of Clash/Mihomo's DNS) or otherwise reserved.
pub fn public_address(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let [a, b, ..] = ip.octets();
            usable(ip)
                && !ip.is_private()
                && !ip.is_broadcast()
                && !ip.is_documentation()
                && a != 0
                && a < 224
                && !(a == 100 && (64..128).contains(&b))
                && !(a == 198 && (18..20).contains(&b))
        }
        IpAddr::V6(ip) => public_ipv6(ip),
    }
}

/// A TCP connection to the account server at `address` that goes straight out of a network card of this PC, past
/// a VPN or a proxy's TUN adapter holding the default route (the user, 2026-11-25: "each computer connects to the
/// server directly, not via a proxy"). For a public address each uplink is tried in route order (a socket bound to
/// its address leaves through that card, see the top of this file), `timeout` each; for any other address (a LAN,
/// Tailscale, this PC), and when no uplink connects, the connection goes as the routing table says.
pub fn connect_direct(address: SocketAddr, timeout: Duration) -> std::io::Result<TcpStream> {
    connect_direct_noted(address, timeout).0
}

/// As [`connect_direct`], with what was tried, for the programs' reports ("from 183.172.248.204: connected").
pub fn connect_direct_noted(
    address: SocketAddr,
    timeout: Duration,
) -> (std::io::Result<TcpStream>, Vec<String>) {
    let mut notes = Vec::new();
    if public_address(address.ip()) {
        let sources: Vec<IpAddr> = match address {
            SocketAddr::V4(_) => uplinks().into_iter().map(IpAddr::V4).collect(),
            SocketAddr::V6(_) => ipv6_uplinks(false).into_iter().map(IpAddr::V6).collect(),
        };
        if sources.is_empty() {
            notes.push("no network card with a gateway".to_string());
        }
        for source in sources {
            match connect_from(source, address, timeout) {
                Ok(stream) => {
                    notes.push(format!("from {source}: connected"));
                    return (Ok(stream), notes);
                }
                Err(error) => notes.push(format!("from {source}: {error}")),
            }
        }
    } else {
        notes.push("not a public address: as the routes say".to_string());
    }
    let stream = TcpStream::connect_timeout(&address, timeout);
    match &stream {
        Ok(stream) => notes.push(format!(
            "as the routes say: from {}",
            stream
                .local_addr()
                .map_or_else(|_| "?".to_string(), |a| a.ip().to_string())
        )),
        Err(error) => notes.push(format!("as the routes say: {error}")),
    }
    (stream, notes)
}

fn connect_from(
    source: IpAddr,
    address: SocketAddr,
    timeout: Duration,
) -> std::io::Result<TcpStream> {
    let socket = socket2::Socket::new(
        socket2::Domain::for_address(address),
        socket2::Type::STREAM,
        Some(socket2::Protocol::TCP),
    )?;
    socket.bind(&SocketAddr::new(source, 0).into())?;
    socket.connect_timeout(&address.into(), timeout)?;
    Ok(socket.into())
}

fn usable(ip: Ipv4Addr) -> bool {
    !ip.is_loopback() && !ip.is_link_local() && !ip.is_unspecified() && !ip.is_multicast()
}

#[cfg(test)]
mod tests {
    #[test]
    fn uplinks_are_real_addresses() {
        // Whatever this PC has: no loopback, link-local or repeated address.
        let found = super::uplinks();
        for (index, ip) in found.iter().enumerate() {
            assert!(super::usable(*ip), "{ip}");
            assert!(!found[..index].contains(ip), "{ip} twice");
        }
    }

    #[test]
    fn ipv6_uplinks_are_public_or_asked_for() {
        use std::net::Ipv6Addr;
        let public = |text: &str| super::public_ipv6(text.parse::<Ipv6Addr>().unwrap());
        // The PC's campus address, a mobile network's, a home broadband's.
        assert!(public("2402:f000:4:1008:809:fffc:8a2f:5778"));
        assert!(public("2409:8a00::1"));
        assert!(public("240e:3b0::1"));
        // Link-local, unique local (Tailscale's), loopback, Teredo and 6to4 (tunnels), multicast.
        for text in [
            "fe80::1",
            "fd7a:115c:a1e0::1",
            "::1",
            "2001:0:4136::1",
            "2002:c000:204::1",
            "ff02::1",
        ] {
            assert!(!public(text), "{text}");
        }
        assert!(super::private_ipv6("fd7a:115c:a1e0::1".parse().unwrap()));
        // Whatever this PC has: public ones only, each once.
        let found = super::ipv6_uplinks(false);
        for (index, ip) in found.iter().enumerate() {
            assert!(super::public_ipv6(*ip), "{ip}");
            assert!(!found[..index].contains(ip), "{ip} twice");
        }
    }

    #[test]
    fn the_server_goes_direct_only_at_a_public_address() {
        let public = |text: &str| super::public_address(text.parse().unwrap());
        // The Aliyun server, a public address, a public IPv6 address.
        assert!(public("101.201.221.56"));
        assert!(public("1.2.3.4"));
        assert!(public("2402:f000:4:1008:809:fffc:8a2f:5778"));
        // A LAN, Tailscale, Mihomo's tunnel and fake DNS addresses, this PC, nothing, multicast, IPv6 ULA.
        for text in [
            "192.168.1.10",
            "10.0.2.2",
            "172.24.43.86",
            "100.64.0.13",
            "198.19.0.1",
            "198.18.0.5",
            "127.0.0.1",
            "0.0.0.0",
            "224.0.0.1",
            "255.255.255.255",
            "fd7a:115c:a1e0::1",
            "::1",
        ] {
            assert!(!public(text), "{text}");
        }
    }

    #[test]
    fn a_direct_connection_to_this_pc_still_connects() {
        // Not public: as the routing table says (a listener on the loopback answers).
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let stream = super::connect_direct(address, std::time::Duration::from_secs(2)).unwrap();
        assert_eq!(stream.peer_addr().unwrap(), address);
    }

    #[test]
    fn a_tunnel_is_an_address_that_is_no_uplink() {
        use std::net::Ipv4Addr;
        let wifi = Ipv4Addr::new(183, 172, 254, 58);
        let mihomo = Ipv4Addr::new(198, 19, 0, 1);
        // The proxy's tunnel holds the route to the server and Wi-Fi is there: the tunnel goes last.
        assert!(super::is_tunnel(mihomo, &[wifi], 1));
        // The server connection leaves through Wi-Fi itself: as before.
        assert!(!super::is_tunnel(wifi, &[wifi], 0));
        // Only the tunnel: it stays first (nothing else to go by).
        assert!(!super::is_tunnel(mihomo, &[], 0));
        assert!(!super::is_tunnel(Ipv4Addr::UNSPECIFIED, &[wifi], 1));
        // A server on this PC: the loopback is no tunnel.
        assert!(!super::is_tunnel(Ipv4Addr::LOCALHOST, &[wifi], 1));
    }

    #[test]
    fn someone_of_another_account_is_offered_one_uplink_only_past_a_tunnel() {
        use std::net::Ipv4Addr;
        let wifi = Ipv4Addr::new(183, 172, 254, 58);
        let mobile = Ipv4Addr::new(10, 64, 1, 2);
        let mihomo = Ipv4Addr::new(198, 19, 0, 1);
        // The account's own devices: every other uplink.
        assert_eq!(super::offered(wifi, &[wifi, mobile], true), vec![mobile]);
        assert_eq!(
            super::offered(mihomo, &[wifi, mobile], true),
            vec![wifi, mobile]
        );
        // Another account: nothing besides the route's address when that is an uplink...
        assert!(super::offered(wifi, &[wifi, mobile], false).is_empty());
        // ...and the first uplink (route order) when the route goes through a tunnel.
        assert_eq!(super::offered(mihomo, &[wifi, mobile], false), vec![wifi]);
        assert!(super::offered(mihomo, &[], false).is_empty());
        assert!(super::offered(Ipv4Addr::LOCALHOST, &[wifi], false).is_empty());
    }
}
