//! Queqiao virtual display ("Super Screen") on Windows: installs the Queqiao indirect display driver on a
//! root-enumerated device, adds / changes / removes virtual monitors through its IOCTLs, and arranges them with the
//! display-configuration (CCD) API. Mirrors `include/queqiao_idd_ioctl.h`
//! and `tool/qqidd.cpp` in drivers/queqiao-idd (tested on Windows Server 2025, driver 0.1.1, 2026-11-28).
//!
//! Typical agent flow (all as SYSTEM or an administrator):
//! 1. `install::ensure_installed(inf)` once (the installer can do it; needs the package's certificate trusted).
//! 2. `let vd = VirtualDisplay::open()?;` keep `vd` for the whole session: when the handle closes (also when the
//!    agent crashes) the driver removes the monitors it added, and Windows returns to the user's own screens.
//! 3. `let m = vd.add_monitor(&MonitorConfig::new(1, Mode::new(2560, 1440, 60)).physical_size_mm(597, 336))?;`
//! 4. In the session being captured: `topology::gdi_name(&m)` gives `\\.\DISPLAYn` for DXGI Desktop Duplication;
//!    `topology::only(&m)` = Super Screen (the physical monitors go dark), `topology::extend()` = extra screen.
//!    The CCD calls act on the caller's session and desktop: run them from the process in the console session
//!    (on the input desktop: Default, or Winlogon at the logon screen), as the capture already does.
//! 5. Mode change within the offered list: `topology::set_mode(&name, mode)`; anything else: `vd.update_modes()`
//!    (replugs the monitor with a new EDID; look the GDI name up again afterwards).
//! 6. `drop(vd)` or `vd.remove_monitor(id)`; `topology::restore()` if the agent changed the topology while the
//!    monitor stays.
//!
//! windows = "0.62", features: Win32_Foundation, Win32_Security, Win32_Storage_FileSystem, Win32_System_IO,
//! Win32_Devices_DeviceAndDriverInstallation, Win32_Devices_Properties, Win32_Devices_Display, Win32_Graphics_Gdi.
//!
//! Copyright (c) 2026 GuoWQ222. Part of Queqiao; see the LICENSE file of the Queqiao repository.
#![cfg(windows)]
#![allow(clippy::upper_case_acronyms)]

use std::ffi::c_void;
use std::mem::size_of;

use windows::Win32::Foundation::{
    CloseHandle, ERROR_INSUFFICIENT_BUFFER, GENERIC_READ, GENERIC_WRITE, HANDLE, LUID, WIN32_ERROR,
};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_MODE, OPEN_EXISTING,
};
use windows::Win32::System::IO::DeviceIoControl;
use windows::core::{Error, GUID, HRESULT, PCWSTR, Result};

/// Hardware id of the root-enumerated device (matches the INF).
pub const HARDWARE_ID: &str = "Root\\QueqiaoIdd";
/// Device interface the driver registers: {D3E4C053-5C51-4536-86FC-BEC2E99E36F7}.
pub const INTERFACE_GUID: GUID = GUID::from_u128(0xd3e4c053_5c51_4536_86fc_bec2e99e36f7);
pub const INTERFACE_VERSION: u32 = 1;
pub const MAX_MONITORS: usize = 4;
pub const MAX_EXTRA_MODES: usize = 16;

const DEVICE_TYPE: u32 = 0xA51D;
const METHOD_BUFFERED: u32 = 0;
const FILE_READ_ACCESS: u32 = 1;
const FILE_WRITE_ACCESS: u32 = 2;
const fn ctl_code(function: u32, access: u32) -> u32 {
    (DEVICE_TYPE << 16) | (access << 14) | (function << 2) | METHOD_BUFFERED
}
pub const IOCTL_GET_INFO: u32 = ctl_code(0x800, FILE_READ_ACCESS);
pub const IOCTL_ADD_MONITOR: u32 = ctl_code(0x801, FILE_READ_ACCESS | FILE_WRITE_ACCESS);
pub const IOCTL_REMOVE_MONITOR: u32 = ctl_code(0x802, FILE_READ_ACCESS | FILE_WRITE_ACCESS);
pub const IOCTL_UPDATE_MODES: u32 = ctl_code(0x803, FILE_READ_ACCESS | FILE_WRITE_ACCESS);
pub const IOCTL_LIST_MONITORS: u32 = ctl_code(0x804, FILE_READ_ACCESS);
pub const IOCTL_GET_LOG: u32 = ctl_code(0x805, FILE_READ_ACCESS);

