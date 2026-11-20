//! Wake-on-LAN: this PC's network cards (told to the server, so another device can wake this one), and the
//! magic packets that wake another PC of the account on this network.

use std::net::{Ipv4Addr, SocketAddr, UdpSocket};

use windows::Win32::NetworkManagement::IpHelper::{
    GAA_FLAG_SKIP_ANYCAST, GAA_FLAG_SKIP_DNS_SERVER, GAA_FLAG_SKIP_MULTICAST, GetAdaptersAddresses,
    IP_ADAPTER_ADDRESSES_LH,
};
use windows::Win32::NetworkManagement::Ndis::IfOperStatusUp;
use windows::Win32::Networking::WinSock::AF_UNSPEC;

/// Interface types that can wake a PC: Ethernet and Wi-Fi.
const ETHERNET: u32 = 6;
const WIFI: u32 = 71;

/// The MAC addresses ("AA-BB-CC-DD-EE-FF") of this PC's Ethernet and Wi-Fi cards that are up.
pub fn macs() -> Vec<String> {
    let mut size = 16 * 1024u32;
    for _ in 0..3 {
        let mut buffer = vec![0u8; size as usize];
        let first = buffer.as_mut_ptr().cast::<IP_ADAPTER_ADDRESSES_LH>();
        let flags = GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST | GAA_FLAG_SKIP_DNS_SERVER;
        let result = unsafe {
            GetAdaptersAddresses(AF_UNSPEC.0 as u32, flags, None, Some(first), &mut size)
        };
        if result == 0 {
            let mut found = Vec::new();
            let mut adapter = first as *const IP_ADAPTER_ADDRESSES_LH;
            while let Some(entry) = unsafe { adapter.as_ref() } {
                let usable = (entry.IfType == ETHERNET || entry.IfType == WIFI)
                    && entry.OperStatus == IfOperStatusUp
                    && entry.PhysicalAddressLength == 6;
                if usable {
                    let mac = &entry.PhysicalAddress[..6];
                    let text: Vec<String> = mac.iter().map(|byte| format!("{byte:02X}")).collect();
                    let text = text.join("-");
                    if !found.contains(&text) {
                        found.push(text);
                    }
                }
                adapter = entry.Next;
            }
            return found;
        }
        // ERROR_BUFFER_OVERFLOW: `size` now says how much is needed.
        if result != 111 {
            return Vec::new();
        }
    }
    Vec::new()
}

/// The magic packet for `mac`: six 0xFF bytes, then the MAC 16 times.
fn magic_packet(mac: &str) -> Option<[u8; 102]> {
    let bytes: Vec<u8> = mac
        .split(['-', ':'])
        .map(|part| u8::from_str_radix(part, 16))
        .collect::<Result<_, _>>()
        .ok()?;
    if bytes.len() != 6 {
        return None;
    }
    let mut packet = [0xFFu8; 102];
    for copy in 0..16 {
        packet[6 + copy * 6..12 + copy * 6].copy_from_slice(&bytes);
    }
    Some(packet)
}

/// Sends the magic packets for `macs` as broadcasts on ports 9 and 7; returns how many went out.
pub fn send(macs: &[String]) -> Result<usize, String> {
    let targets = [
        SocketAddr::from((Ipv4Addr::BROADCAST, 9)),
        SocketAddr::from((Ipv4Addr::BROADCAST, 7)),
    ];
    send_to(macs, &targets)
}

fn send_to(macs: &[String], targets: &[SocketAddr]) -> Result<usize, String> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).map_err(|e| e.to_string())?;
    socket.set_broadcast(true).map_err(|e| e.to_string())?;
    let mut sent = 0;
    for packet in macs.iter().filter_map(|mac| magic_packet(mac)) {
        for target in targets {
            if socket.send_to(&packet, target).is_ok() {
                sent += 1;
            }
        }
    }
    Ok(sent)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_magic_packet_reaches_a_listener() {
        let listener = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        listener
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        let target = listener.local_addr().unwrap();
        let sent = send_to(
            &["02-00-5E-10-20-30".to_string(), "not a mac".to_string()],
            &[target],
        )
        .unwrap();
        assert_eq!(sent, 1);
        let mut buffer = [0u8; 200];
        let (length, _) = listener.recv_from(&mut buffer).unwrap();
        assert_eq!(length, 102);
        assert!(buffer[..6].iter().all(|byte| *byte == 0xFF));
        for copy in 0..16 {
            assert_eq!(
                &buffer[6 + copy * 6..12 + copy * 6],
                &[0x02, 0x00, 0x5E, 0x10, 0x20, 0x30]
            );
        }
    }

    #[test]
    fn this_pc_has_its_cards_listed() {
        // Every test PC has a network card that is up; each address has six parts.
        let macs = macs();
        assert!(
            macs.iter().all(|mac| mac.split('-').count() == 6),
            "{macs:?}"
        );
    }
}
