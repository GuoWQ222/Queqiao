// Queqiao virtual display driver ("Super Screen"): the control interface between the driver and the programs that
// drive it (the test tool qqidd.exe, later the Rust agent). Plain C, fixed-size little-endian fields, no pointers,
// so the Rust side can mirror it with #[repr(C)] structs.
//
// Copyright (c) 2026 GuoWQ222. Part of Queqiao; see the LICENSE file of the Queqiao repository.
//
// How a program talks to the driver:
//   1. Find the device interface QQIDD_INTERFACE_GUID (CM_Get_Device_Interface_ListW, present devices only).
//   2. CreateFileW(path, GENERIC_READ | GENERIC_WRITE, 0, NULL, OPEN_EXISTING, 0, NULL). The device's security
//      descriptor (set by the INF) admits only SYSTEM and the Administrators group.
//   3. DeviceIoControl with the IOCTLs below (all METHOD_BUFFERED; every in/out struct starts with its Size).
// Monitors added through a handle are removed by the driver when that handle is closed (the process exits or crashes),
// unless QQIDD_CONFIG_KEEP_AFTER_CLOSE was given: a crashed agent never leaves a virtual screen behind.
#pragma once

#ifdef _MSC_VER
#include <winioctl.h>
#endif

// {D3E4C053-5C51-4536-86FC-BEC2E99E36F7}: device interface the driver registers.
#define QQIDD_INTERFACE_GUID_INIT { 0xd3e4c053, 0x5c51, 0x4536, { 0x86, 0xfc, 0xbe, 0xc2, 0xe9, 0x9e, 0x36, 0xf7 } }
// Hardware id of the root-enumerated device (INF and installer).
#define QQIDD_HARDWARE_ID L"Root\\QueqiaoIdd"

#define QQIDD_INTERFACE_VERSION 1u
#define QQIDD_MAX_MONITORS 4u
#define QQIDD_MAX_EXTRA_MODES 16u
#define QQIDD_MIN_SIZE 360u        // smallest width or height accepted
#define QQIDD_MAX_SIZE 7680u       // largest width or height accepted
#define QQIDD_MAX_REFRESH 240u

#define QQIDD_DEVICE_TYPE 0xA51Du  // vendor range (>= 0x8000)
#define QQIDD_CTL(fn, access) CTL_CODE(QQIDD_DEVICE_TYPE, (fn), METHOD_BUFFERED, (access))

// out: QQIDD_DRIVER_INFO
#define IOCTL_QQIDD_GET_INFO        QQIDD_CTL(0x800, FILE_READ_ACCESS)
// in: QQIDD_MONITOR_CONFIG, out: QQIDD_MONITOR_INFO (optional)
#define IOCTL_QQIDD_ADD_MONITOR     QQIDD_CTL(0x801, FILE_READ_ACCESS | FILE_WRITE_ACCESS)
// in: QQIDD_MONITOR_ID (MonitorId 0 = every monitor)
#define IOCTL_QQIDD_REMOVE_MONITOR  QQIDD_CTL(0x802, FILE_READ_ACCESS | FILE_WRITE_ACCESS)
// in: QQIDD_MONITOR_CONFIG (same MonitorId), out: QQIDD_MONITOR_INFO (optional)
#define IOCTL_QQIDD_UPDATE_MODES    QQIDD_CTL(0x803, FILE_READ_ACCESS | FILE_WRITE_ACCESS)
// out: QQIDD_MONITOR_LIST
#define IOCTL_QQIDD_LIST_MONITORS   QQIDD_CTL(0x804, FILE_READ_ACCESS)
// out: UTF-16 text, the driver's recent log lines (diagnostics), NUL-terminated
#define IOCTL_QQIDD_GET_LOG         QQIDD_CTL(0x805, FILE_READ_ACCESS)

typedef struct QQIDD_MODE {
    UINT32 Width;
    UINT32 Height;
    UINT32 RefreshHz;
} QQIDD_MODE;  // 12 bytes

// QQIDD_MONITOR_CONFIG.Flags
#define QQIDD_CONFIG_KEEP_AFTER_CLOSE 0x1u  // the monitor stays when the handle that added it is closed