/// `MonitorConfig::flags`: the monitor stays after the handle that added it closes (only for tests / tools).
pub const CONFIG_KEEP_AFTER_CLOSE: u32 = 0x1;
/// `MonitorInfo::flags`
pub const INFO_KEEP_AFTER_CLOSE: u32 = 0x1;
pub const INFO_SWAPCHAIN: u32 = 0x2; // Windows gave the monitor a swap chain: it is on and part of the desktop
pub const INFO_REPLUGGED: u32 = 0x4; // update_modes unplugged and replugged the monitor

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Mode {
    pub width: u32,
    pub height: u32,
    pub refresh_hz: u32,
}

impl Mode {
    pub const fn new(width: u32, height: u32, refresh_hz: u32) -> Self {
        Self {
            width,
            height,
            refresh_hz,
        }
    }
}

/// QQIDD_MONITOR_CONFIG. Width and height 360..=7680, refresh 24..=240 Hz.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct MonitorConfig {
    size: u32,
    pub monitor_id: u32,
    pub preferred: Mode,
    /// Physical size written into the EDID; Windows derives its recommended scale from it (0 = size at 96 DPI).
    /// Measured on the test VM for 2560x1440: 344x194 mm (15.6") -> 200 %, 597x336 mm (27") -> 100 %.
    pub width_mm: u32,
    pub height_mm: u32,
    pub flags: u32,
    extra_mode_count: u32,
    extra_modes: [Mode; MAX_EXTRA_MODES],
}

impl MonitorConfig {
    /// `monitor_id` must be nonzero; it is the EDID serial number, so keep it stable per use (Windows stores mode,
    /// position and scale per monitor identity, which includes the whole EDID).
    pub fn new(monitor_id: u32, preferred: Mode) -> Self {
        Self {
            size: size_of::<Self>() as u32,
            monitor_id,
            preferred,
            width_mm: 0,
            height_mm: 0,
            flags: 0,
            extra_mode_count: 0,
            extra_modes: [Mode::default(); MAX_EXTRA_MODES],
        }
    }

    /// The controlling device's screen size: Windows then recommends the scale it would for that screen.
    pub fn physical_size_mm(mut self, width_mm: u32, height_mm: u32) -> Self {
        self.width_mm = width_mm.min(4095);
        self.height_mm = height_mm.min(4095);
        self
    }

    /// Physical size for a given pixel density (mm = pixels * 25.4 / dpi).
    pub fn dpi(self, dpi: u32) -> Self {
        let mm = |px: u32| ((px as u64 * 254 + dpi as u64 * 5) / (dpi as u64 * 10)) as u32;
        let (w, h) = (mm(self.preferred.width), mm(self.preferred.height));
        self.physical_size_mm(w, h)
    }

    /// Further modes besides the preferred one and the common ones the driver always offers
    /// (3840x2160 ... 800x600 at 60 Hz and at the preferred refresh rate).
    pub fn extra_modes(mut self, modes: &[Mode]) -> Self {
        let n = modes.len().min(MAX_EXTRA_MODES);
        self.extra_modes[..n].copy_from_slice(&modes[..n]);
        self.extra_mode_count = n as u32;
        self
    }
}

/// QQIDD_MONITOR_INFO
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct MonitorInfo {
    size: u32,
    pub monitor_id: u32,
    pub connector_index: u32,
    pub flags: u32,
    pub adapter_luid_low: u32,
    pub adapter_luid_high: i32,
    /// With the adapter LUID: the CCD target of this monitor (DISPLAYCONFIG_PATH_TARGET_INFO adapterId / id).
    pub target_id: u32,
    pub preferred: Mode,
    pub mode_count: u32,
    reserved: u32,
    pub frames_consumed: u64,
    reserved2: [u32; 2],
}

impl MonitorInfo {
    /// A monitor another process added (the Queqiao service adds them; the agent arranges them in its own session):
    /// enough of it for the display configuration (`topology`).
    pub fn located(
        monitor_id: u32,
        adapter_luid_low: u32,
        adapter_luid_high: i32,
        target_id: u32,
        preferred: Mode,
    ) -> Self {
        MonitorInfo {
            size: size_of::<MonitorInfo>() as u32,
            monitor_id,
            adapter_luid_low,
            adapter_luid_high,
            target_id,
            preferred,
            ..MonitorInfo::default()
        }
    }

