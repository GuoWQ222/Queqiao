// Shared by the programs' build scripts (`include!`): makes a Windows resource file of Queqiao's icons in OUT_DIR and
// hands it to the linker. Icon group 1 is assets\queqiao.ico, the program's icon (Explorer, the taskbar, the Start
// menu, Settings > Apps show it; its windows and tray icon load it, queqiao_display::app_icon). Icon group 2 is
// assets\magpie_spread.ico, the magpie with its wings spread that the main window's card draws. The .res file is
// written here byte by byte, so no resource compiler is needed. The icons come from assets\icon\make_icon.ps1.
//
// Also the version resource (2026-11-27, the user: "add version information first"): product, publisher, the
// program's own description (`description`: Task Manager, the firewall's "Allow access" dialog and the file's
// Properties show it), version and copyright. Windows showed the programs with an unknown publisher and no
// description, and its Defender took them for a trojan by machine learning (Trojan:Win32/Bearfoos.A!ml on sunshine);
// a program that says what it is and who made it is one of the signs such checks weigh (a signature is the other).

fn embed_icon(description: &str) {
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() != Ok("msvc") {
        return;
    }
    let manifest = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let assets = manifest.join("..").join("..").join("assets");
    println!("cargo:rerun-if-changed={}", assets.join("icon").join("embed.rs").display());

    // One resource: its header (type and name as numbers), its data, padded to four bytes.
    fn resource(out: &mut Vec<u8>, kind: u16, id: u16, flags: u16, data: &[u8]) {
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&32u32.to_le_bytes());
        out.extend_from_slice(&0xFFFFu16.to_le_bytes());
        out.extend_from_slice(&kind.to_le_bytes());
        out.extend_from_slice(&0xFFFFu16.to_le_bytes());
        out.extend_from_slice(&id.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes()); // data version
        out.extend_from_slice(&flags.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // language neutral
        out.extend_from_slice(&0u32.to_le_bytes()); // version
        out.extend_from_slice(&0u32.to_le_bytes()); // characteristics
        out.extend_from_slice(data);
        while !out.len().is_multiple_of(4) {
            out.push(0);
        }
    }
    const RT_ICON: u16 = 3;
    const RT_GROUP_ICON: u16 = 14;
    let mut res = Vec::new();
    // A .res file starts with an empty resource.
    resource(&mut res, 0, 0, 0, &[]);
    // The images of all groups are numbered one after another.
    let mut next_image: u16 = 1;
    for (file, group_id) in [("queqiao.ico", 1u16), ("magpie_spread.ico", 2u16)] {
        let path = assets.join(file);
        println!("cargo:rerun-if-changed={}", path.display());
        let ico = std::fs::read(&path).unwrap_or_else(|e| panic!("assets\\{file}: {e}"));
        let u16_at = |at: usize| u16::from_le_bytes([ico[at], ico[at + 1]]);
        let u32_at = |at: usize| u32::from_le_bytes([ico[at], ico[at + 1], ico[at + 2], ico[at + 3]]);
        assert!(ico.len() >= 6 && u16_at(0) == 0 && u16_at(2) == 1, "assets\\{file} is not an icon file");
        let count = usize::from(u16_at(4));
        assert!(count > 0 && ico.len() >= 6 + 16 * count, "assets\\{file} has no images");
        let mut group = Vec::new();
        group.extend_from_slice(&0u16.to_le_bytes());
        group.extend_from_slice(&1u16.to_le_bytes());
        group.extend_from_slice(&(count as u16).to_le_bytes());
        for index in 0..count {
            let entry = 6 + 16 * index;
            let size = u32_at(entry + 8) as usize;
            let offset = u32_at(entry + 12) as usize;
            assert!(offset + size <= ico.len(), "assets\\{file} is cut short");
            let id = next_image;
            next_image += 1;
            resource(&mut res, RT_ICON, id, 0x1010, &ico[offset..offset + size]);
            // The group entry: width, height, colours, reserved, planes, bits, size (as in the .ico), then the id.
            group.extend_from_slice(&ico[entry..entry + 8]);
            group.extend_from_slice(&(size as u32).to_le_bytes());
            group.extend_from_slice(&id.to_le_bytes());
        }
        resource(&mut res, RT_GROUP_ICON, group_id, 0x1030, &group);
    }
    const RT_VERSION: u16 = 16;
    resource(&mut res, RT_VERSION, 1, 0x0030, &version_info(description));
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR")).join("queqiao_icon.res");
    std::fs::write(&out, res).expect("the icon resource file");
    println!("cargo:rustc-link-arg-bins={}", out.display());
}

