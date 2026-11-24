//! The interface list on Linux, by the same rules as on Windows: an uplink is an interface that is up and holds a
//! default route through a gateway (a tunnel such as Mihomo, Tailscale or a VPN routes without one, or holds no
//! default route); network cards (a device behind `/sys/class/net/<name>/device`) come first, then the route's
//! metric. IPv4 addresses come from `getifaddrs`, routes from the kernel's tables (`/proc/net/route`,
//! `/proc/net/ipv6_route`), IPv6 address states from `/proc/net/if_inet6`.

use std::collections::HashMap;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::path::Path;

use super::{private_ipv6, public_ipv6, usable};

/// IPv6 address flags in `/proc/net/if_inet6` (linux/if_addr.h).
const IFA_F_TEMPORARY: u32 = 0x01;
const IFA_F_DADFAILED: u32 = 0x08;
const IFA_F_DEPRECATED: u32 = 0x20;
const IFA_F_TENTATIVE: u32 = 0x40;
/// Route flags (linux/route.h).
const RTF_UP: u32 = 0x1;
const RTF_GATEWAY: u32 = 0x2;

/// This PC's uplinks: network cards first, then virtual adapters with a gateway of their own; each in route order.
pub fn uplinks() -> Vec<Ipv4Addr> {
    let gateways = gateways_v4(&std::fs::read_to_string("/proc/net/route").unwrap_or_default());
    let mut found: Vec<((bool, u32), Ipv4Addr)> = Vec::new();
    for (name, ip) in addresses_v4() {
        if let Some(metric) = gateways.get(&name)
            && usable(ip)
            && !found.iter().any(|(_, known)| *known == ip)
        {
            found.push(((!hardware(&name), *metric), ip));
        }
    }
    found.sort_by_key(|(order, _)| *order);
    found.into_iter().map(|(_, ip)| ip).collect()
}

/// This PC's public IPv6 addresses: one for each interface that is up and has an IPv6 default route through a
/// gateway, network cards first, in route order; preferred addresses only (not tentative, deprecated or failed),
/// and of an interface's addresses a temporary or random one rather than one made from its hardware address (EUI-64)
/// when there is one. `any` (tests on one PC): private (fc00::/7) addresses too, and interfaces without a gateway.
pub fn ipv6_uplinks(any: bool) -> Vec<Ipv6Addr> {
    let gateways =
        gateways_v6(&std::fs::read_to_string("/proc/net/ipv6_route").unwrap_or_default());
    let table = std::fs::read_to_string("/proc/net/if_inet6").unwrap_or_default();
    // Per interface: (made from the hardware address, the address); the first of the best kind.
    let mut best: Vec<(String, bool, Ipv6Addr)> = Vec::new();
    for (ip, flags, name) in inet6_addresses(&table) {
        if flags & (IFA_F_DADFAILED | IFA_F_DEPRECATED | IFA_F_TENTATIVE) != 0
            || !(public_ipv6(ip) || (any && private_ipv6(ip)))
            || !(any || gateways.contains_key(&name))
            || !is_up(&name)
        {
            continue;
        }
        let from_hardware = flags & IFA_F_TEMPORARY == 0 && made_from_hardware(ip);
        match best.iter_mut().find(|(known, _, _)| *known == name) {
            Some(entry) if entry.1 && !from_hardware => *entry = (name, from_hardware, ip),
            Some(_) => {}
            None => best.push((name, from_hardware, ip)),
        }
    }
    let mut found: Vec<((bool, u32), Ipv6Addr)> = best
        .into_iter()
        .map(|(name, _, ip)| {
            let metric = gateways.get(&name).copied().unwrap_or(u32::MAX);
            ((!hardware(&name), metric), ip)
        })
        .collect();
    found.sort_by_key(|(order, _)| *order);
    found.into_iter().map(|(_, ip)| ip).collect()
}