    pub fn adapter_luid(&self) -> LUID {
        LUID {
            LowPart: self.adapter_luid_low,
            HighPart: self.adapter_luid_high,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
struct MonitorId {
    size: u32,
    monitor_id: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct MonitorList {
    size: u32,
    count: u32,
    monitors: [MonitorInfo; MAX_MONITORS],
}

/// QQIDD_DRIVER_INFO
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct DriverInfo {
    size: u32,
    pub interface_version: u32,
    pub driver_version: u32, // 0xMMmmpppp
    pub iddcx_version: u32,  // 0x1200 = IddCx 1.2 ... 0x1a80 on Windows Server 2025 / 11 24H2
    pub max_monitors: u32,
    pub max_extra_modes: u32,
    pub adapter_ready: u32,
    pub monitor_count: u32,
    reserved: [u32; 2],
}

const _: () = assert!(size_of::<Mode>() == 12);
const _: () = assert!(size_of::<MonitorConfig>() == 228);
const _: () = assert!(size_of::<MonitorInfo>() == 64);
const _: () = assert!(size_of::<MonitorId>() == 8);
const _: () = assert!(size_of::<MonitorList>() == 264);
const _: () = assert!(size_of::<DriverInfo>() == 40);

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn win32(code: u32) -> Result<()> {
    WIN32_ERROR(code).ok()
}

/// An open handle to the driver. Monitors added through it (without CONFIG_KEEP_AFTER_CLOSE) are removed by the
/// driver when it is dropped or the process ends. Errors from the driver come back as Win32 errors:
/// ERROR_NOT_READY (adapter still starting), ERROR_INVALID_PARAMETER, ERROR_ALREADY_EXISTS (id in use),
/// ERROR_NOT_ENOUGH_QUOTA (4 monitors), ERROR_NOT_FOUND (unknown id).
pub struct VirtualDisplay {
    handle: HANDLE,
}

unsafe impl Send for VirtualDisplay {}

impl Drop for VirtualDisplay {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.handle);
        }
    }
}

impl VirtualDisplay {
    /// Opens the driver's interface (present devices only). Fails with ERROR_NOT_FOUND when it is not installed
    /// or not started, ERROR_ACCESS_DENIED when the caller is neither SYSTEM nor an administrator.
    pub fn open() -> Result<Self> {
        let path = interface_path()?.ok_or_else(|| Error::from(WIN32_ERROR(1168).to_hresult()))?; // ERROR_NOT_FOUND
        let path = wide(&path);
        let handle = unsafe {
            CreateFileW(
                PCWSTR(path.as_ptr()),
                (GENERIC_READ | GENERIC_WRITE).0,
                FILE_SHARE_MODE(0),
                None,
                OPEN_EXISTING,
                FILE_FLAGS_AND_ATTRIBUTES(0),
                None,
            )?
        };
        let vd = Self { handle };
        let info = vd.info()?;
        if info.interface_version != INTERFACE_VERSION {
            return Err(Error::new(
                HRESULT(0x80004005u32 as i32),
                "unsupported Queqiao virtual display interface version",
            ));
        }
        Ok(vd)
    }

    fn ioctl<I, O>(&self, code: u32, input: Option<&I>, output: Option<&mut O>) -> Result<u32> {
        let mut returned = 0u32;
        unsafe {
            DeviceIoControl(
                self.handle,
                code,
                input.map(|i| i as *const I as *const c_void),
                input.map_or(0, |_| size_of::<I>() as u32),
                output.map(|o| o as *mut O as *mut c_void),
                if size_of::<O>() > 0 {
                    size_of::<O>() as u32
                } else {
                    0
                },
                Some(&mut returned),
                None,
            )?;
        }
        Ok(returned)
    }

    pub fn info(&self) -> Result<DriverInfo> {
        let mut info = DriverInfo::default();
        self.ioctl::<(), _>(IOCTL_GET_INFO, None, Some(&mut info))?;
        Ok(info)
    }

    /// Adds a monitor; Windows extends the desktop to it (when the console's display is on). Returns its CCD
    /// target. Takes a few milliseconds.
    pub fn add_monitor(&self, config: &MonitorConfig) -> Result<MonitorInfo> {
        let mut info = MonitorInfo::default();
        self.ioctl(IOCTL_ADD_MONITOR, Some(config), Some(&mut info))?;
        Ok(info)
    }

