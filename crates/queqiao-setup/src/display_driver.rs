//! Queqiao's virtual display driver on this PC (drivers/queqiao-idd: Super Screen, a virtual monitor of the size the
//! viewer asks for). The install puts it in with the service: as an administrator (`--install-service`,
//! `--install-display-driver`) or as SYSTEM (`--service-update`), always from this installer's own files (payload, as
//! Windows checked them), staged in a folder of Program Files, never from a folder the person may change.
//!
//! It is signed with Queqiao's own certificate (the user's choice, 2026-11-28), whose public part the installer
//! carries and puts in the machine's Trusted Root and Trusted Publishers stores first: without Root, Windows refuses
//! the package outright (0x800B0109); without Trusted Publishers it asks the person, which a hidden administrator's
//! step or a service cannot answer (0xE0000242). Tested on Windows Server 2025, 2026-11-28.
//!
//! A failure here never fails Queqiao's install: Super Screen is then unavailable, and the report says why.
//! Uninstalling takes the device, every package of the driver, the virtual monitors' device nodes and their records
//! in Windows' display database, and the certificate away again.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use windows::Win32::Devices::DeviceAndDriverInstallation::{
    CM_DEVNODE_STATUS_FLAGS, CM_Get_DevNode_Status, CM_PROB, CR_SUCCESS, DIGCF_ALLCLASSES,
    DIGCF_PRESENT, DiUninstallDevice, HDEVINFO, INFINFO_INF_NAME_IS_ABSOLUTE, SP_DEVINFO_DATA,
    SP_INF_INFORMATION, SP_ORIGINAL_FILE_INFO_W, SPDRP_HARDWAREID, SUOI_FORCEDELETE,
    SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInfo, SetupDiGetClassDevsW,
    SetupDiGetDeviceInstanceIdW, SetupDiGetDevicePropertyW, SetupDiGetDeviceRegistryPropertyW,
    SetupGetInfInformationW, SetupQueryInfOriginalFileInformationW, SetupUninstallOEMInfW,
};
use windows::Win32::Devices::Properties::DEVPROPTYPE;
use windows::Win32::Foundation::{DEVPROPKEY, HWND};
use windows::Win32::Security::Cryptography::{
    CERT_CONTEXT, CERT_FIND_EXISTING, CERT_NAME_SIMPLE_DISPLAY_TYPE, CERT_OPEN_STORE_FLAGS,
    CERT_QUERY_ENCODING_TYPE, CERT_SHA1_HASH_PROP_ID, CERT_STORE_ADD_REPLACE_EXISTING,
    CERT_STORE_OPEN_EXISTING_FLAG, CERT_STORE_PROV_SYSTEM_W, CERT_STORE_READONLY_FLAG,
    CERT_SYSTEM_STORE_LOCAL_MACHINE, CertAddEncodedCertificateToStore, CertCloseStore,
    CertCompareCertificateName, CertCreateCertificateContext, CertDeleteCertificateFromStore,
    CertFindCertificateInStore, CertFreeCertificateContext, CertGetCertificateContextProperty,
    CertGetNameStringW, CertOpenStore, HCERTSTORE, X509_ASN_ENCODING,
};
use windows::Win32::System::Registry::{
    HKEY, HKEY_LOCAL_MACHINE, KEY_ALL_ACCESS, RegCloseKey, RegDeleteTreeW, RegEnumKeyExW,
    RegOpenKeyExW,
};
use windows::core::{GUID, PCWSTR, PWSTR};

use crate::install::program_files;
use crate::payload::Entry;

