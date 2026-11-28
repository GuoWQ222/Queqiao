//! The interface list on Windows: GetAdaptersAddresses, with gateways, metrics and the hardware flag.

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

use super::{private_ipv6, public_ipv6, usable};

/// ERROR_BUFFER_OVERFLOW: the size needed is in `size` now.
const BUFFER_OVERFLOW: u32 = 111;

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
/// have a gateway of their own (Radmin VPN's has one: between the PC and the test laptop it was taken for the PC's
/// uplink and the session went through Tailscale, 2026-11-23); each in route order (the adapter's IPv4 metric).
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