    /// New mode list / preferred mode / physical size for a monitor. Same preferred mode and size, and only modes
    /// it already offers: the list is narrowed in place. Otherwise the monitor is replugged with a new EDID
    /// (INFO_REPLUGGED; its \\.\DISPLAYn name can change, and Windows treats it as another monitor identity).
    /// To switch among offered modes use `topology::set_mode` instead (no replug).
    pub fn update_modes(&self, config: &MonitorConfig) -> Result<MonitorInfo> {
        let mut info = MonitorInfo::default();
        self.ioctl(IOCTL_UPDATE_MODES, Some(config), Some(&mut info))?;
        Ok(info)
    }

    /// Removes one monitor (any owner), or all with id 0.
    pub fn remove_monitor(&self, monitor_id: u32) -> Result<()> {
        let id = MonitorId {
            size: size_of::<MonitorId>() as u32,
            monitor_id,
        };
        self.ioctl::<_, ()>(IOCTL_REMOVE_MONITOR, Some(&id), None)?;
        Ok(())
    }

    pub fn list(&self) -> Result<Vec<MonitorInfo>> {
        let mut list = MonitorList {
            size: 0,
            count: 0,
            monitors: [MonitorInfo::default(); MAX_MONITORS],
        };
        self.ioctl::<(), _>(IOCTL_LIST_MONITORS, None, Some(&mut list))?;
        Ok(list.monitors[..(list.count as usize).min(MAX_MONITORS)].to_vec())
    }

    /// The driver's recent log lines (diagnostics).
    pub fn log(&self) -> Result<String> {
        let mut buffer = vec![0u16; 32 * 1024];
        let mut returned = 0u32;
        unsafe {
            DeviceIoControl(
                self.handle,
                IOCTL_GET_LOG,
                None,
                0,
                Some(buffer.as_mut_ptr() as *mut c_void),
                (buffer.len() * 2) as u32,
                Some(&mut returned),
                None,
            )?;
        }
        let chars = (returned as usize / 2).min(buffer.len());
        let text = &buffer[..chars];
        let end = text.iter().position(|&c| c == 0).unwrap_or(chars);
        Ok(String::from_utf16_lossy(&text[..end]))
    }
}

/// Path of the driver's interface, if a started device has it.
pub fn interface_path() -> Result<Option<String>> {
    use windows::Win32::Devices::DeviceAndDriverInstallation::{
        CM_GET_DEVICE_INTERFACE_LIST_PRESENT, CM_Get_Device_Interface_List_SizeW,
        CM_Get_Device_Interface_ListW, CR_BUFFER_SMALL, CR_SUCCESS,
    };
    loop {
        let mut len = 0u32;
        let cr = unsafe {
            CM_Get_Device_Interface_List_SizeW(
                &mut len,
                &INTERFACE_GUID,
                PCWSTR::null(),
                CM_GET_DEVICE_INTERFACE_LIST_PRESENT,
            )
        };
        if cr != CR_SUCCESS {
            return Err(Error::new(
                HRESULT(0x80004005u32 as i32),
                format!("CM_Get_Device_Interface_List_SizeW: CONFIGRET {}", cr.0),
            ));
        }
        if len <= 1 {
            return Ok(None);
        }
        let mut buffer = vec![0u16; len as usize];
        let cr = unsafe {
            CM_Get_Device_Interface_ListW(
                &INTERFACE_GUID,
                PCWSTR::null(),
                &mut buffer,
                CM_GET_DEVICE_INTERFACE_LIST_PRESENT,
            )
        };
        if cr == CR_BUFFER_SMALL {
            continue; // an interface arrived in between
        }
        if cr != CR_SUCCESS {
            return Err(Error::new(
                HRESULT(0x80004005u32 as i32),
                format!("CM_Get_Device_Interface_ListW: CONFIGRET {}", cr.0),
            ));
        }
        let end = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
        return Ok((end > 0).then(|| String::from_utf16_lossy(&buffer[..end])));
    }
}

/// Device and driver package installation (devcon-style root device + UpdateDriverForPlugAndPlayDevicesW).
/// Needs administrator rights. With a self-signed package the certificate must already be in the machine's
/// Trusted Root and Trusted Publishers stores (else: 0x800B0109 / 0xE0000242, see REPORT.md).
pub mod install {
    use super::*;
    use windows::Win32::Devices::DeviceAndDriverInstallation::{
        DICD_GENERATE_ID, DIF_REGISTERDEVICE, DIGCF_ALLCLASSES, DiUninstallDevice, HDEVINFO,
        INSTALLFLAG_FORCE, INSTALLFLAG_NONINTERACTIVE, SP_DEVINFO_DATA, SPDRP_HARDWAREID,
        SetupDiCallClassInstaller, SetupDiCreateDeviceInfoList, SetupDiCreateDeviceInfoW,
        SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInfo, SetupDiGetClassDevsW,
        SetupDiGetDevicePropertyW, SetupDiGetDeviceRegistryPropertyW, SetupDiGetINFClassW,
        SetupDiSetDeviceRegistryPropertyW, SetupUninstallOEMInfW,
        UpdateDriverForPlugAndPlayDevicesW,
    };
    use windows::Win32::Devices::Properties::DEVPROPTYPE;
    use windows::Win32::Foundation::{DEVPROPKEY, HWND};

