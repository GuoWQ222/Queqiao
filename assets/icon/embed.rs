// Shared by the programs' build scripts (`include!`): makes a Windows resource file of Queqiao's icons in OUT_DIR and
// hands it to the linker. Icon group 1 is assets\queqiao.ico, the program's icon (Explorer, the taskbar, the Start
// menu, Settings > Apps show it; its windows and tray icon load it, queqiao_display::app_icon). Icon group 2 is
// assets\magpie_spread.ico, the magpie with its wings spread that the main window's card draws. The .res file is
// written here byte by byte, so no resource compiler is needed. The icons come from assets\icon\make_icon.ps1.

fn embed_icon() {
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
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR")).join("queqiao_icon.res");
    std::fs::write(&out, res).expect("the icon resource file");
    println!("cargo:rustc-link-arg-bins={}", out.display());
}