/// The IPv4 addresses of the interfaces that are up: (interface name, address).
fn addresses_v4() -> Vec<(String, Ipv4Addr)> {
    let mut list: *mut libc::ifaddrs = std::ptr::null_mut();
    if unsafe { libc::getifaddrs(&mut list) } != 0 {
        return Vec::new();
    }
    let mut found = Vec::new();
    let mut entry = list;
    while let Some(item) = unsafe { entry.as_ref() } {
        let up = item.ifa_flags & libc::IFF_UP as libc::c_uint != 0;
        if up
            && let Some(address) = unsafe { item.ifa_addr.as_ref() }
            && i32::from(address.sa_family) == libc::AF_INET
        {
            let v4 = unsafe { &*(item.ifa_addr as *const libc::sockaddr_in) };
            let ip = Ipv4Addr::from(u32::from_be(v4.sin_addr.s_addr));
            let name = unsafe { std::ffi::CStr::from_ptr(item.ifa_name) }
                .to_string_lossy()
                .into_owned();
            found.push((name, ip));
        }
        entry = item.ifa_next;
    }
    unsafe { libc::freeifaddrs(list) };
    found
}

/// From `/proc/net/route`: the interfaces with a default route through a gateway, each with that route's lowest
/// metric.
fn gateways_v4(table: &str) -> HashMap<String, u32> {
    let mut found = HashMap::new();
    // Iface Destination Gateway Flags RefCnt Use Metric Mask MTU Window IRTT (addresses in hex).
    for line in table.lines().skip(1) {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 8 {
            continue;
        }
        let flags = u32::from_str_radix(fields[3], 16).unwrap_or(0);
        let default = fields[1] == "00000000" && fields[7] == "00000000";
        if default
            && fields[2] != "00000000"
            && flags & (RTF_UP | RTF_GATEWAY) == RTF_UP | RTF_GATEWAY
        {
            let metric = fields[6].parse().unwrap_or(u32::MAX);
            let lowest = found.entry(fields[0].to_string()).or_insert(metric);
            *lowest = (*lowest).min(metric);
        }
    }
    found
}

/// From `/proc/net/ipv6_route`: the interfaces with a default route (::/0) through a gateway, each with its lowest
/// metric.
fn gateways_v6(table: &str) -> HashMap<String, u32> {
    const NONE: &str = "00000000000000000000000000000000";
    let mut found = HashMap::new();
    // destination, its prefix length, source, its prefix length, next hop, metric, refcount, use, flags, interface.
    for line in table.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 10 {
            continue;
        }
        let flags = u32::from_str_radix(fields[8], 16).unwrap_or(0);
        let default = fields[0] == NONE && fields[1] == "00";
        if default && fields[4] != NONE && flags & RTF_UP != 0 {
            let metric = u32::from_str_radix(fields[5], 16).unwrap_or(u32::MAX);
            let lowest = found.entry(fields[9].to_string()).or_insert(metric);
            *lowest = (*lowest).min(metric);
        }
    }
    found
}

/// From `/proc/net/if_inet6`: (address, flags, interface name) for each address.
fn inet6_addresses(table: &str) -> Vec<(Ipv6Addr, u32, String)> {
    table
        .lines()
        .filter_map(|line| {
            // address (32 hex digits), interface index, prefix length, scope, flags, name.
            let fields: Vec<&str> = line.split_whitespace().collect();
            if fields.len() < 6 || fields[0].len() != 32 {
                return None;
            }
            let mut bytes = [0u8; 16];
            for (index, byte) in bytes.iter_mut().enumerate() {
                *byte = u8::from_str_radix(fields[0].get(index * 2..index * 2 + 2)?, 16).ok()?;
            }
            let flags = u32::from_str_radix(fields[4], 16).ok()?;
            Some((Ipv6Addr::from(bytes), flags, fields[5].to_string()))
        })
        .collect()
}