    const MAX_CLASS_NAME_LEN: usize = 32;
    const SUOI_FORCEDELETE: u32 = 1;
    // DEVPKEY_Device_DriverInfPath
    const DRIVER_INF_PATH: DEVPROPKEY = DEVPROPKEY {
        fmtid: GUID::from_u128(0xa8b865dd_2e3d_4094_ad97_e593a70c75d6),
        pid: 5,
    };

    struct DevInfoSet(HDEVINFO);
    impl Drop for DevInfoSet {
        fn drop(&mut self) {
            unsafe {
                let _ = SetupDiDestroyDeviceInfoList(self.0);
            }
        }
    }

    fn has_our_hardware_id(set: HDEVINFO, data: &SP_DEVINFO_DATA) -> bool {
        let mut buffer = [0u8; 2048];
        if unsafe {
            SetupDiGetDeviceRegistryPropertyW(
                set,
                data,
                SPDRP_HARDWAREID,
                None,
                Some(&mut buffer[..2044]),
                None,
            )
        }
        .is_err()
        {
            return false;
        }
        let words: Vec<u16> = buffer
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_le_bytes(*c))
            .collect();
        words
            .split(|&c| c == 0)
            .take_while(|s| !s.is_empty())
            .any(|s| String::from_utf16_lossy(s).eq_ignore_ascii_case(HARDWARE_ID))
    }

    /// Calls `f` for every device (present or not) with our hardware id; returns how many there were.
    fn for_each_device(mut f: impl FnMut(HDEVINFO, &SP_DEVINFO_DATA)) -> Result<usize> {
        let root = wide("ROOT");
        let set = DevInfoSet(unsafe {
            SetupDiGetClassDevsW(None, PCWSTR(root.as_ptr()), None, DIGCF_ALLCLASSES)?
        });
        let mut count = 0;
        let mut index = 0;
        loop {
            let mut data = SP_DEVINFO_DATA {
                cbSize: size_of::<SP_DEVINFO_DATA>() as u32,
                ..Default::default()
            };
            if unsafe { SetupDiEnumDeviceInfo(set.0, index, &mut data) }.is_err() {
                break; // ERROR_NO_MORE_ITEMS
            }
            index += 1;
            if has_our_hardware_id(set.0, &data) {
                count += 1;
                f(set.0, &data);
            }
        }
        Ok(count)
    }

