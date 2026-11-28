// Queqiao virtual display driver ("Super Screen"): a UMDF 2 indirect display driver (IddCx 1.2, Windows 10 1803 and
// later) whose monitors are added, changed and removed by Queqiao through the IOCTLs of queqiao_idd_ioctl.h.
// Written from Microsoft's IddCx documentation and headers (see REPORT.md).
//
// Copyright (c) 2026 GuoWQ222. Part of Queqiao; see the LICENSE file of the Queqiao repository.
#pragma once

#define NOMINMAX
// windows.h without its few STATUS_* codes: wudfwdm.h brings the full ntstatus.h.
#define WIN32_NO_STATUS
#include <windows.h>
#undef WIN32_NO_STATUS
#pragma warning(push)
#pragma warning(disable : 4201 4324 4471)  // in the WDF headers
#include <bugcodes.h>
#include <wudfwdm.h>
#include <wdf.h>
#include <iddcx.h>
#pragma warning(pop)

#include <dxgi1_5.h>
#include <d3d11_2.h>
#include <avrt.h>

#include <vector>

#include "../include/queqiao_idd_ioctl.h"

#define QQIDD_DRIVER_VERSION 0x00010001u  // 0.1.1, as DriverVer in the INF

namespace qqidd {

// ---- log.cpp-like ring buffer (driver.cpp) ----
void Log(_Printf_format_string_ const wchar_t* format, ...);
size_t CopyLog(wchar_t* out, size_t outChars);

// ---- edid.cpp ----
struct Timing {
    UINT32 width, height, refresh;
    UINT32 hTotal, vTotal;
    UINT32 hFront, hSync, vFront, vSync;
    UINT64 pixelClockHz;
};
Timing MakeTiming(const QQIDD_MODE& mode);
// The modes a monitor offers: the preferred one first, then the extra ones, then the common ones (deduplicated).
std::vector<QQIDD_MODE> BuildModeList(const QQIDD_MONITOR_CONFIG& config);
// 128-byte EDID 1.4 with the preferred mode as detailed timing, the physical size, and the monitor id as serial.
void BuildEdid(const QQIDD_MONITOR_CONFIG& config, BYTE (&edid)[128]);
bool EdidMonitorId(const void* data, UINT size, UINT32* monitorId);
bool ModeIsValid(const QQIDD_MODE& mode);
void FillMonitorMode(const QQIDD_MODE& mode, IDDCX_MONITOR_MODE* out);
void FillTargetMode(const QQIDD_MODE& mode, IDDCX_TARGET_MODE* out);
GUID ContainerIdFor(UINT32 monitorId);

// ---- frames.cpp ----
// Consumes the frames of one swap chain on its own thread (Queqiao captures the virtual monitor with DXGI Desktop
// Duplication, so the frames themselves are only released back to the OS).
class FrameConsumer {
public:
    static FrameConsumer* Start(IDDCX_SWAPCHAIN swapChain, LUID renderAdapter, HANDLE frameEvent,
                                volatile LONG64* frameCounter, UINT32 monitorId);
    // Stops the thread and waits for it; deletes the object. deleteSwapChain: also delete the IddCx swap-chain object
    // (the documented way to hand it back; false when the OS already destroyed it with its monitor).
    void StopAndDelete(bool deleteSwapChain = true);

private:
    FrameConsumer() = default;
    static DWORD WINAPI ThreadMain(void* self);
    void Run();
    bool Consume(IDXGIDevice* device);

    IDDCX_SWAPCHAIN swapChain_ = nullptr;
    LUID renderAdapter_ = {};
    HANDLE frameEvent_ = nullptr;
    HANDLE stopEvent_ = nullptr;
    HANDLE thread_ = nullptr;
    volatile LONG64* frameCounter_ = nullptr;
    volatile LONG deleteSwapChain_ = 1;
    UINT32 monitorId_ = 0;
};

// ---- driver.cpp ----
struct MonitorSlot {
    bool used = false;
    bool arrived = false;
    bool orphaned = false;            // its handle was closed: the orphan worker removes it
    UINT32 id = 0;
    UINT32 connector = 0;
    UINT32 flags = 0;                 // QQIDD_CONFIG_*
    WDFFILEOBJECT owner = nullptr;    // the handle that added it (nullptr with QQIDD_CONFIG_KEEP_AFTER_CLOSE)
    IDDCX_MONITOR monitor = nullptr;
    QQIDD_MONITOR_CONFIG config = {};
    std::vector<QQIDD_MODE> modes;        // monitor modes (from our EDID)
    std::vector<QQIDD_MODE> targetModes;  // target modes (a subset after UPDATE_MODES without replug)
    BYTE edid[128] = {};
    LUID osAdapter = {};
    UINT32 osTarget = 0;
    FrameConsumer* consumer = nullptr;
    volatile LONG64 frames = 0;
};

struct DeviceState {
    WDFDEVICE device = nullptr;
    IDDCX_ADAPTER adapter = nullptr;
    bool adapterReady = false;
    UINT32 iddCxVersion = 0;
    SRWLOCK dataLock = SRWLOCK_INIT;   // guards slots; never held while calling into IddCx
    SRWLOCK opLock = SRWLOCK_INIT;     // serializes add/remove/update (held across IddCx calls; callbacks never take it)
    MonitorSlot slots[QQIDD_MAX_MONITORS];
    // Monitors of a closed handle are removed on this thread, not inside EvtFileCleanup (see driver.cpp).
    HANDLE orphanEvent = nullptr;
    HANDLE stopEvent = nullptr;
    HANDLE orphanThread = nullptr;
};

}  // namespace qqidd