/// What the installer carries for it (scripts\package.ps1): the signed package and the certificate's public part.
pub const FILES: [&str; 4] = [
    "QueqiaoIdd.inf",
    "QueqiaoIdd.dll",
    "QueqiaoIdd.cat",
    "queqiao-signing.cer",
];
const INF: &str = FILES[0];
const CERTIFICATE: &str = FILES[3];
/// The virtual monitors' id in Windows (EDID manufacturer QQV, product 0001: drivers/queqiao-idd/driver/edid.cpp).
const MONITOR_ID: &str = "QQV0001";
/// Windows' display database: the arrangement and scale it keeps per set of monitors.
const DISPLAY_DATABASE: [&str; 3] = [
    r"SYSTEM\CurrentControlSet\Control\GraphicsDrivers\Configuration",
    r"SYSTEM\CurrentControlSet\Control\GraphicsDrivers\Connectivity",
    r"SYSTEM\CurrentControlSet\Control\GraphicsDrivers\ScaleFactors",
];
/// What the administrator's step did, in Program Files\Queqiao (only administrators may write there), for the person's
/// install to report.
const LOG: &str = "display-driver.log";
/// DEVPKEY_Device_DriverVersion.
const DRIVER_VERSION: DEVPROPKEY = DEVPROPKEY {
    fmtid: GUID::from_u128(0xa8b865dd_2e3d_4094_ad97_e593a70c75d6),
    pid: 3,
};
/// How long the device may take to start after the install.
const START: Duration = Duration::from_secs(15);

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

fn file<'a>(entries: &'a [Entry], name: &str) -> Option<&'a [u8]> {
    entries
        .iter()
        .find(|entry| entry.name.eq_ignore_ascii_case(name))
        .map(|entry| entry.bytes.as_slice())
}

/// Whether `entries` hold the whole driver (an installer made with `package.ps1 -NoDriver` does not).
pub fn carried(entries: &[Entry]) -> bool {
    FILES.iter().all(|name| file(entries, name).is_some())
}

/// The driver files from a folder (`--install-display-driver <folder>`, an administrator's own choice; the install
/// never uses this).
pub fn read_folder(folder: &Path) -> Result<Vec<Entry>, String> {
    FILES
        .iter()
        .map(|name| {
            std::fs::read(folder.join(name))
                .map(|bytes| Entry {
                    name: name.to_string(),
                    bytes,
                })
                .map_err(|e| format!("{}: {e}", folder.join(name).display()))
        })
        .collect()
}

/// The driver's version in its INF (`DriverVer = <date>,<version>`).
fn inf_version(inf: &[u8]) -> Option<String> {
    String::from_utf8_lossy(inf).lines().find_map(|line| {
        let line = line.split(';').next()?.trim();
        let (key, value) = line.split_once('=')?;
        let version = value.split(',').nth(1)?.trim();
        (key.trim().eq_ignore_ascii_case("DriverVer") && !version.is_empty())
            .then(|| version.to_string())
    })
}

// ------------------------------------------------------------------------------------------------------------------
// The device

struct DeviceSet(HDEVINFO);

impl Drop for DeviceSet {
    fn drop(&mut self) {
        let _ = unsafe { SetupDiDestroyDeviceInfoList(self.0) };
    }
}

/// Calls `found` with each device that `enumerator` ("ROOT", "DISPLAY") has, present ones only if `present`, and its
/// instance id.
fn for_each_device(
    enumerator: &str,
    present: bool,
    mut found: impl FnMut(HDEVINFO, &SP_DEVINFO_DATA, &str),
) {
    let name = wide(enumerator);
    let flags = if present {
        DIGCF_ALLCLASSES | DIGCF_PRESENT
    } else {
        DIGCF_ALLCLASSES
    };
    let Ok(set) = (unsafe { SetupDiGetClassDevsW(None, PCWSTR(name.as_ptr()), None, flags) })
    else {
        return;
    };
    let set = DeviceSet(set);
    for index in 0.. {
        let mut data = SP_DEVINFO_DATA {
            cbSize: size_of::<SP_DEVINFO_DATA>() as u32,
            ..Default::default()
        };
        if unsafe { SetupDiEnumDeviceInfo(set.0, index, &mut data) }.is_err() {
            break;
        }
        let mut id = [0u16; 512];
        if unsafe { SetupDiGetDeviceInstanceIdW(set.0, &data, Some(&mut id), None) }.is_ok() {
            let end = id.iter().position(|&c| c == 0).unwrap_or(id.len());
            found(set.0, &data, &String::from_utf16_lossy(&id[..end]));
        }
    }
}

/// Whether the device carries the driver's hardware id (Root\QueqiaoIdd).
fn is_ours(set: HDEVINFO, data: &SP_DEVINFO_DATA) -> bool {
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
        .map(|pair| u16::from_le_bytes(*pair))
        .collect();
    words
        .split(|&c| c == 0)
        .take_while(|id| !id.is_empty())
        .any(|id| String::from_utf16_lossy(id).eq_ignore_ascii_case(queqiao_vdisplay::HARDWARE_ID))
}