    /// Creates ROOT\QUEQIAOIDD\0000 if missing and installs the package `inf` on it (staging it in the driver
    /// store). Never shows UI. Returns whether Windows wants a reboot (not seen in tests).
    pub fn ensure_installed(inf: &std::path::Path) -> Result<bool> {
        let inf_w = wide(&inf.to_string_lossy());
        let mut created = false;
        if for_each_device(|_, _| {})? == 0 {
            let mut class_guid = GUID::zeroed();
            let mut class_name = [0u16; MAX_CLASS_NAME_LEN];
            unsafe {
                SetupDiGetINFClassW(
                    PCWSTR(inf_w.as_ptr()),
                    &mut class_guid,
                    &mut class_name,
                    None,
                )?
            };
            let set = DevInfoSet(unsafe { SetupDiCreateDeviceInfoList(Some(&class_guid), None)? });
            let mut data = SP_DEVINFO_DATA {
                cbSize: size_of::<SP_DEVINFO_DATA>() as u32,
                ..Default::default()
            };
            let name = wide("QueqiaoIdd");
            let description = wide("Queqiao Virtual Display");
            unsafe {
                SetupDiCreateDeviceInfoW(
                    set.0,
                    PCWSTR(name.as_ptr()),
                    &class_guid,
                    PCWSTR(description.as_ptr()),
                    None,
                    DICD_GENERATE_ID,
                    Some(&mut data),
                )?;
                // REG_MULTI_SZ: the id, then an empty string.
                let ids: Vec<u8> = HARDWARE_ID
                    .encode_utf16()
                    .chain([0u16, 0u16])
                    .flat_map(|c| c.to_le_bytes())
                    .collect();
                SetupDiSetDeviceRegistryPropertyW(set.0, &mut data, SPDRP_HARDWAREID, Some(&ids))?;
                SetupDiCallClassInstaller(DIF_REGISTERDEVICE, set.0, Some(&data))?;
            }
            created = true;
        }
        let hwid = wide(HARDWARE_ID);
        let mut reboot = windows::core::BOOL(0);
        let result = unsafe {
            UpdateDriverForPlugAndPlayDevicesW(
                None,
                PCWSTR(hwid.as_ptr()),
                PCWSTR(inf_w.as_ptr()),
                INSTALLFLAG_FORCE | INSTALLFLAG_NONINTERACTIVE,
                Some(&mut reboot),
            )
        };
        if let Err(e) = result {
            if created {
                // Do not leave a driverless device behind.
                let _ = for_each_device(|set, data| unsafe {
                    let _ = DiUninstallDevice(HWND::default(), set, data, 0, None);
                });
            }
            return Err(e);
        }
        Ok(reboot.as_bool())
    }

    /// Removes the device(s) and deletes the driver package(s) they used from the driver store.
    pub fn uninstall() -> Result<()> {
        let mut infs: Vec<Vec<u16>> = Vec::new();
        for_each_device(|set, data| unsafe {
            let mut kind = DEVPROPTYPE::default();
            let mut buffer = [0u8; 520];
            if SetupDiGetDevicePropertyW(
                set,
                data,
                &DRIVER_INF_PATH,
                &mut kind,
                Some(&mut buffer),
                None,
                0,
            )
            .is_ok()
            {
                let words: Vec<u16> = buffer
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|c| u16::from_le_bytes(*c))
                    .collect();
                let end = words.iter().position(|&c| c == 0).unwrap_or(words.len());
                if end > 0 {
                    infs.push(words[..=end.min(words.len() - 1)].to_vec());
                }
            }
            let _ = DiUninstallDevice(HWND::default(), set, data, 0, None);
        })?;
        for inf in infs {
            unsafe {
                let _ = SetupUninstallOEMInfW(PCWSTR(inf.as_ptr()), SUOI_FORCEDELETE, None);
            }
        }
        Ok(())
    }
}

/// Display configuration of the virtual monitors (CCD). Acts on the calling process's session; call from the
/// agent process in the console session, on the input desktop.
pub mod topology {
    use super::*;
    use windows::Win32::Devices::Display::{
        DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME, DISPLAYCONFIG_DEVICE_INFO_HEADER,
        DISPLAYCONFIG_DEVICE_INFO_TYPE, DISPLAYCONFIG_MODE_INFO, DISPLAYCONFIG_PATH_INFO,
        DISPLAYCONFIG_SOURCE_DEVICE_NAME, DisplayConfigGetDeviceInfo, DisplayConfigSetDeviceInfo,
        GetDisplayConfigBufferSizes, QDC_ALL_PATHS, QDC_ONLY_ACTIVE_PATHS,
        QUERY_DISPLAY_CONFIG_FLAGS, QueryDisplayConfig, SDC_ALLOW_CHANGES, SDC_APPLY,
        SDC_TOPOLOGY_EXTEND, SDC_USE_DATABASE_CURRENT, SDC_USE_SUPPLIED_DISPLAY_CONFIG,
        SetDisplayConfig,
    };
    use windows::Win32::Graphics::Gdi::{
        CDS_TYPE, ChangeDisplaySettingsExW, DEVMODEW, DISP_CHANGE_SUCCESSFUL, DM_DISPLAYFREQUENCY,
        DM_PELSHEIGHT, DM_PELSWIDTH,
    };

