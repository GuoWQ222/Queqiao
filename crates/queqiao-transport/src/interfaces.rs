//! This PC's uplinks: the IPv4 addresses of the network cards that are up and have a default gateway (Ethernet,
//! Wi-Fi, a mobile connection). A session of the account's own devices offers a host candidate on each, on a
//! socket bound to that address. Windows sends from a bound socket only out of the card that has the address, so
//! a session can go past a VPN or a proxy's TUN adapter holding the default route (Clash/Mihomo and the like are
//! common): through such a tunnel the answers came back from the proxy's address and nothing matched (found on
//! the test machines 2026-11-21). Tunnels have no gateway of their own and are left out here; the address the
//! server connection uses is offered anyway (it may be one).

use std::net::Ipv4Addr;

use windows::Win32::NetworkManagement::IpHelper::{
    GAA_FLAG_INCLUDE_GATEWAYS, GAA_FLAG_SKIP_ANYCAST, GAA_FLAG_SKIP_DNS_SERVER,
    GAA_FLAG_SKIP_MULTICAST, GetAdaptersAddresses, GetIfEntry2, IP_ADAPTER_ADDRESSES_LH,
    MIB_IF_ROW2,
};
use windows::Win32::NetworkManagement::Ndis::IfOperStatusUp;
use windows::Win32::Networking::WinSock::{AF_INET, SOCKADDR, SOCKADDR_IN};

/// ERROR_BUFFER_OVERFLOW: the size needed is in `size` now.
const BUFFER_OVERFLOW: u32 = 111;

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
/// sunshine's UDP to the PC in 318 ms, Wi-Fi in 3-4 ms (2026-11-23). The other account sees what it would see
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

/// This PC's uplinks: network cards first (Windows marks them as hardware interfaces), then virtual adapters that
/// have a gateway of their own (Radmin VPN's has one: between the PC and sunshine it was taken for the PC's uplink
/// and the session went through Tailscale, 2026-11-23); each in route order (the adapter's IPv4 metric).
pub fn uplinks() -> Vec<Ipv4Addr> {
    let mut size = 32 * 1024u32;
    for _ in 0..3 {
        // u64s: the entries hold pointers and must be aligned.
        let mut buffer = vec![0u64; (size as usize).div_ceil(8)];
        let first = buffer.as_mut_ptr().cast::<IP_ADAPTER_ADDRESSES_LH>();
        let flags = GAA_FLAG_INCLUDE_GATEWAYS
            | GAA_FLAG_SKIP_ANYCAST
            | GAA_FLAG_SKIP_MULTICAST
            | GAA_FLAG_SKIP_DNS_SERVER;
        let result = unsafe {
            GetAdaptersAddresses(u32::from(AF_INET.0), flags, None, Some(first), &mut size)
        };
        if result == BUFFER_OVERFLOW {
            continue;
        }
        if result != 0 {
            return Vec::new();
        }
        let mut found: Vec<((bool, u32), Ipv4Addr)> = Vec::new();
        let mut adapter = first as *const IP_ADAPTER_ADDRESSES_LH;
        while let Some(entry) = unsafe { adapter.as_ref() } {
            if entry.OperStatus == IfOperStatusUp && has_gateway(entry) {
                let mut unicast = entry.FirstUnicastAddress;
                while let Some(address) = unsafe { unicast.as_ref() } {
                    if let Some(ip) = ipv4(address.Address.lpSockaddr)
                        && usable(ip)
                        && !found.iter().any(|(_, known)| *known == ip)
                    {
                        found.push(((!hardware(entry), entry.Ipv4Metric), ip));
                    }
                    unicast = address.Next;
                }
            }
            adapter = entry.Next;
        }
        found.sort_by_key(|(order, _)| *order);
        return found.into_iter().map(|(_, ip)| ip).collect();
    }
    Vec::new()
}

/// Whether the adapter is a network card (Windows' HardwareInterface flag), not a virtual one.
fn hardware(entry: &IP_ADAPTER_ADDRESSES_LH) -> bool {
    let mut row = MIB_IF_ROW2 {
        InterfaceLuid: entry.Luid,
        ..Default::default()
    };
    unsafe { GetIfEntry2(&mut row) }.is_ok() && row.InterfaceAndOperStatusFlags._bitfield & 1 != 0
}

fn has_gateway(entry: &IP_ADAPTER_ADDRESSES_LH) -> bool {
    let mut gateway = entry.FirstGatewayAddress;
    while let Some(address) = unsafe { gateway.as_ref() } {
        if ipv4(address.Address.lpSockaddr).is_some_and(|ip| !ip.is_unspecified()) {
            return true;
        }
        gateway = address.Next;
    }
    false
}

fn ipv4(address: *const SOCKADDR) -> Option<Ipv4Addr> {
    let address = unsafe { address.as_ref() }?;
    if address.sa_family != AF_INET {
        return None;
    }
    let v4 = unsafe { &*(address as *const SOCKADDR).cast::<SOCKADDR_IN>() };
    Some(Ipv4Addr::from(u32::from_be(unsafe {
        v4.sin_addr.S_un.S_addr
    })))
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