/// Queqiao's display device as Windows has it: its driver's version and whether it started.
struct DeviceState {
    version: String,
    started: bool,
    problem: u32,
}

fn device_state() -> Option<DeviceState> {
    let mut state = None;
    for_each_device("ROOT", true, |set, data, _| {
        if state.is_some() || !is_ours(set, data) {
            return;
        }
        let mut kind = DEVPROPTYPE::default();
        let mut buffer = [0u8; 128];
        let version = unsafe {
            SetupDiGetDevicePropertyW(
                set,
                data,
                &DRIVER_VERSION,
                &mut kind,
                Some(&mut buffer),
                None,
                0,
            )
        }
        .ok()
        .map(|()| {
            let words: Vec<u16> = buffer
                .as_chunks::<2>()
                .0
                .iter()
                .map(|pair| u16::from_le_bytes(*pair))
                .collect();
            let end = words.iter().position(|&c| c == 0).unwrap_or(words.len());
            String::from_utf16_lossy(&words[..end])
        })
        .unwrap_or_default();
        let (mut status, mut problem) = (CM_DEVNODE_STATUS_FLAGS(0), CM_PROB(0));
        let read = unsafe { CM_Get_DevNode_Status(&mut status, &mut problem, data.DevInst, 0) };
        let started = read == CR_SUCCESS && status.0 & 0x8 != 0 && problem.0 == 0; // DN_STARTED
        state = Some(DeviceState {
            version,
            started,
            problem: problem.0,
        });
    });
    state
}

/// Whether any device of the driver is there (present or not).
fn device_listed() -> bool {
    let mut listed = false;
    for_each_device("ROOT", false, |set, data, _| listed |= is_ours(set, data));
    listed
}