typedef struct QQIDD_MONITOR_CONFIG {
    UINT32 Size;               // sizeof(QQIDD_MONITOR_CONFIG) = 228
    UINT32 MonitorId;          // caller's id, nonzero; also the EDID serial number, so it should be stable per use
    QQIDD_MODE Preferred;      // the mode Windows picks for a new monitor
    UINT32 WidthMm;            // physical size written into the EDID (Windows derives its default scale from it);
    UINT32 HeightMm;           //   0 = the size at 96 DPI (mm = pixels * 25.4 / 96)
    UINT32 Flags;              // QQIDD_CONFIG_*
    UINT32 ExtraModeCount;     // further modes to offer, besides the preferred one and the common ones
    QQIDD_MODE ExtraModes[QQIDD_MAX_EXTRA_MODES];
} QQIDD_MONITOR_CONFIG;

// QQIDD_MONITOR_INFO.Flags
#define QQIDD_INFO_KEEP_AFTER_CLOSE 0x1u    // as QQIDD_CONFIG_KEEP_AFTER_CLOSE
#define QQIDD_INFO_SWAPCHAIN        0x2u    // Windows has given the monitor a swap chain (it is part of the desktop)
#define QQIDD_INFO_REPLUGGED        0x4u    // UPDATE_MODES had to unplug and replug the monitor (new mode list)

typedef struct QQIDD_MONITOR_INFO {
    UINT32 Size;               // sizeof(QQIDD_MONITOR_INFO) = 64
    UINT32 MonitorId;
    UINT32 ConnectorIndex;
    UINT32 Flags;              // QQIDD_INFO_*
    UINT32 AdapterLuidLow;     // OS adapter LUID + target id of the monitor (IDARG_OUT_MONITORARRIVAL): match them
    INT32 AdapterLuidHigh;     //   with DISPLAYCONFIG_PATH_INFO.targetInfo.adapterId / .id from QueryDisplayConfig
    UINT32 TargetId;
    QQIDD_MODE Preferred;
    UINT32 ModeCount;          // number of modes offered to Windows
    UINT32 Reserved;
    UINT64 FramesConsumed;     // frames taken from the swap chain since the monitor arrived
    UINT32 Reserved2[2];
} QQIDD_MONITOR_INFO;

typedef struct QQIDD_MONITOR_ID {
    UINT32 Size;               // sizeof(QQIDD_MONITOR_ID) = 8
    UINT32 MonitorId;          // 0 = all monitors
} QQIDD_MONITOR_ID;

typedef struct QQIDD_MONITOR_LIST {
    UINT32 Size;               // sizeof(QQIDD_MONITOR_LIST) = 264
    UINT32 Count;
    QQIDD_MONITOR_INFO Monitors[QQIDD_MAX_MONITORS];
} QQIDD_MONITOR_LIST;

typedef struct QQIDD_DRIVER_INFO {
    UINT32 Size;               // sizeof(QQIDD_DRIVER_INFO) = 40
    UINT32 InterfaceVersion;   // QQIDD_INTERFACE_VERSION
    UINT32 DriverVersion;      // 0xMMmmpppp
    UINT32 IddCxVersion;       // IddCxGetVersion (0x1200 = 1.2 / Windows 10 1709, 0x1400 = 1.4, ...)
    UINT32 MaxMonitors;
    UINT32 MaxExtraModes;
    UINT32 AdapterReady;       // 1 when monitors can be added
    UINT32 MonitorCount;
    UINT32 Reserved[2];
} QQIDD_DRIVER_INFO;

#ifdef __cplusplus
static_assert(sizeof(QQIDD_MODE) == 12, "layout");
static_assert(sizeof(QQIDD_MONITOR_CONFIG) == 228, "layout");
static_assert(sizeof(QQIDD_MONITOR_INFO) == 64, "layout");
static_assert(sizeof(QQIDD_MONITOR_ID) == 8, "layout");
static_assert(sizeof(QQIDD_MONITOR_LIST) == 264, "layout");
static_assert(sizeof(QQIDD_DRIVER_INFO) == 40, "layout");
#endif
