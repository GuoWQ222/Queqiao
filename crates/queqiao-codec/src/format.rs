//! What a vendor encoder (NVENC, oneVPL) makes, as UU Remote offers it: H.264 or H.265, 4:2:0 or 4:4:4 chroma,
//! 8 or 10 bits. Also the texture each takes: NV12 (4:2:0), P010 (4:2:0 10-bit), or RGB for 4:4:4, which the
//! encoder turns into YUV 4:4:4 itself (the converter only scales it).

use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_NV12, DXGI_FORMAT_P010,
    DXGI_FORMAT_R10G10B10A2_UNORM,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Codec {
    H264,
    H265,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EncodeFormat {
    pub codec: Codec,
    pub chroma444: bool,
    pub ten_bit: bool,
}

impl EncodeFormat {
    pub const H264: EncodeFormat = EncodeFormat {
        codec: Codec::H264,
        chroma444: false,
        ten_bit: false,
    };

    /// The formats by name: "h264", "h264_444", "hevc", "hevc10", "hevc444", "hevc444_10". (H.264 10-bit is
    /// left out: neither NVIDIA's nor Intel's encoders make it.)
    pub fn from_name(name: &str) -> Option<EncodeFormat> {
        let format = |codec, chroma444, ten_bit| EncodeFormat {
            codec,
            chroma444,
            ten_bit,
        };
        Some(match name {
            "h264" => format(Codec::H264, false, false),
            "h264_444" => format(Codec::H264, true, false),
            "hevc" => format(Codec::H265, false, false),
            "hevc10" => format(Codec::H265, false, true),
            "hevc444" => format(Codec::H265, true, false),
            "hevc444_10" => format(Codec::H265, true, true),
            _ => return None,
        })
    }

    /// For measurements until the session negotiates the format: `QUEQIAO_ENCODE_FORMAT` (a name above), else H.264.
    pub fn from_env() -> EncodeFormat {
        std::env::var("QUEQIAO_ENCODE_FORMAT")
            .ok()
            .and_then(|name| EncodeFormat::from_name(&name))
            .unwrap_or(EncodeFormat::H264)
    }

    /// "H.265 4:4:4 10-bit", "H.264 4:2:0".
    pub fn name(self) -> String {
        format!(
            "{} {}{}",
            match self.codec {
                Codec::H264 => "H.264",
                Codec::H265 => "H.265",
            },
            if self.chroma444 { "4:4:4" } else { "4:2:0" },
            if self.ten_bit { " 10-bit" } else { "" }
        )
    }

    /// The texture format the encoder takes.
    pub fn input(self) -> DXGI_FORMAT {
        match (self.chroma444, self.ten_bit) {
            (false, false) => DXGI_FORMAT_NV12,
            (false, true) => DXGI_FORMAT_P010,
            (true, false) => DXGI_FORMAT_B8G8R8A8_UNORM,
            (true, true) => DXGI_FORMAT_R10G10B10A2_UNORM,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_name_reads_back() {
        for name in [
            "h264",
            "h264_444",
            "hevc",
            "hevc10",
            "hevc444",
            "hevc444_10",
        ] {
            assert!(EncodeFormat::from_name(name).is_some(), "{name}");
        }
        assert_eq!(
            EncodeFormat::from_name("hevc444_10")
                .map(EncodeFormat::name)
                .as_deref(),
            Some("H.265 4:4:4 10-bit")
        );
        assert_eq!(EncodeFormat::H264.name(), "H.264 4:2:0");
        assert!(EncodeFormat::from_name("h264_10").is_none());
        assert_eq!(
            EncodeFormat::from_name("hevc10").map(EncodeFormat::input),
            Some(DXGI_FORMAT_P010)
        );
    }
}