/// The device nodes Windows made for the virtual monitors (DISPLAY\QQV0001\...), removed.
fn remove_monitor_nodes(lines: &mut Vec<String>) {
    let prefix = format!(r"DISPLAY\{MONITOR_ID}\");
    let mut removed = 0;
    for_each_device("DISPLAY", false, |set, data, id| {
        if id.to_ascii_uppercase().starts_with(&prefix)
            && unsafe { DiUninstallDevice(HWND::default(), set, data, 0, None) }.is_ok()
        {
            removed += 1;
        }
    });
    if removed > 0 {
        lines.push(format!("{removed} virtual monitor device node(s) removed"));
    }
}

// ------------------------------------------------------------------------------------------------------------------
// The driver store

/// The driver store's names (oemNN.inf) of every package of this driver (its original INF: QueqiaoIdd.inf).
fn packages() -> Vec<String> {
    use windows::Win32::System::SystemInformation::GetSystemWindowsDirectoryW;
    let mut buffer = [0u16; 260];
    let length = unsafe { GetSystemWindowsDirectoryW(Some(&mut buffer)) } as usize;
    let folder = PathBuf::from(String::from_utf16_lossy(
        &buffer[..length.min(buffer.len())],
    ))
    .join("INF");
    let Ok(listing) = std::fs::read_dir(&folder) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for item in listing.flatten() {
        let name = item.file_name().to_string_lossy().to_string();
        let lower = name.to_ascii_lowercase();
        if !(lower.starts_with("oem") && lower.ends_with(".inf")) {
            continue;
        }
        let path = wide(&item.path().display().to_string());
        let mut size = 0u32;
        if unsafe {
            SetupGetInfInformationW(
                path.as_ptr().cast(),
                INFINFO_INF_NAME_IS_ABSOLUTE,
                None,
                0,
                Some(&mut size),
            )
        }
        .is_err()
        {
            continue;
        }
        // SP_INF_INFORMATION is followed by its data: a buffer of u64, so it is aligned.
        let mut info = vec![0u64; (size as usize).div_ceil(8).max(1)];
        let info = info.as_mut_ptr().cast::<SP_INF_INFORMATION>();
        let mut original = SP_ORIGINAL_FILE_INFO_W {
            cbSize: size_of::<SP_ORIGINAL_FILE_INFO_W>() as u32,
            ..Default::default()
        };
        let read = unsafe {
            SetupGetInfInformationW(
                path.as_ptr().cast(),
                INFINFO_INF_NAME_IS_ABSOLUTE,
                Some(info),
                size,
                None,
            )
            .and_then(|()| SetupQueryInfOriginalFileInformationW(info, 0, None, &mut original))
        };
        if read.is_ok() {
            let end = original
                .OriginalInfName
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(original.OriginalInfName.len());
            if String::from_utf16_lossy(&original.OriginalInfName[..end]).eq_ignore_ascii_case(INF)
            {
                found.push(name);
            }
        }
    }
    found
}

/// Deletes the driver's packages from the driver store: all of them (`force`), or those no device uses (an earlier
/// version after an update). How many went.
fn delete_packages(force: bool, lines: &mut Vec<String>) -> usize {
    let mut deleted = 0;
    for name in packages() {
        let wide_name = wide(&name);
        let flags = if force { SUOI_FORCEDELETE } else { 0 };
        if unsafe { SetupUninstallOEMInfW(PCWSTR(wide_name.as_ptr()), flags, None) }.as_bool() {
            lines.push(format!("driver package {name} deleted"));
            deleted += 1;
        }
    }
    deleted
}

// ------------------------------------------------------------------------------------------------------------------
// The certificate

struct Store(HCERTSTORE);

impl Drop for Store {
    fn drop(&mut self) {
        let _ = unsafe { CertCloseStore(Some(self.0), 0) };
    }
}

/// The machine's certificate store `name` ("Root", "TrustedPublisher").
fn open_store(name: &str, write: bool) -> Result<Store, String> {
    let name_w = wide(name);
    let mut flags =
        CERT_OPEN_STORE_FLAGS(CERT_SYSTEM_STORE_LOCAL_MACHINE) | CERT_STORE_OPEN_EXISTING_FLAG;
    if !write {
        flags |= CERT_STORE_READONLY_FLAG;
    }
    unsafe {
        CertOpenStore(
            CERT_STORE_PROV_SYSTEM_W,
            CERT_QUERY_ENCODING_TYPE(0),
            None,
            flags,
            Some(name_w.as_ptr().cast()),
        )
    }
    .map(Store)
    .map_err(|e| format!(r"LocalMachine\{name}: {}", e.message()))
}

struct Certificate(*mut CERT_CONTEXT);

impl Drop for Certificate {
    fn drop(&mut self) {
        let _ = unsafe { CertFreeCertificateContext(Some(self.0)) };
    }
}

impl Certificate {
    fn parse(der: &[u8]) -> Result<Certificate, String> {
        let context = unsafe { CertCreateCertificateContext(X509_ASN_ENCODING, der) };
        if context.is_null() {
            return Err(format!("{CERTIFICATE} is not a certificate"));
        }
        Ok(Certificate(context))
    }

    /// Self-signed (ours today): it must be a trusted root. One a public authority issued chains to a root Windows
    /// has: Trusted Publishers is enough then.
    fn self_signed(&self) -> bool {
        unsafe {
            let info = &*(*self.0).pCertInfo;
            CertCompareCertificateName(X509_ASN_ENCODING, &info.Subject, &info.Issuer).as_bool()
        }
    }

    /// The stores it goes in.
    fn stores(&self) -> &'static [&'static str] {
        if self.self_signed() {
            &["Root", "TrustedPublisher"]
        } else {
            &["TrustedPublisher"]
        }
    }

    /// "CN=..., thumbprint <SHA-1>", as certificate tools show it.
    fn describe(&self) -> String {
        let mut name = [0u16; 256];
        let length = unsafe {
            CertGetNameStringW(
                self.0,
                CERT_NAME_SIMPLE_DISPLAY_TYPE,
                0,
                None,
                Some(&mut name),
            )
        } as usize;
        let name = String::from_utf16_lossy(&name[..length.saturating_sub(1).min(name.len())]);
        let mut hash = [0u8; 20];
        let mut size = hash.len() as u32;
        let thumbprint = unsafe {
            CertGetCertificateContextProperty(
                self.0,
                CERT_SHA1_HASH_PROP_ID,
                Some(hash.as_mut_ptr().cast()),
                &mut size,
            )
        }
        .map(|()| {
            hash.iter()
                .map(|byte| format!("{byte:02X}"))
                .collect::<String>()
        })
        .unwrap_or_default();
        format!("\"{name}\" (thumbprint {thumbprint})")
    }

    /// Its copy in `store`, if there (to be freed, or deleted).
    fn find_in(&self, store: &Store) -> Option<Certificate> {
        let found = unsafe {
            CertFindCertificateInStore(
                store.0,
                X509_ASN_ENCODING,
                0,
                CERT_FIND_EXISTING,
                Some(self.0.cast_const().cast()),
                None,
            )
        };
        (!found.is_null()).then_some(Certificate(found))
    }
}