/// An interface id made from the hardware address (modified EUI-64: ff:fe in its middle).
fn made_from_hardware(ip: Ipv6Addr) -> bool {
    let bytes = ip.octets();
    bytes[11] == 0xff && bytes[12] == 0xfe
}

/// Whether an interface is up (IFF_UP in its flags).
fn is_up(name: &str) -> bool {
    std::fs::read_to_string(Path::new("/sys/class/net").join(name).join("flags"))
        .ok()
        .and_then(|text| u32::from_str_radix(text.trim().trim_start_matches("0x"), 16).ok())
        .is_some_and(|flags| flags & libc::IFF_UP as u32 != 0)
}

/// Whether an interface is a network card: a device stands behind it (virtual and tunnel interfaces have none).
fn hardware(name: &str) -> bool {
    Path::new("/sys/class/net")
        .join(name)
        .join("device")
        .exists()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_routes_through_a_gateway() {
        // Wi-Fi with a gateway (metric 600), Ethernet with one (100) and a second, worse one (200), Clash's tunnel
        // holding default routes without a gateway, a plain subnet route.
        let table = "Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT\n\
            wlan0\t00000000\t0101A8C0\t0003\t0\t0\t600\t00000000\t0\t0\t0\n\
            eth0\t00000000\t01FEA8C0\t0003\t0\t0\t100\t00000000\t0\t0\t0\n\
            eth0\t00000000\t02FEA8C0\t0003\t0\t0\t200\t00000000\t0\t0\t0\n\
            Mihomo\t00000000\t00000000\t0001\t0\t0\t0\t00000000\t0\t0\t0\n\
            eth0\t00FEA8C0\t00000000\t0001\t0\t0\t100\t00FFFFFF\t0\t0\t0\n";
        let found = gateways_v4(table);
        assert_eq!(found.get("eth0"), Some(&100));
        assert_eq!(found.get("wlan0"), Some(&600));
        assert_eq!(found.len(), 2, "{found:?}");
    }

    #[test]
    fn ipv6_default_routes_and_addresses() {
        let routes = "00000000000000000000000000000000 00 00000000000000000000000000000000 00 fe80000000000000165174fffe6d2002 00000400 00000001 00000000 00000003  enp3s0\n\
            00000000000000000000000000000000 00 00000000000000000000000000000000 00 00000000000000000000000000000000 00000000 00000001 00000000 00000001 tailscale0\n\
            24020000000000000000000000000000 40 00000000000000000000000000000000 00 00000000000000000000000000000000 00000100 00000001 00000000 00000001  enp3s0\n";
        let found = gateways_v6(routes);
        assert_eq!(found.get("enp3s0"), Some(&0x400));
        assert_eq!(found.len(), 1, "{found:?}");

        let addresses = "2402f00000040008080900fffe2f5778 02 40 00 00   enp3s0\n\
            2402f000000400084d3dace0d43e021d 02 40 00 01   enp3s0\n\
            fe800000000000002cc51fca1492093a 02 40 20 80   enp3s0\n\
            fd7a115ca1e000000000000000093b78 05 80 00 80 tailscale0\n";
        let parsed = inet6_addresses(addresses);
        assert_eq!(parsed.len(), 4);
        assert_eq!(
            parsed[0].0,
            "2402:f000:4:8:809:ff:fe2f:5778"
                .parse::<Ipv6Addr>()
                .unwrap()
        );
        assert!(made_from_hardware(parsed[0].0));
        assert_eq!(parsed[1].1, IFA_F_TEMPORARY);
        assert!(!made_from_hardware(parsed[1].0));
        assert_eq!(parsed[3].2, "tailscale0");
    }

    #[test]
    fn this_pcs_lists_are_real_addresses() {
        for ip in uplinks() {
            assert!(usable(ip), "{ip}");
        }
        for ip in ipv6_uplinks(false) {
            assert!(public_ipv6(ip), "{ip}");
        }
    }
}
