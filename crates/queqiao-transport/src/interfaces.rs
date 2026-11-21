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
    GAA_FLAG_SKIP_MULTICAST, GetAdaptersAddresses, IP_ADAPTER_ADDRESSES_LH,
};
use windows::Win32::NetworkManagement::Ndis::IfOperStatusUp;
use windows::Win32::Networking::WinSock::{AF_INET, SOCKADDR, SOCKADDR_IN};

/// ERROR_BUFFER_OVERFLOW: the size needed is in `size` now.
const BUFFER_OVERFLOW: u32 = 111;

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
        let mut found = Vec::new();
        let mut adapter = first as *const IP_ADAPTER_ADDRESSES_LH;
        while let Some(entry) = unsafe { adapter.as_ref() } {
            if entry.OperStatus == IfOperStatusUp && has_gateway(entry) {
                let mut unicast = entry.FirstUnicastAddress;
                while let Some(address) = unsafe { unicast.as_ref() } {
                    if let Some(ip) = ipv4(address.Address.lpSockaddr)
                        && usable(ip)
                        && !found.contains(&ip)
                    {
                        found.push(ip);
                    }
                    unicast = address.Next;
                }
            }
            adapter = entry.Next;
        }
        return found;
    }
    Vec::new()
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
}