/// Whether the certificate is in every store it must be in (no administrator needed).
fn trusted(der: &[u8]) -> bool {
    let Ok(certificate) = Certificate::parse(der) else {
        return false;
    };
    certificate.stores().iter().all(|name| {
        open_store(name, false).is_ok_and(|store| certificate.find_in(&store).is_some())
    })
}

/// Puts the certificate in the machine's stores it must be in.
fn trust(der: &[u8], lines: &mut Vec<String>) -> Result<(), String> {
    let certificate = Certificate::parse(der)?;
    for name in certificate.stores() {
        let store = open_store(name, true)?;
        if certificate.find_in(&store).is_some() {
            continue;
        }
        unsafe {
            CertAddEncodedCertificateToStore(
                Some(store.0),
                X509_ASN_ENCODING,
                der,
                CERT_STORE_ADD_REPLACE_EXISTING,
                None,
            )
        }
        .map_err(|e| {
            format!(
                r"adding the certificate to LocalMachine\{name}: {}",
                e.message()
            )
        })?;
        lines.push(format!(
            r"certificate {} added to LocalMachine\{name}",
            certificate.describe()
        ));
    }
    Ok(())
}

/// Takes the certificate out of both machine stores (wherever it is).
fn distrust(der: &[u8], lines: &mut Vec<String>) -> Result<(), String> {
    let certificate = Certificate::parse(der)?;
    for name in ["Root", "TrustedPublisher"] {
        let store = open_store(name, true)?;
        while let Some(found) = certificate.find_in(&store) {
            // Deleting frees the found copy's context.
            let found = std::mem::ManuallyDrop::new(found);
            unsafe { CertDeleteCertificateFromStore(found.0) }
                .map_err(|e| format!(r"LocalMachine\{name}: {}", e.message()))?;
            lines.push(format!(
                r"certificate {} taken out of LocalMachine\{name}",
                certificate.describe()
            ));
        }
    }
    Ok(())
}

// ------------------------------------------------------------------------------------------------------------------
// Windows' display database

/// The display database's records of every set of monitors that held a virtual monitor (they are Windows' own; with
/// the driver gone they only take room). How many went.
fn remove_display_records(lines: &mut Vec<String>) -> usize {
    let mut removed = 0;
    for key in DISPLAY_DATABASE {
        let key_w = wide(key);
        let mut handle = HKEY::default();
        if unsafe {
            RegOpenKeyExW(
                HKEY_LOCAL_MACHINE,
                PCWSTR(key_w.as_ptr()),
                None,
                KEY_ALL_ACCESS,
                &mut handle,
            )
        }
        .is_err()
        {
            continue;
        }
        let mut names = Vec::new();
        for index in 0.. {
            let mut name = [0u16; 512];
            let mut length = name.len() as u32;
            let read = unsafe {
                RegEnumKeyExW(
                    handle,
                    index,
                    Some(PWSTR(name.as_mut_ptr())),
                    &mut length,
                    None,
                    None,
                    None,
                    None,
                )
            };
            if read.is_err() {
                break;
            }
            let name = String::from_utf16_lossy(&name[..length as usize]);
            if name.to_ascii_uppercase().contains(MONITOR_ID) {
                names.push(name);
            }
        }
        for name in names {
            let name_w = wide(&name);
            if unsafe { RegDeleteTreeW(handle, PCWSTR(name_w.as_ptr())) }.is_ok() {
                removed += 1;
            }
        }
        let _ = unsafe { RegCloseKey(handle) };
    }
    if removed > 0 {
        lines.push(format!(
            "{removed} record(s) of the virtual monitors removed from the display database"
        ));
    }
    removed
}