/// The VS_VERSIONINFO block of a program: the fixed part (the version as numbers, a Windows program), the strings in
/// US English (Unicode), and the translation that names them.
fn version_info(description: &str) -> Vec<u8> {
    // The version the program reports (queqiao_core::VERSION: QUEQIAO_VERSION when set at build time).
    println!("cargo:rerun-if-env-changed=QUEQIAO_VERSION");
    let version = std::env::var("QUEQIAO_VERSION")
        .ok()
        .filter(|version| !version.is_empty())
        .unwrap_or_else(|| std::env::var("CARGO_PKG_VERSION").expect("CARGO_PKG_VERSION"));
    let mut numbers = version
        .split(|c: char| !c.is_ascii_digit())
        .filter(|part| !part.is_empty())
        .map(|part| part.parse::<u16>().unwrap_or(0));
    let mut next = || numbers.next().unwrap_or(0);
    let (major, minor, patch, build) = (next(), next(), next(), next());
    let program = std::env::var("CARGO_PKG_NAME").expect("CARGO_PKG_NAME");

    fn utf16(text: &str) -> Vec<u8> {
        text.encode_utf16().chain(Some(0)).flat_map(u16::to_le_bytes).collect()
    }
    fn align(out: &mut Vec<u8>) {
        while !out.len().is_multiple_of(4) {
            out.push(0);
        }
    }
    // One node: its length (filled in at the end, without padding after it), the length of its value (words for text,
    // bytes otherwise), its type (1 text, 0 binary), its key, then its value and its children, each on four bytes.
    fn node(key: &str, value: &[u8], value_length: u16, text: bool, children: &[Vec<u8>]) -> Vec<u8> {
        let mut out = vec![0, 0];
        out.extend_from_slice(&value_length.to_le_bytes());
        out.extend_from_slice(&u16::from(text).to_le_bytes());
        out.extend_from_slice(&utf16(key));
        align(&mut out);
        out.extend_from_slice(value);
        for child in children {
            align(&mut out);
            out.extend_from_slice(child);
        }
        let length = out.len() as u16;
        out[..2].copy_from_slice(&length.to_le_bytes());
        out
    }
    let string = |key: &str, value: &str| {
        let words = value.encode_utf16().count() as u16 + 1;
        node(key, &utf16(value), words, true, &[])
    };

    let ms = (u32::from(major) << 16) | u32::from(minor);
    let ls = (u32::from(patch) << 16) | u32::from(build);
    let mut fixed = Vec::new();
    for word in [
        0xFEEF_04BDu32, // signature
        0x0001_0000,    // structure version
        ms,             // file version
        ls,
        ms, // product version
        ls,
        0x3F,      // the flags that mean something
        0,         // flags: a release (no debug, pre-release or private build)
        0x0004_0004, // VOS_NT_WINDOWS32
        1,         // VFT_APP
        0,         // subtype
        0,         // date
        0,
    ] {
        fixed.extend_from_slice(&word.to_le_bytes());
    }
    let shown = format!("{major}.{minor}.{patch}");
    let copyright = "Copyright (c) 2026 GuoWQ222. All rights reserved.";
    let file = format!("{program}.exe");
    let strings = [
        string("CompanyName", "Queqiao"),
        string("FileDescription", description),
        string("FileVersion", &shown),
        string("InternalName", &program),
        string("LegalCopyright", copyright),
        string("OriginalFilename", &file),
        string("ProductName", "Queqiao"),
        string("ProductVersion", &shown),
    ];
    let table = node("040904B0", &[], 0, true, &strings);
    let string_file_info = node("StringFileInfo", &[], 0, true, &[table]);
    // US English, Unicode.
    let translation = [0x09u8, 0x04, 0xB0, 0x04];
    let var = node("Translation", &translation, 4, false, &[]);
    let var_file_info = node("VarFileInfo", &[], 0, true, &[var]);
    node(
        "VS_VERSION_INFO",
        &fixed,
        fixed.len() as u16,
        false,
        &[string_file_info, var_file_info],
    )
}