    const DISPLAYCONFIG_PATH_ACTIVE: u32 = 0x1; // wingdi.h
    const DISPLAYCONFIG_PATH_MODE_IDX_INVALID: u32 = 0xffff_ffff;

    fn query(
        flags: QUERY_DISPLAY_CONFIG_FLAGS,
    ) -> Result<(Vec<DISPLAYCONFIG_PATH_INFO>, Vec<DISPLAYCONFIG_MODE_INFO>)> {
        loop {
            let (mut np, mut nm) = (0u32, 0u32);
            unsafe { GetDisplayConfigBufferSizes(flags, &mut np, &mut nm) }.ok()?;
            let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); np as usize];
            let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); nm as usize];
            let e = unsafe {
                QueryDisplayConfig(
                    flags,
                    &mut np,
                    paths.as_mut_ptr(),
                    &mut nm,
                    modes.as_mut_ptr(),
                    None,
                )
            };
            if e == ERROR_INSUFFICIENT_BUFFER {
                continue; // the configuration changed in between
            }
            e.ok()?;
            paths.truncate(np as usize);
            modes.truncate(nm as usize);
            return Ok((paths, modes));
        }
    }

    fn is_ours(p: &DISPLAYCONFIG_PATH_INFO, m: &MonitorInfo) -> bool {
        let a = m.adapter_luid();
        p.targetInfo.adapterId.LowPart == a.LowPart
            && p.targetInfo.adapterId.HighPart == a.HighPart
            && p.targetInfo.id == m.target_id
    }

    /// `\\.\DISPLAYn` of the monitor when it is active in this session (for DXGI output matching, EnumDisplaySettings,
    /// ChangeDisplaySettingsEx). None while inactive.
    pub fn gdi_name(m: &MonitorInfo) -> Result<Option<String>> {
        let (paths, _) = query(QDC_ONLY_ACTIVE_PATHS)?;
        let Some(p) = paths.iter().find(|p| is_ours(p, m)) else {
            return Ok(None);
        };
        let mut source = DISPLAYCONFIG_SOURCE_DEVICE_NAME {
            header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
                r#type: DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
                size: size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32,
                adapterId: p.sourceInfo.adapterId,
                id: p.sourceInfo.id,
            },
            ..Default::default()
        };
        win32(unsafe { DisplayConfigGetDeviceInfo(&mut source.header) } as u32)?;
        let end = source
            .viewGdiDeviceName
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(source.viewGdiDeviceName.len());
        Ok(Some(String::from_utf16_lossy(
            &source.viewGdiDeviceName[..end],
        )))
    }

    /// Extends the desktop over all connected monitors (Windows' "Extend").
    pub fn extend() -> Result<()> {
        win32(unsafe { SetDisplayConfig(None, None, SDC_APPLY | SDC_TOPOLOGY_EXTEND) } as u32)
    }

    /// Super Screen: the virtual monitor becomes the only active display (the physical ones go dark). Not saved
    /// to Windows' display database, so when the monitor departs (removed, or the agent's handle closes) Windows
    /// returns to the stored configuration of the physical monitors (tested).
    pub fn only(m: &MonitorInfo) -> Result<()> {
        let (paths, _) = query(QDC_ALL_PATHS)?;
        let mut path = *paths
            .iter()
            .find(|p| is_ours(p, m) && p.targetInfo.targetAvailable.as_bool())
            .ok_or_else(|| Error::from(WIN32_ERROR(1168).to_hresult()))?;
        path.flags = DISPLAYCONFIG_PATH_ACTIVE;
        path.sourceInfo.Anonymous.modeInfoIdx = DISPLAYCONFIG_PATH_MODE_IDX_INVALID;
        path.targetInfo.Anonymous.modeInfoIdx = DISPLAYCONFIG_PATH_MODE_IDX_INVALID;
        win32(unsafe {
            SetDisplayConfig(
                Some(&[path]),
                None,
                SDC_APPLY | SDC_USE_SUPPLIED_DISPLAY_CONFIG | SDC_ALLOW_CHANGES,
            )
        } as u32)
    }

    /// Back to the configuration Windows has stored for the monitors now connected.
    pub fn restore() -> Result<()> {
        win32(unsafe { SetDisplayConfig(None, None, SDC_APPLY | SDC_USE_DATABASE_CURRENT) } as u32)
    }

    /// Switches the monitor to one of its offered modes (not persisted). About 50 ms in the test.
    pub fn set_mode(gdi_name: &str, mode: Mode) -> Result<()> {
        let name = wide(gdi_name);
        let mut dm = DEVMODEW {
            dmSize: size_of::<DEVMODEW>() as u16,
            ..Default::default()
        };
        dm.dmPelsWidth = mode.width;
        dm.dmPelsHeight = mode.height;
        dm.dmDisplayFrequency = mode.refresh_hz;
        dm.dmFields = DM_PELSWIDTH | DM_PELSHEIGHT | DM_DISPLAYFREQUENCY;
        let r = unsafe {
            ChangeDisplaySettingsExW(PCWSTR(name.as_ptr()), Some(&dm), None, CDS_TYPE(0), None)
        };
        if r == DISP_CHANGE_SUCCESSFUL {
            Ok(())
        } else {
            Err(Error::new(
                HRESULT(0x80004005u32 as i32),
                format!("ChangeDisplaySettingsExW: {}", r.0),
            ))
        }
    }

    // UNDOCUMENTED: DisplayConfig{Get,Set}DeviceInfo with types -3 / -4 carry the per-monitor scale as steps
    // relative to Windows' recommended scale. Every "set DPI" tool uses them; Microsoft documents no other way to
    // set one monitor's scale. Stored per user (and per monitor identity). Tested on Windows Server 2025.
    #[repr(C)]
    struct DpiGet {
        header: DISPLAYCONFIG_DEVICE_INFO_HEADER,
        min_rel: i32,
        cur_rel: i32,
        max_rel: i32,
    }
    #[repr(C)]
    struct DpiSet {
        header: DISPLAYCONFIG_DEVICE_INFO_HEADER,
        rel: i32,
    }
    const SCALES: [u32; 12] = [100, 125, 150, 175, 200, 225, 250, 300, 350, 400, 450, 500];

    /// (current %, recommended %, min %, max %) of the monitor's scale.
    pub fn scale(m: &MonitorInfo) -> Result<(u32, u32, u32, u32)> {
        let (_, get) = scale_query(m)?;
        let recommended = -get.min_rel;
        let at = |rel: i32| {
            SCALES
                .get((recommended + rel) as usize)
                .copied()
                .unwrap_or(0)
        };
        Ok((at(get.cur_rel), at(0), at(get.min_rel), at(get.max_rel)))
    }

    fn scale_query(m: &MonitorInfo) -> Result<(DISPLAYCONFIG_PATH_INFO, DpiGet)> {
        let (paths, _) = query(QDC_ONLY_ACTIVE_PATHS)?;
        let path = *paths
            .iter()
            .find(|p| is_ours(p, m))
            .ok_or_else(|| Error::from(WIN32_ERROR(1168).to_hresult()))?;
        let mut get = DpiGet {
            header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
                r#type: DISPLAYCONFIG_DEVICE_INFO_TYPE(-3),
                size: size_of::<DpiGet>() as u32,
                adapterId: path.sourceInfo.adapterId,
                id: path.sourceInfo.id,
            },
            min_rel: 0,
            cur_rel: 0,
            max_rel: 0,
        };
        win32(unsafe { DisplayConfigGetDeviceInfo(&mut get.header) } as u32)?;
        Ok((path, get))
    }

    /// Sets the monitor's scale (100, 125, ... %), clamped to what Windows allows for it; returns the scale set.
    pub fn set_scale_percent(m: &MonitorInfo, percent: u32) -> Result<u32> {
        let (path, get) = scale_query(m)?;
        let recommended = -get.min_rel;
        let index = SCALES
            .iter()
            .position(|&s| s == percent)
            .ok_or_else(|| Error::from(WIN32_ERROR(87).to_hresult()))? as i32;
        let rel = (index - recommended).clamp(get.min_rel, get.max_rel);
        let set = DpiSet {
            header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
                r#type: DISPLAYCONFIG_DEVICE_INFO_TYPE(-4),
                size: size_of::<DpiSet>() as u32,
                adapterId: path.sourceInfo.adapterId,
                id: path.sourceInfo.id,
            },
            rel,
        };
        win32(unsafe { DisplayConfigSetDeviceInfo(&set.header) } as u32)?;
        Ok(SCALES[(recommended + rel) as usize])
    }
}