// ------------------------------------------------------------------------------------------------------------------
// Install and removal

/// Whether this installer's driver is in place (no administrator needed): its certificate trusted, the device started
/// with this driver version.
pub fn is_current(entries: &[Entry]) -> bool {
    let (Some(inf), Some(der)) = (file(entries, INF), file(entries, CERTIFICATE)) else {
        return false;
    };
    trusted(der)
        && inf_version(inf).is_some_and(|version| {
            device_state().is_some_and(|state| state.started && state.version == version)
        })
}

/// Whether any of it is on this PC (the device, a package, or the certificate `entries` carry): an uninstall removes
/// it then. No administrator needed.
pub fn present(entries: &[Entry]) -> bool {
    device_listed()
        || !packages().is_empty()
        || file(entries, CERTIFICATE).is_some_and(|der| {
            Certificate::parse(der).is_ok_and(|certificate| {
                ["Root", "TrustedPublisher"].iter().any(|name| {
                    open_store(name, false).is_ok_and(|store| certificate.find_in(&store).is_some())
                })
            })
        })
}

/// A new, empty folder `Program Files\Queqiao.driver` (only administrators may make one there) with the package, for
/// Windows to copy into its driver store.
fn stage(entries: &[Entry]) -> Result<PathBuf, String> {
    use std::io::Write;
    let staging = program_files().with_file_name("Queqiao.driver");
    if let Ok(meta) = std::fs::symlink_metadata(&staging) {
        let removed = if meta.is_dir() && !meta.file_type().is_symlink() {
            std::fs::remove_dir_all(&staging)
        } else if meta.is_dir() {
            std::fs::remove_dir(&staging)
        } else {
            std::fs::remove_file(&staging)
        };
        removed.map_err(|e| format!("{}: {e}", staging.display()))?;
    }
    std::fs::create_dir(&staging).map_err(|e| format!("{}: {e}", staging.display()))?;
    for name in &FILES[..3] {
        let bytes = file(entries, name).ok_or_else(|| format!("{name} is missing"))?;
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(staging.join(name))
            .and_then(|mut out| out.write_all(bytes))
            .map_err(|e| format!("{name}: {e}"))?;
    }
    Ok(staging)
}

/// As an administrator or SYSTEM: the certificate trusted, the driver installed on Queqiao's device (made if missing)
/// or updated when its version changed, earlier packages of it deleted, and the device started.
pub fn install(entries: &[Entry], lines: &mut Vec<String>) -> Result<(), String> {
    if !carried(entries) {
        return Err("this installer carries no display driver".into());
    }
    let inf = file(entries, INF).unwrap_or_default();
    let version = inf_version(inf).ok_or("QueqiaoIdd.inf has no DriverVer")?;
    trust(file(entries, CERTIFICATE).unwrap_or_default(), lines)?;
    match device_state() {
        Some(state) if state.started && state.version == version => {
            lines.push(format!("{version} is installed already"));
            return Ok(());
        }
        Some(state) => lines.push(format!(
            "{} on the device ({}): installing {version}",
            if state.version.is_empty() {
                "?"
            } else {
                &state.version
            },
            if state.started {
                "started".to_string()
            } else {
                format!("not started, problem {}", state.problem)
            }
        )),
        None => {}
    }
    let staging = stage(entries)?;
    let started = Instant::now();
    let installed = queqiao_vdisplay::install::ensure_installed(&staging.join(INF));
    let _ = std::fs::remove_dir_all(&staging);
    let reboot = installed.map_err(|e| format!("installing the driver: {}", e.message()))?;
    if reboot {
        lines.push("Windows asks for a restart to finish the display driver".into());
    }
    // Started: its interface is there.
    let deadline = Instant::now() + START;
    while !matches!(queqiao_vdisplay::interface_path(), Ok(Some(_))) {
        if Instant::now() >= deadline {
            let problem = device_state().map_or("no device".into(), |state| {
                format!("problem {}", state.problem)
            });
            return Err(format!(
                "the display driver {version} is installed but its device did not start ({problem})"
            ));
        }
        std::thread::sleep(Duration::from_millis(300));
    }
    // Earlier versions' packages: no device uses them any more (the one in use is never deleted without force).
    delete_packages(false, lines);
    lines.push(format!(
        "{version} installed and started in {:.1} s (Super Screen available)",
        started.elapsed().as_secs_f64()
    ));
    Ok(())
}

