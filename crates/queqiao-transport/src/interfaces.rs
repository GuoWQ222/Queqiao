//! This PC's uplinks: the IPv4 addresses of the network cards that are up and have a default gateway (Ethernet,
//! Wi-Fi, a mobile connection). A session of the account's own devices offers a host candidate on each, on a
//! socket bound to that address. Windows sends from a bound socket only out of the card that has the address, so
//! a session can go past a VPN or a proxy's TUN adapter holding the default route (Clash/Mihomo and the like are
//! common): through such a tunnel the answers came back from the proxy's address and nothing matched (found on
//! the test machines 2026-11-21). Tunnels have no gateway of their own and are left out here; the address the
//! server connection uses is offered anyway (it may be one). The account's own devices also get this PC's public
//! IPv6 addresses ([`ipv6_uplinks`]): no NAT there, so two PCs with IPv6 connect directly.

use std::net::{Ipv4Addr, Ipv6Addr};

use windows::Win32::NetworkManagement::IpHelper::{
    GAA_FLAG_INCLUDE_GATEWAYS, GAA_FLAG_SKIP_ANYCAST, GAA_FLAG_SKIP_DNS_SERVER,
    GAA_FLAG_SKIP_MULTICAST, GetAdaptersAddresses, GetIfEntry2, IP_ADAPTER_ADDRESSES_LH,
    MIB_IF_ROW2,
};
use windows::Win32::NetworkManagement::Ndis::IfOperStatusUp;
use windows::Win32::Networking::WinSock::{
    AF_INET, AF_INET6, IpDadStatePreferred, IpSuffixOriginLinkLayerAddress, SOCKADDR, SOCKADDR_IN,
    SOCKADDR_IN6,
};

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

/// Calls `each` with every adapter Windows lists for `family` (AF_INET or AF_INET6), gateways included.
fn for_each_adapter(family: u16, mut each: impl FnMut(&IP_ADAPTER_ADDRESSES_LH)) {
    let mut size = 32 * 1024u32;
    for _ in 0..3 {
        // u64s: the entries hold pointers and must be aligned.
        let mut buffer = vec![0u64; (size as usize).div_ceil(8)];
        let first = buffer.as_mut_ptr().cast::<IP_ADAPTER_ADDRESSES_LH>();
        let flags = GAA_FLAG_INCLUDE_GATEWAYS
            | GAA_FLAG_SKIP_ANYCAST
            | GAA_FLAG_SKIP_MULTICAST
            | GAA_FLAG_SKIP_DNS_SERVER;
        let result =
            unsafe { GetAdaptersAddresses(u32::from(family), flags, None, Some(first), &mut size) };
        if result == BUFFER_OVERFLOW {
            continue;
        }
        if result != 0 {
            return;
        }
        let mut adapter = first as *const IP_ADAPTER_ADDRESSES_LH;
        while let Some(entry) = unsafe { adapter.as_ref() } {
            each(entry);
            adapter = entry.Next;
        }
        return;
    }
}

/// This PC's uplinks: network cards first (Windows marks them as hardware interfaces), then virtual adapters that
/// have a gateway of their own (Radmin VPN's has one: between the PC and sunshine it was taken for the PC's uplink
/// and the session went through Tailscale, 2026-11-23); each in route order (the adapter's IPv4 metric).
pub fn uplinks() -> Vec<Ipv4Addr> {
    let mut found: Vec<((bool, u32), Ipv4Addr)> = Vec::new();
    for_each_adapter(AF_INET.0, |entry| {
        if entry.OperStatus != IfOperStatusUp || !has_gateway(entry, false) {
            return;
        }
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
    });
    found.sort_by_key(|(order, _)| *order);
    found.into_iter().map(|(_, ip)| ip).collect()
}

/// This PC's public IPv6 addresses (2026-11-24): one for each adapter that is up and has an IPv6 default gateway,
/// network cards first, in route order. IPv6 has no NAT: the address is the one the other side reaches, so two PCs
/// with IPv6 meet directly, their firewalls letting the other's packets in as answers once both have sent (ICE's
/// checks go both ways at once). Only global addresses (2000::/3; not Teredo or 6to4, which are tunnels), only
/// preferred ones (not tentative or deprecated), and of an adapter's addresses one not made from its hardware
/// (MAC) address when there is one: Windows' random or temporary addresses say less about the PC.
///
/// `any` (tests on one PC, QUEQIAO_FORCE=ipv6:any): also private (fc00::/7) addresses and adapters without a
/// gateway, such as Tailscale's.
pub fn ipv6_uplinks(any: bool) -> Vec<Ipv6Addr> {
    let mut found: Vec<((bool, u32), Ipv6Addr)> = Vec::new();
    for_each_adapter(AF_INET6.0, |entry| {
        if entry.OperStatus != IfOperStatusUp || !(any || has_gateway(entry, true)) {
            return;
        }
        // (made from the hardware address, the address): the first of the best kind.
        let mut best: Option<(bool, Ipv6Addr)> = None;
        let mut unicast = entry.FirstUnicastAddress;
        while let Some(address) = unsafe { unicast.as_ref() } {
            if let Some(ip) = ipv6(address.Address.lpSockaddr)
                && (public_ipv6(ip) || (any && private_ipv6(ip)))
                && address.DadState == IpDadStatePreferred
            {
                let from_hardware = address.SuffixOrigin == IpSuffixOriginLinkLayerAddress;
                if best.is_none_or(|(known_hardware, _)| known_hardware && !from_hardware) {
                    best = Some((from_hardware, ip));
                }
            }
            unicast = address.Next;
        }
        if let Some((_, ip)) = best
            && !found.iter().any(|(_, known)| *known == ip)
        {
            found.push(((!hardware(entry), entry.Ipv6Metric), ip));
        }
    });
    found.sort_by_key(|(order, _)| *order);
    found.into_iter().map(|(_, ip)| ip).collect()
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

/// Whether the adapter is a network card (Windows' HardwareInterface flag), not a virtual one.
fn hardware(entry: &IP_ADAPTER_ADDRESSES_LH) -> bool {
    let mut row = MIB_IF_ROW2 {
        InterfaceLuid: entry.Luid,
        ..Default::default()
    };
    unsafe { GetIfEntry2(&mut row) }.is_ok() && row.InterfaceAndOperStatusFlags._bitfield & 1 != 0
}

/// Whether the adapter has a default gateway of the family (`v6`: IPv6, else IPv4).
fn has_gateway(entry: &IP_ADAPTER_ADDRESSES_LH, v6: bool) -> bool {
    let mut gateway = entry.FirstGatewayAddress;
    while let Some(address) = unsafe { gateway.as_ref() } {
        let found = if v6 {
            ipv6(address.Address.lpSockaddr).is_some_and(|ip| !ip.is_unspecified())
        } else {
            ipv4(address.Address.lpSockaddr).is_some_and(|ip| !ip.is_unspecified())
        };
        if found {
            return true;
        }
        gateway = address.Next;
    }
    false
}

fn ipv6(address: *const SOCKADDR) -> Option<Ipv6Addr> {
    let address = unsafe { address.as_ref() }?;
    if address.sa_family != AF_INET6 {
        return None;
    }
    let v6 = unsafe { &*(address as *const SOCKADDR).cast::<SOCKADDR_IN6>() };
    Some(Ipv6Addr::from(unsafe { v6.sin6_addr.u.Byte }))
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
