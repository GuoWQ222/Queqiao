//! The Wake-on-LAN set-up check ("Wake-on-LAN check..." in the tray): what Windows knows about whether another device
//! can wake this PC. For each physical network card: may it wake the PC (`powercfg -devicequery wake_armed`), and does
//! its driver wake on a magic packet (`*WakeOnMagicPacket` in the card's settings); and whether Fast Startup is on
//! (with it, "Shut down" is a kind of hibernation that some cards do not wake from). The BIOS (UEFI) setting cannot be
//! read from Windows: the advice names it. Nothing is changed (that needs an administrator, and is the person's call).

use std::os::windows::process::CommandExt;

use windows::Win32::Globalization::{CP_OEMCP, MULTI_BYTE_TO_WIDE_CHAR_FLAGS, MultiByteToWideChar};
use windows::Win32::NetworkManagement::IpHelper::{
    GAA_FLAG_SKIP_ANYCAST, GAA_FLAG_SKIP_DNS_SERVER, GAA_FLAG_SKIP_MULTICAST, GetAdaptersAddresses,
    IP_ADAPTER_ADDRESSES_LH,
};
use windows::Win32::Networking::WinSock::AF_UNSPEC;
use windows::Win32::System::Registry::{
    HKEY, HKEY_LOCAL_MACHINE, KEY_READ, RRF_RT_REG_DWORD, RRF_RT_REG_SZ, RegCloseKey,
    RegEnumKeyExW, RegGetValueW, RegOpenKeyExW,
};
use windows::core::{PCWSTR, PWSTR};

use queqiao_core::text::{tr, trf};

const ETHERNET: u32 = 6;
const WIFI: u32 = 71;
/// Network cards' settings (the network adapter device class).
const NET_CLASS: &str =
    r"SYSTEM\CurrentControlSet\Control\Class\{4d36e972-e325-11ce-bfc1-08002be10318}";
/// NCF_PHYSICAL in a card's Characteristics: a real card, not a virtual or VPN adapter.
const NCF_PHYSICAL: u32 = 0x4;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Card {
    /// The connection's name ("Ethernet") and the card's ("Realtek PCIe GbE Family Controller").
    pub connection: String,
    pub device: String,
    pub wired: bool,
    /// May wake the PC; None: unknown.
    pub armed: Option<bool>,
    /// Its driver wakes on a magic packet; None: the driver does not say.
    pub magic_packet: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Check {
    pub cards: Vec<Card>,
    pub fast_startup: Option<bool>,
}

pub fn check() -> Check {
    let armed = wake_armed();
    let cards = physical_cards()
        .into_iter()
        .map(|(connection, guid, wired)| {
            let settings = card_settings(&guid);
            let device = settings.as_ref().map(|s| s.0.clone()).unwrap_or_default();
            Card {
                armed: armed
                    .as_ref()
                    .map(|names| names.iter().any(|name| same_device(name, &device))),
                magic_packet: settings.and_then(|s| s.1),
                connection,
                device,
                wired,
            }
        })
        .collect();
    let fast_startup = read_dword(
        r"SYSTEM\CurrentControlSet\Control\Session Manager\Power",
        "HiberbootEnabled",
    )
    .map(|value| value != 0);
    Check {
        cards,
        fast_startup,
    }
}

fn yes_no(value: Option<bool>) -> &'static str {
    match value {
        Some(true) => "yes",
        Some(false) => "no",
        None => "unknown",
    }
}

/// For reports (English).
pub fn report(check: &Check) -> Vec<String> {
    let mut lines: Vec<String> = check
        .cards
        .iter()
        .map(|card| {
            format!(
                "wake-on-lan: {} ({}, {}): may wake the PC: {}; on a magic packet: {}",
                card.connection,
                card.device,
                if card.wired { "wired" } else { "Wi-Fi" },
                yes_no(card.armed),
                yes_no(card.magic_packet)
            )
        })
        .collect();
    if check.cards.is_empty() {
        lines.push("wake-on-lan: no physical network card".into());
    }
    lines.push(format!(
        "wake-on-lan: fast startup on: {}",
        yes_no(check.fast_startup)
    ));
    lines.push(format!(
        "wake-on-lan: ready as far as Windows can tell: {}",
        if ready(check) { "yes" } else { "no" }
    ));
    lines
}

/// A wired card that may wake the PC and does not refuse magic packets, and Fast Startup not on.
pub fn ready(check: &Check) -> bool {
    check
        .cards
        .iter()
        .any(|card| card.wired && card.armed == Some(true) && card.magic_packet != Some(false))
        && check.fast_startup != Some(true)
}