/// [`install`], with what happened also written to Program Files\Queqiao\display-driver.log for the person's install
/// to report ([`report_lines`]). The caller decides whether a failure matters: for Queqiao's install it does not.
pub fn install_reported(entries: &[Entry], lines: &mut Vec<String>) -> Result<(), String> {
    let mut own = Vec::new();
    let result = install(entries, &mut own);
    if let Err(error) = &result {
        own.push(format!("Super Screen is unavailable: {error}"));
    }
    let folder = program_files();
    if folder.is_dir() {
        let _ = std::fs::write(folder.join(LOG), own.join("\r\n") + "\r\n");
    }
    lines.extend(
        own.into_iter()
            .map(|line| format!("display driver: {line}")),
    );
    result
}

/// What the administrator's step wrote ([`install_reported`]), for the person's install report.
pub fn report_lines() -> Vec<String> {
    std::fs::read_to_string(program_files().join(LOG))
        .map(|text| {
            text.lines()
                .filter(|line| !line.trim().is_empty())
                .map(|line| format!("display driver: {line}"))
                .collect()
        })
        .unwrap_or_default()
}

/// As an administrator: the device(s), every package of the driver, the virtual monitors' device nodes and display
/// database records, and the certificate `entries` carry out of both machine stores. Goes on past a step that fails;
/// the error says what is left.
pub fn remove(entries: &[Entry], lines: &mut Vec<String>) -> Result<(), String> {
    let mut left = Vec::new();
    if device_listed() {
        if let Err(error) = queqiao_vdisplay::install::uninstall() {
            left.push(format!("the device ({})", error.message()));
        } else if device_listed() {
            left.push("the device".to_string());
        } else {
            lines.push("display device removed".into());
        }
    }
    remove_monitor_nodes(lines);
    delete_packages(true, lines);
    if !packages().is_empty() {
        left.push(format!("driver packages {}", packages().join(", ")));
    }
    remove_display_records(lines);
    match file(entries, CERTIFICATE) {
        Some(der) => {
            if let Err(error) = distrust(der, lines) {
                left.push(format!("the certificate ({error})"));
            }
        }
        None => lines.push("no certificate carried: the machine's stores left as they are".into()),
    }
    // What the administrator's step reported when it installed the driver.
    let _ = std::fs::remove_file(program_files().join(LOG));
    if left.is_empty() {
        lines.push("display driver removed".into());
        Ok(())
    } else {
        Err(format!("still on this PC: {}", left.join("; ")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_version_comes_from_driver_ver() {
        let inf = b"[Version]\r\nSignature = \"$Windows NT$\"\r\nDriverVer   = 11/28/2026,0.1.1.0 ; date, version\r\n";
        assert_eq!(inf_version(inf).as_deref(), Some("0.1.1.0"));
        assert_eq!(
            inf_version(b"[Version]\r\n; DriverVer = 1/1/2026,9.9.9.9\r\n"),
            None
        );
        assert_eq!(
            inf_version(b"driverver=01/01/2026,1.2.3.4"),
            Some("1.2.3.4".into())
        );
        // The driver in this repository.
        let ours = include_bytes!("../../../drivers/queqiao-idd/driver/QueqiaoIdd.inf");
        assert!(inf_version(ours).is_some_and(|version| version.split('.').count() == 4));
    }

    #[test]
    fn its_files_go_in_an_installer() {
        for name in FILES {
            assert!(crate::payload::valid_name(name), "{name}");
        }
        let entry = |name: &str| Entry {
            name: name.into(),
            bytes: vec![1],
        };
        let all: Vec<Entry> = FILES.iter().map(|name| entry(name)).collect();
        assert!(carried(&all));
        assert!(!carried(&all[..3]));
        // The device and the person's own folder are not touched by these checks.
        assert!(!is_current(&all[..3]));
    }
}
