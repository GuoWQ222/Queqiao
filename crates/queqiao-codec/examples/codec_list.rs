//! Lists the Media Foundation encoders and decoders on this PC for H.264, H.265 (HEVC) and AV1, hardware and software:
//! what a codec choice beyond H.264 can count on here. Runs outside the desktop session (over SSH) too.
//! Usage: codec_list

use std::ptr::null_mut;

use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoTaskMemFree};
use windows::core::{GUID, PWSTR};

fn names(category: GUID, input: Option<GUID>, output: Option<GUID>, hardware: bool) -> Vec<String> {
    let info = |subtype: GUID| MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: subtype,
    };
    let (input, output) = (input.map(info), output.map(info));
    let flags = if hardware {
        MFT_ENUM_FLAG(MFT_ENUM_FLAG_HARDWARE.0 | MFT_ENUM_FLAG_SORTANDFILTER.0)
    } else {
        MFT_ENUM_FLAG(
            MFT_ENUM_FLAG_SYNCMFT.0 | MFT_ENUM_FLAG_ASYNCMFT.0 | MFT_ENUM_FLAG_SORTANDFILTER.0,
        )
    };
    let mut list: *mut Option<IMFActivate> = null_mut();
    let mut count = 0;
    let result = unsafe {
        MFTEnumEx(
            category,
            flags,
            input.as_ref().map(|i| i as *const _),
            output.as_ref().map(|o| o as *const _),
            &mut list,
            &mut count,
        )
    };
    let mut found = Vec::new();
    if result.is_ok() && !list.is_null() {
        let slots = unsafe { std::slice::from_raw_parts_mut(list, count as usize) };
        for slot in slots.iter_mut() {
            if let Some(activate) = slot.take() {
                let mut text = PWSTR::null();
                let mut length = 0;
                let name = if unsafe {
                    activate.GetAllocatedString(
                        &MFT_FRIENDLY_NAME_Attribute,
                        &mut text,
                        &mut length,
                    )
                }
                .is_ok()
                {
                    let name = unsafe { text.to_string() }.unwrap_or_default();
                    unsafe { CoTaskMemFree(Some(text.0 as *const _)) };
                    name
                } else {
                    "unnamed".into()
                };
                found.push(name);
            }
        }
        unsafe { CoTaskMemFree(Some(list as *const _)) };
    }
    found
}

fn main() {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let _ = MFStartup(MF_VERSION, MFSTARTUP_FULL);
    }
    let codecs = [
        ("H.264", MFVideoFormat_H264),
        ("H.265", MFVideoFormat_HEVC),
        ("AV1", MFVideoFormat_AV1),
    ];
    for (name, subtype) in codecs {
        for hardware in [true, false] {
            let kind = if hardware { "hardware" } else { "software" };
            let encoders = names(MFT_CATEGORY_VIDEO_ENCODER, None, Some(subtype), hardware);
            let decoders = names(MFT_CATEGORY_VIDEO_DECODER, Some(subtype), None, hardware);
            println!(
                "{name} {kind} encoders: {}",
                if encoders.is_empty() {
                    "none".into()
                } else {
                    encoders.join("; ")
                }
            );
            println!(
                "{name} {kind} decoders: {}",
                if decoders.is_empty() {
                    "none".into()
                } else {
                    decoders.join("; ")
                }
            );
        }
    }
}