/// For the person: what was found, and what to set where it is not.
pub fn advice(check: &Check) -> String {
    let mut out = vec![tr("Wake-on-LAN on this PC (another device of your account wakes it through a PC on the same network):").to_string()];
    let word = |value: Option<bool>| match value {
        Some(true) => tr("yes"),
        Some(false) => tr("no"),
        None => tr("unknown"),
    };
    for card in &check.cards {
        out.push(trf(
            "- {connection} ({device}): may wake the PC: {armed}; on a magic packet: {magic}",
            &[
                ("connection", &card.connection),
                ("device", &card.device),
                ("armed", word(card.armed)),
                ("magic", word(card.magic_packet)),
            ],
        ));
        if !card.wired {
            out.push(
                tr("  A Wi-Fi card can seldom wake a PC that is off: use a network cable for it.")
                    .to_string(),
            );
        }
    }
    if check.cards.is_empty() {
        out.push(tr("- No network card found.").to_string());
    }
    if check.fast_startup == Some(true) {
        out.push(tr("- Fast Startup is on: many network cards do not wake a PC shut down with it. Turn it off in Control Panel > Power Options > Choose what the power buttons do.").to_string());
    }
    out.push(String::new());
    if ready(check) {
        out.push(tr("Windows is ready. If the PC still does not wake, turn on Wake on LAN (or Power On by PCI-E) in its BIOS/UEFI.").to_string());
    } else {
        out.push(tr("To set: Device Manager > Network adapters > the wired card > Power Management: Allow this device to wake the computer, and Only allow a magic packet; Advanced: Wake on Magic Packet: Enabled (this needs an administrator). Then Wake on LAN in the BIOS/UEFI.").to_string());
    }
    out.join("\r\n")
}

/// (connection name, adapter GUID, wired) of the physical Ethernet and Wi-Fi cards.
fn physical_cards() -> Vec<(String, String, bool)> {
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
                if (entry.IfType == ETHERNET || entry.IfType == WIFI)
                    && entry.PhysicalAddressLength == 6
                {
                    let guid = unsafe { entry.AdapterName.to_string() }.unwrap_or_default();
                    let connection = unsafe { entry.FriendlyName.to_string() }.unwrap_or_default();
                    let physical =
                        card_characteristics(&guid).is_some_and(|c| c & NCF_PHYSICAL != 0);
                    if physical
                        && !found
                            .iter()
                            .any(|(_, known, _): &(String, String, bool)| *known == guid)
                    {
                        found.push((connection, guid, entry.IfType == ETHERNET));
                    }
                }
                adapter = entry.Next;
            }
            return found;
        }
        if result != 111 {
            return Vec::new();
        }
    }
    Vec::new()
}

/// The card's settings key under the network class (the subkey whose NetCfgInstanceId is the adapter's GUID).
fn card_key(guid: &str) -> Option<String> {
    let class = wide(NET_CLASS);
    let mut key = HKEY::default();
    unsafe {
        RegOpenKeyExW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(class.as_ptr()),
            None,
            KEY_READ,
            &mut key,
        )
    }
    .ok()
    .ok()?;
    let mut found = None;
    for index in 0..1000u32 {
        let mut name = [0u16; 256];
        let mut length = name.len() as u32;
        let listed = unsafe {
            RegEnumKeyExW(
                key,
                index,
                Some(PWSTR(name.as_mut_ptr())),
                &mut length,
                None,
                None,
                None,
                None,
            )
        };
        if listed.is_err() {
            break;
        }
        let sub = format!(
            r"{NET_CLASS}\{}",
            String::from_utf16_lossy(&name[..length as usize])
        );
        if read_text(&sub, "NetCfgInstanceId").is_some_and(|id| id.eq_ignore_ascii_case(guid)) {
            found = Some(sub);
            break;
        }
    }
    let _ = unsafe { RegCloseKey(key) };
    found
}

fn card_characteristics(guid: &str) -> Option<u32> {
    read_dword(&card_key(guid)?, "Characteristics")
}

/// (the card's name, wakes on a magic packet).
fn card_settings(guid: &str) -> Option<(String, Option<bool>)> {
    let key = card_key(guid)?;
    let name = read_text(&key, "DriverDesc").unwrap_or_default();
    let magic = read_text(&key, "*WakeOnMagicPacket").map(|value| value.trim() != "0");
    Some((name, magic))
}

