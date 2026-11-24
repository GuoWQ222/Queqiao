//! The Wake-on-LAN check on Linux (Windows: wol_check.rs), with its API: each physical network card, whether it may
//! wake the PC (the kernel's device wakeup setting) and whether it wakes on a magic packet (ethtool's "Wake-on: g"),
//! and what to set where it is not. Fast Startup is Windows' own; on Linux it is not asked.

use std::path::Path;

use queqiao_core::text::{tr, trf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Card {
    /// The card's name ("enp3s0") and its driver ("r8169").
    pub connection: String,
    pub device: String,
    pub wired: bool,
    /// May wake the PC (/sys/class/net/<card>/device/power/wakeup); None: unknown.
    pub armed: Option<bool>,
    /// Wakes on a magic packet (ethtool's Wake-on contains g); None: ethtool does not say.
    pub magic_packet: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Check {
    pub cards: Vec<Card>,
    pub fast_startup: Option<bool>,
}

pub fn check() -> Check {
    let mut cards = Vec::new();
    if let Ok(entries) = std::fs::read_dir("/sys/class/net") {
        for entry in entries.flatten() {
            let path = entry.path();
            // Physical cards only (a device behind them).
            if !path.join("device").exists() {
                continue;
            }
            let connection = entry.file_name().to_string_lossy().into_owned();
            let device = std::fs::read_link(path.join("device/driver"))
                .ok()
                .and_then(|driver| driver.file_name().map(|n| n.to_string_lossy().into_owned()))
                .unwrap_or_default();
            cards.push(Card {
                wired: !path.join("wireless").exists() && !path.join("phy80211").exists(),
                armed: wakeup(&path),
                magic_packet: magic_packet(&connection),
                connection,
                device,
            });
        }
    }
    cards.sort_by(|a, b| a.connection.cmp(&b.connection));
    Check {
        cards,
        fast_startup: None,
    }
}

fn wakeup(card: &Path) -> Option<bool> {
    match std::fs::read_to_string(card.join("device/power/wakeup"))
        .ok()?
        .trim()
    {
        "enabled" => Some(true),
        "disabled" => Some(false),
        _ => None,
    }
}

fn magic_packet(card: &str) -> Option<bool> {
    let output = std::process::Command::new("ethtool")
        .arg(card)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    // "Wake-on: g" (the setting), not "Supports Wake-on: pumbg".
    text.lines()
        .map(str::trim)
        .find_map(|line| line.strip_prefix("Wake-on:"))
        .map(|modes| modes.contains('g'))
}

fn yes_no(value: Option<bool>) -> &'static str {
    match value {
        Some(true) => "yes",
        Some(false) => "no",
        None => "unknown",
    }
}

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
        "wake-on-lan: ready as far as Linux can tell: {}",
        if ready(check) { "yes" } else { "no" }
    ));
    lines
}

/// A wired card that may wake the PC and does not refuse magic packets.
pub fn ready(check: &Check) -> bool {
    check
        .cards
        .iter()
        .any(|card| card.wired && card.armed != Some(false) && card.magic_packet == Some(true))
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
    }
    if check.cards.is_empty() {
        out.push(tr("No physical network card was found.").into());
    }
    if !ready(check) {
        out.push(tr("To set it: turn Wake-on-LAN on in the BIOS or UEFI settings, then for the wired card: sudo ethtool -s <card> wol g (NetworkManager keeps it with: nmcli connection modify <connection> 802-3-ethernet.wake-on-lan magic).").into());
    }
    out.join("\n")
}