/// The devices that may wake the PC, as `powercfg -devicequery wake_armed` lists them (no administrator needed).
fn wake_armed() -> Option<Vec<String>> {
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let output = std::process::Command::new("powercfg")
        .args(["-devicequery", "wake_armed"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| parse_device_list(&oem_text(&output.stdout)))
}

/// Whether powercfg's `listed` name is the card `device` (its driver's name): Windows adds " #2" and so on to the
/// names of a second card of the same kind.
fn same_device(listed: &str, device: &str) -> bool {
    if device.is_empty() {
        return false;
    }
    let (listed, device) = (listed.to_lowercase(), device.to_lowercase());
    match listed.strip_prefix(&device) {
        Some("") => true,
        Some(rest) => rest
            .strip_prefix(" #")
            .is_some_and(|number| !number.is_empty() && number.bytes().all(|b| b.is_ascii_digit())),
        None => false,
    }
}

/// Device names, one per line; "NONE" (or a translation of it, a single word) when there are none.
fn parse_device_list(text: &str) -> Vec<String> {
    let names: Vec<String> = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect();
    if names.len() == 1 && names[0].eq_ignore_ascii_case("none") {
        return Vec::new();
    }
    names
}

/// Console output in the OEM code page (GBK on a Chinese PC) as text.
fn oem_text(bytes: &[u8]) -> String {
    if bytes.is_empty() {
        return String::new();
    }
    let length =
        unsafe { MultiByteToWideChar(CP_OEMCP, MULTI_BYTE_TO_WIDE_CHAR_FLAGS(0), bytes, None) };
    if length <= 0 {
        return String::from_utf8_lossy(bytes).into_owned();
    }
    let mut wide = vec![0u16; length as usize];
    unsafe {
        MultiByteToWideChar(
            CP_OEMCP,
            MULTI_BYTE_TO_WIDE_CHAR_FLAGS(0),
            bytes,
            Some(&mut wide),
        )
    };
    String::from_utf16_lossy(&wide)
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

fn read_text(key: &str, name: &str) -> Option<String> {
    let (key, name) = (wide(key), wide(name));
    let mut buffer = vec![0u16; 1024];
    let mut size = (buffer.len() * 2) as u32;
    unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(key.as_ptr()),
            PCWSTR(name.as_ptr()),
            RRF_RT_REG_SZ,
            None,
            Some(buffer.as_mut_ptr().cast()),
            Some(&mut size),
        )
    }
    .ok()
    .ok()?;
    let length = (size as usize / 2).saturating_sub(1).min(buffer.len());
    Some(String::from_utf16_lossy(&buffer[..length]))
}

fn read_dword(key: &str, name: &str) -> Option<u32> {
    let (key, name) = (wide(key), wide(name));
    let mut value = 0u32;
    let mut size = 4u32;
    unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(key.as_ptr()),
            PCWSTR(name.as_ptr()),
            RRF_RT_REG_DWORD,
            None,
            Some((&mut value as *mut u32).cast()),
            Some(&mut size),
        )
    }
    .ok()
    .ok()?;
    Some(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card(wired: bool, armed: Option<bool>, magic: Option<bool>) -> Card {
        Card {
            connection: "Ethernet".into(),
            device: "Realtek PCIe GbE Family Controller".into(),
            wired,
            armed,
            magic_packet: magic,
        }
    }

    #[test]
    fn listed_names_match_with_a_number_after() {
        let card = "MediaTek Wi-Fi 7 MT7925 Wireless LAN Card";
        assert!(same_device(card, card));
        assert!(same_device(
            "MediaTek Wi-Fi 7 MT7925 Wireless LAN Card #2",
            card
        ));
        assert!(same_device(
            "mediatek wi-fi 7 mt7925 wireless lan card #12",
            card
        ));
        assert!(!same_device(
            "MediaTek Wi-Fi 7 MT7925 Wireless LAN Card Extra",
            card
        ));
        assert!(!same_device(
            "MediaTek Wi-Fi 7 MT7925 Wireless LAN Card #",
            card
        ));
        assert!(!same_device("MediaTek Wi-Fi 7", card));
        assert!(!same_device("anything", ""));
    }

    #[test]
    fn device_lists() {
        assert_eq!(parse_device_list("NONE\r\n"), Vec::<String>::new());
        assert_eq!(
            parse_device_list("HID Keyboard Device\r\nRealtek PCIe GbE Family Controller\r\n\r\n"),
            vec!["HID Keyboard Device", "Realtek PCIe GbE Family Controller"]
        );
    }

    #[test]
    fn ready_needs_an_armed_wired_card_and_no_fast_startup() {
        let check = |cards, fast_startup| Check {
            cards,
            fast_startup,
        };
        assert!(ready(&check(
            vec![card(true, Some(true), Some(true))],
            Some(false)
        )));
        // A driver that does not say is not counted against it.
        assert!(ready(&check(vec![card(true, Some(true), None)], None)));
        assert!(!ready(&check(
            vec![card(true, Some(true), Some(true))],
            Some(true)
        )));
        assert!(!ready(&check(
            vec![card(true, Some(false), Some(true))],
            Some(false)
        )));
        assert!(!ready(&check(
            vec![card(true, Some(true), Some(false))],
            Some(false)
        )));
        assert!(!ready(&check(
            vec![card(false, Some(true), Some(true))],
            Some(false)
        )));
        assert!(!ready(&check(Vec::new(), Some(false))));
    }

    #[test]
    fn the_advice_names_what_to_set() {
        let text = advice(&Check {
            cards: vec![card(true, Some(false), Some(true))],
            fast_startup: Some(true),
        });
        assert!(text.contains("Realtek PCIe GbE Family Controller"));
        assert!(text.contains("BIOS/UEFI"));
    }
}
