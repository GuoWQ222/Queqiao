// Queqiao virtual display driver: driver entry, device and adapter, monitors, and the control IOCTLs.
//
// Flow (IddCx documentation, "Indirect display driver model"):
//   DriverEntry -> WdfDriverCreate; EvtDeviceAdd -> IddCxDeviceInitConfig, WdfDeviceCreate, IddCxDeviceInitialize,
//   our device interface; EvtDeviceD0Entry -> IddCxAdapterInitAsync; EvtIddCxAdapterInitFinished -> adapter ready.
//   IOCTL_QQIDD_ADD_MONITOR -> IddCxMonitorCreate + IddCxMonitorArrival with a generated EDID; the OS then asks for
//   the modes (EvtIddCxParseMonitorDescription, EvtIddCxMonitorQueryTargetModes) and assigns a swap chain
//   (EvtIddCxMonitorAssignSwapChain) once the monitor is part of the desktop. REMOVE -> IddCxMonitorDeparture.
//
// Copyright (c) 2026 GuoWQ222. Part of Queqiao; see the LICENSE file of the Queqiao repository.
#include "driver.h"

#include <stdarg.h>
#include <stdio.h>
#include <wchar.h>

using namespace qqidd;

struct DeviceContext {
    DeviceState* state;
};
WDF_DECLARE_CONTEXT_TYPE_WITH_NAME(DeviceContext, GetDeviceContext);

struct AdapterContext {
    DeviceState* state;
};
WDF_DECLARE_CONTEXT_TYPE_WITH_NAME(AdapterContext, GetAdapterContext);

struct MonitorContext {
    DeviceState* state;
    UINT32 slot;
    UINT32 monitorId;
};
WDF_DECLARE_CONTEXT_TYPE_WITH_NAME(MonitorContext, GetMonitorContext);

static const GUID kInterfaceGuid = QQIDD_INTERFACE_GUID_INIT;

// EvtIddCxParseMonitorDescription has no device parameter: the one device's state, found through this.
static SRWLOCK g_stateLock = SRWLOCK_INIT;
static DeviceState* g_state = nullptr;

// ------------------------------------------------------------------------------------------------------------------
// Log ring (read back with IOCTL_QQIDD_GET_LOG; also sent to OutputDebugString)

namespace qqidd {
namespace {
constexpr size_t kLogLines = 128;
constexpr size_t kLineChars = 200;
SRWLOCK g_logLock = SRWLOCK_INIT;
wchar_t g_log[kLogLines][kLineChars];
size_t g_logNext = 0;
size_t g_logCount = 0;
}  // namespace

void Log(const wchar_t* format, ...) {
    wchar_t line[kLineChars];
    const ULONGLONG ms = GetTickCount64();
    int n = _snwprintf_s(line, kLineChars, _TRUNCATE, L"[%llu.%03llu] ", ms / 1000, ms % 1000);
    if (n < 0) n = 0;
    va_list args;
    va_start(args, format);
    _vsnwprintf_s(line + n, kLineChars - n, _TRUNCATE, format, args);
    va_end(args);
    OutputDebugStringW(L"QueqiaoIdd ");
    OutputDebugStringW(line);
    OutputDebugStringW(L"\n");
    AcquireSRWLockExclusive(&g_logLock);
    wcscpy_s(g_log[g_logNext], line);
    g_logNext = (g_logNext + 1) % kLogLines;
    if (g_logCount < kLogLines) ++g_logCount;
    ReleaseSRWLockExclusive(&g_logLock);
}

size_t CopyLog(wchar_t* out, size_t outChars) {
    if (outChars == 0) return 0;
    size_t pos = 0;
    AcquireSRWLockShared(&g_logLock);
    const size_t first = (g_logNext + kLogLines - g_logCount) % kLogLines;
    for (size_t i = 0; i < g_logCount; ++i) {
        const wchar_t* text = g_log[(first + i) % kLogLines];
        const size_t len = wcslen(text);
        if (pos + len + 2 > outChars) break;
        memcpy(out + pos, text, len * sizeof(wchar_t));
        pos += len;
        out[pos++] = L'\n';
    }
    ReleaseSRWLockShared(&g_logLock);
    out[pos] = 0;
    return pos + 1;
}
}  // namespace qqidd

// ------------------------------------------------------------------------------------------------------------------
// Slots

static void ResetSlot(MonitorSlot& s) {
    s.used = false;
    s.arrived = false;
    s.orphaned = false;
    s.id = 0;
    s.connector = 0;
    s.flags = 0;
    s.owner = nullptr;
    s.monitor = nullptr;
    s.config = {};
    s.modes.clear();
    s.targetModes.clear();
    memset(s.edid, 0, sizeof(s.edid));
    s.osAdapter = {};
    s.osTarget = 0;
    s.consumer = nullptr;
    // s.frames is kept: it only counts up.
}

static int FindSlot(DeviceState* state, UINT32 id) {  // dataLock held
    for (UINT32 i = 0; i < QQIDD_MAX_MONITORS; ++i)
        if (state->slots[i].used && state->slots[i].id == id) return static_cast<int>(i);
    return -1;
}

static void FillInfo(DeviceState* state, UINT32 index, QQIDD_MONITOR_INFO* info) {  // dataLock held
    const MonitorSlot& s = state->slots[index];
    *info = {};
    info->Size = sizeof(*info);
    info->MonitorId = s.id;
    info->ConnectorIndex = s.connector;
    info->Flags = ((s.flags & QQIDD_CONFIG_KEEP_AFTER_CLOSE) ? QQIDD_INFO_KEEP_AFTER_CLOSE : 0) |
                  (s.consumer ? QQIDD_INFO_SWAPCHAIN : 0);
    info->AdapterLuidLow = s.osAdapter.LowPart;
    info->AdapterLuidHigh = s.osAdapter.HighPart;
    info->TargetId = s.osTarget;
    info->Preferred = s.config.Preferred;
    info->ModeCount = static_cast<UINT32>(s.targetModes.size());
    info->FramesConsumed = static_cast<UINT64>(InterlockedCompareExchange64(const_cast<volatile LONG64*>(&s.frames), 0, 0));
}

// Creates the IddCx monitor of a filled slot and reports its arrival. opLock held, dataLock not held.
static NTSTATUS PlugIn(DeviceState* state, UINT32 index) {
    MonitorSlot& s = state->slots[index];
    IDDCX_MONITOR_INFO info = {};
    info.Size = sizeof(info);
    info.MonitorType = DISPLAYCONFIG_OUTPUT_TECHNOLOGY_HDMI;
    info.ConnectorIndex = s.connector;
    info.MonitorDescription.Size = sizeof(info.MonitorDescription);
    info.MonitorDescription.Type = IDDCX_MONITOR_DESCRIPTION_TYPE_EDID;
    info.MonitorDescription.DataSize = sizeof(s.edid);
    info.MonitorDescription.pData = s.edid;
    info.MonitorContainerId = ContainerIdFor(s.id);

    WDF_OBJECT_ATTRIBUTES attributes;
    WDF_OBJECT_ATTRIBUTES_INIT_CONTEXT_TYPE(&attributes, MonitorContext);
    attributes.EvtCleanupCallback = [](WDFOBJECT object) {
        Log(L"monitor %u: object destroyed", GetMonitorContext(object)->monitorId);
    };
    IDARG_IN_MONITORCREATE create = {};
    create.ObjectAttributes = &attributes;
    create.pMonitorInfo = &info;
    IDARG_OUT_MONITORCREATE created = {};
    NTSTATUS status = IddCxMonitorCreate(state->adapter, &create, &created);
    if (!NT_SUCCESS(status)) {
        Log(L"monitor %u: IddCxMonitorCreate 0x%08x", s.id, status);
        return status;
    }
    MonitorContext* context = GetMonitorContext(created.MonitorObject);
    context->state = state;
    context->slot = index;
    context->monitorId = s.id;
    AcquireSRWLockExclusive(&state->dataLock);
    s.monitor = created.MonitorObject;
    ReleaseSRWLockExclusive(&state->dataLock);

    IDARG_OUT_MONITORARRIVAL arrival = {};
    status = IddCxMonitorArrival(created.MonitorObject, &arrival);
    if (!NT_SUCCESS(status)) {
        Log(L"monitor %u: IddCxMonitorArrival 0x%08x", s.id, status);
        WdfObjectDelete(reinterpret_cast<WDFOBJECT>(created.MonitorObject));
        AcquireSRWLockExclusive(&state->dataLock);
        s.monitor = nullptr;
        ReleaseSRWLockExclusive(&state->dataLock);
        return status;
    }
    AcquireSRWLockExclusive(&state->dataLock);
    s.arrived = true;
    s.osAdapter = arrival.OsAdapterLuid;
    s.osTarget = arrival.OsTargetId;
    ReleaseSRWLockExclusive(&state->dataLock);
    Log(L"monitor %u: arrived on connector %u, %ux%u@%u, %u modes, adapter %08x:%08x target %u", s.id, s.connector,
        s.config.Preferred.Width, s.config.Preferred.Height, s.config.Preferred.RefreshHz,
        static_cast<UINT32>(s.modes.size()), arrival.OsAdapterLuid.HighPart, arrival.OsAdapterLuid.LowPart,
        arrival.OsTargetId);
    return STATUS_SUCCESS;
}

// Reports the departure of a slot's monitor (the OS then destroys the monitor object). opLock held.
static void Unplug(DeviceState* state, UINT32 index) {
    MonitorSlot& s = state->slots[index];
    AcquireSRWLockShared(&state->dataLock);
    IDDCX_MONITOR monitor = s.arrived ? s.monitor : nullptr;
    const UINT32 id = s.id;
    ReleaseSRWLockShared(&state->dataLock);
    if (monitor) {
        NTSTATUS status = IddCxMonitorDeparture(monitor);
        Log(L"monitor %u: departure 0x%08x", id, status);
    }
    AcquireSRWLockExclusive(&state->dataLock);
    FrameConsumer* leftover = s.consumer;
    s.consumer = nullptr;
    s.arrived = false;
    s.monitor = nullptr;
    ReleaseSRWLockExclusive(&state->dataLock);
    if (leftover) {
        // The OS normally unassigns the swap chain during the departure; if not, stop the thread here.
        Log(L"monitor %u: swap chain still assigned after departure, stopping its thread", id);
        leftover->StopAndDelete(false);  // the swap chain went with the monitor object
    }
}

static NTSTATUS ValidateConfig(const QQIDD_MONITOR_CONFIG& c) {
    if (c.Size != sizeof(QQIDD_MONITOR_CONFIG) || c.MonitorId == 0 || !ModeIsValid(c.Preferred) ||
        c.ExtraModeCount > QQIDD_MAX_EXTRA_MODES || c.WidthMm > 4095 || c.HeightMm > 4095 ||
        (c.Flags & ~QQIDD_CONFIG_KEEP_AFTER_CLOSE) != 0)
        return STATUS_INVALID_PARAMETER;
    for (UINT32 i = 0; i < c.ExtraModeCount; ++i)
        if (!ModeIsValid(c.ExtraModes[i])) return STATUS_INVALID_PARAMETER;
    return STATUS_SUCCESS;
}

static NTSTATUS AddMonitor(DeviceState* state, const QQIDD_MONITOR_CONFIG& config, WDFFILEOBJECT file,
                           QQIDD_MONITOR_INFO* out) {
    NTSTATUS status = ValidateConfig(config);
    if (!NT_SUCCESS(status)) return status;
    AcquireSRWLockExclusive(&state->opLock);
    AcquireSRWLockExclusive(&state->dataLock);
    int index = -1;
    if (!state->adapterReady) {
        status = STATUS_DEVICE_NOT_READY;
    } else if (FindSlot(state, config.MonitorId) >= 0) {
        status = STATUS_OBJECT_NAME_COLLISION;
    } else {
        for (UINT32 i = 0; i < QQIDD_MAX_MONITORS; ++i)
            if (!state->slots[i].used) {
                index = static_cast<int>(i);
                break;
            }
        if (index < 0) status = STATUS_QUOTA_EXCEEDED;
    }
    if (index >= 0) {
        MonitorSlot& s = state->slots[index];
        ResetSlot(s);
        s.used = true;
        s.id = config.MonitorId;
        s.connector = static_cast<UINT32>(index);
        s.flags = config.Flags;
        s.owner = (config.Flags & QQIDD_CONFIG_KEEP_AFTER_CLOSE) ? nullptr : file;
        s.config = config;
        s.modes = BuildModeList(config);
        s.targetModes = s.modes;
        BuildEdid(config, s.edid);
    }
    ReleaseSRWLockExclusive(&state->dataLock);
    if (index >= 0) {
        status = PlugIn(state, static_cast<UINT32>(index));
        AcquireSRWLockExclusive(&state->dataLock);
        if (NT_SUCCESS(status)) {
            if (out) FillInfo(state, static_cast<UINT32>(index), out);
        } else {
            ResetSlot(state->slots[index]);
        }
        ReleaseSRWLockExclusive(&state->dataLock);
    }
    ReleaseSRWLockExclusive(&state->opLock);
    return status;
}

// Removes monitor `id` (0 = all), or with orphansOnly the monitors whose handle was closed.
static NTSTATUS RemoveMonitors(DeviceState* state, UINT32 id, bool orphansOnly, UINT32* removed) {
    *removed = 0;
    AcquireSRWLockExclusive(&state->opLock);
    for (UINT32 i = 0; i < QQIDD_MAX_MONITORS; ++i) {
        AcquireSRWLockShared(&state->dataLock);
        const MonitorSlot& s = state->slots[i];
        const bool match = s.used && (orphansOnly ? s.orphaned : (id == 0 || s.id == id));
        ReleaseSRWLockShared(&state->dataLock);
        if (!match) continue;
        Unplug(state, i);
        AcquireSRWLockExclusive(&state->dataLock);
        ResetSlot(state->slots[i]);
        ReleaseSRWLockExclusive(&state->dataLock);
        ++*removed;
    }
    ReleaseSRWLockExclusive(&state->opLock);
    return (*removed || id == 0 || orphansOnly) ? STATUS_SUCCESS : STATUS_NOT_FOUND;
}

// The orphan worker: removes the monitors of closed handles. Not done inside EvtFileCleanup itself: there the
// departure can wait for IddCx work that cannot run until the cleanup callback returns (a monitor whose handle
// closes while Windows is still setting it up).
static DWORD WINAPI OrphanWorker(void* parameter) {
    DeviceState* state = static_cast<DeviceState*>(parameter);
    HANDLE waits[2] = {state->stopEvent, state->orphanEvent};
    for (;;) {
        if (WaitForMultipleObjects(2, waits, FALSE, INFINITE) != WAIT_OBJECT_0 + 1) return 0;
        UINT32 removed = 0;
        RemoveMonitors(state, 0, true, &removed);
        if (removed) Log(L"handle closed: removed %u monitor(s) it had added", removed);
    }
}

static bool ContainsMode(const std::vector<QQIDD_MODE>& modes, const QQIDD_MODE& m) {
    for (const auto& x : modes)
        if (x.Width == m.Width && x.Height == m.Height && x.RefreshHz == m.RefreshHz) return true;
    return false;
}

// New mode list for a monitor. If the preferred mode and the physical size stay and every requested mode is already
// offered by the monitor (its EDID), only the target modes change (IddCxMonitorUpdateModes, no replug). Otherwise the
// monitor is unplugged and plugged in again with a new EDID: Windows only reads a monitor's own modes when it arrives.
static NTSTATUS UpdateModes(DeviceState* state, const QQIDD_MONITOR_CONFIG& config, QQIDD_MONITOR_INFO* out) {
    NTSTATUS status = ValidateConfig(config);
    if (!NT_SUCCESS(status)) return status;
    AcquireSRWLockExclusive(&state->opLock);
    AcquireSRWLockExclusive(&state->dataLock);
    const int index = FindSlot(state, config.MonitorId);
    bool replug = false;
    std::vector<QQIDD_MODE> wanted;
    IDDCX_MONITOR monitor = nullptr;
    if (index < 0) {
        status = STATUS_NOT_FOUND;
    } else {
        MonitorSlot& s = state->slots[index];
        wanted = BuildModeList(config);
        const QQIDD_MODE& oldPreferred = s.config.Preferred;
        replug = !s.arrived || oldPreferred.Width != config.Preferred.Width ||
                 oldPreferred.Height != config.Preferred.Height || oldPreferred.RefreshHz != config.Preferred.RefreshHz ||
                 s.config.WidthMm != config.WidthMm || s.config.HeightMm != config.HeightMm;  // new EDID needed
        for (const auto& m : wanted)
            if (!ContainsMode(s.modes, m)) replug = true;
        monitor = s.monitor;
    }
    ReleaseSRWLockExclusive(&state->dataLock);

    if (index >= 0 && !replug) {
        std::vector<IDDCX_TARGET_MODE> targets(wanted.size());
        for (size_t i = 0; i < wanted.size(); ++i) FillTargetMode(wanted[i], &targets[i]);
        IDARG_IN_UPDATEMODES update = {};
        update.Reason = IDDCX_UPDATE_REASON_OTHER;
        update.TargetModeCount = static_cast<UINT>(targets.size());
        update.pTargetModes = targets.data();
        // The OS may query the target modes again inside this call: store them first.
        AcquireSRWLockExclusive(&state->dataLock);
        state->slots[index].targetModes = wanted;
        ReleaseSRWLockExclusive(&state->dataLock);
        status = IddCxMonitorUpdateModes(monitor, &update);
        Log(L"monitor %u: target modes updated (%u modes) 0x%08x", config.MonitorId, update.TargetModeCount, status);
    } else if (index >= 0) {
        Unplug(state, static_cast<UINT32>(index));
        AcquireSRWLockExclusive(&state->dataLock);
        MonitorSlot& s = state->slots[index];
        s.config = config;
        s.config.Flags = s.flags;  // ownership is fixed when the monitor is added
        s.modes = wanted;
        s.targetModes = wanted;
        BuildEdid(config, s.edid);
        ReleaseSRWLockExclusive(&state->dataLock);
        status = PlugIn(state, static_cast<UINT32>(index));
        Log(L"monitor %u: replugged with %ux%u@%u 0x%08x", config.MonitorId, config.Preferred.Width,
            config.Preferred.Height, config.Preferred.RefreshHz, status);
        if (!NT_SUCCESS(status)) {
            AcquireSRWLockExclusive(&state->dataLock);
            ResetSlot(state->slots[index]);
            ReleaseSRWLockExclusive(&state->dataLock);
        }
    }
    if (NT_SUCCESS(status) && out) {
        AcquireSRWLockShared(&state->dataLock);
        FillInfo(state, static_cast<UINT32>(index), out);
        if (replug) out->Flags |= QQIDD_INFO_REPLUGGED;
        ReleaseSRWLockShared(&state->dataLock);
    }
    ReleaseSRWLockExclusive(&state->opLock);
    return status;
}

// ------------------------------------------------------------------------------------------------------------------
// IOCTLs

template <typename T>
static NTSTATUS ReadInput(WDFREQUEST request, T* value) {
    void* buffer = nullptr;
    size_t length = 0;
    NTSTATUS status = WdfRequestRetrieveInputBuffer(request, sizeof(T), &buffer, &length);
    if (!NT_SUCCESS(status)) return STATUS_INVALID_PARAMETER;
    memcpy(value, buffer, sizeof(T));  // METHOD_BUFFERED: copy before the output overwrites the shared buffer
    return STATUS_SUCCESS;
}

template <typename T>
static NTSTATUS WriteOutput(WDFREQUEST request, const T& value, size_t* written) {
    void* buffer = nullptr;
    size_t length = 0;
    NTSTATUS status = WdfRequestRetrieveOutputBuffer(request, sizeof(T), &buffer, &length);
    if (!NT_SUCCESS(status)) return STATUS_BUFFER_TOO_SMALL;
    memcpy(buffer, &value, sizeof(T));
    *written = sizeof(T);
    return STATUS_SUCCESS;
}

static VOID EvtIoctl(WDFDEVICE device, WDFREQUEST request, size_t outputLength, size_t inputLength, ULONG code) {
    UNREFERENCED_PARAMETER(inputLength);
    DeviceState* state = GetDeviceContext(device)->state;
    NTSTATUS status = STATUS_INVALID_DEVICE_REQUEST;
    size_t written = 0;
    switch (code) {
    case IOCTL_QQIDD_GET_INFO: {
        QQIDD_DRIVER_INFO info = {};
        info.Size = sizeof(info);
        info.InterfaceVersion = QQIDD_INTERFACE_VERSION;
        info.DriverVersion = QQIDD_DRIVER_VERSION;
        info.MaxMonitors = QQIDD_MAX_MONITORS;
        info.MaxExtraModes = QQIDD_MAX_EXTRA_MODES;
        AcquireSRWLockShared(&state->dataLock);
        info.IddCxVersion = state->iddCxVersion;
        info.AdapterReady = state->adapterReady ? 1 : 0;
        for (const auto& s : state->slots)
            if (s.used) ++info.MonitorCount;
        ReleaseSRWLockShared(&state->dataLock);
        status = WriteOutput(request, info, &written);
        break;
    }
    case IOCTL_QQIDD_ADD_MONITOR:
    case IOCTL_QQIDD_UPDATE_MODES: {
        QQIDD_MONITOR_CONFIG config;
        status = ReadInput(request, &config);
        if (!NT_SUCCESS(status)) break;
        QQIDD_MONITOR_INFO info = {};
        status = code == IOCTL_QQIDD_ADD_MONITOR
                     ? AddMonitor(state, config, WdfRequestGetFileObject(request), &info)
                     : UpdateModes(state, config, &info);
        if (NT_SUCCESS(status) && outputLength >= sizeof(info)) status = WriteOutput(request, info, &written);
        break;
    }
    case IOCTL_QQIDD_REMOVE_MONITOR: {
        QQIDD_MONITOR_ID id;
        status = ReadInput(request, &id);
        if (!NT_SUCCESS(status)) break;
        if (id.Size != sizeof(id)) {
            status = STATUS_INVALID_PARAMETER;
            break;
        }
        UINT32 removed = 0;
        status = RemoveMonitors(state, id.MonitorId, false, &removed);
        Log(L"remove %u: %u monitor(s) 0x%08x", id.MonitorId, removed, status);
        break;
    }
    case IOCTL_QQIDD_LIST_MONITORS: {
        QQIDD_MONITOR_LIST list = {};
        list.Size = sizeof(list);
        AcquireSRWLockShared(&state->dataLock);
        for (UINT32 i = 0; i < QQIDD_MAX_MONITORS; ++i)
            if (state->slots[i].used) FillInfo(state, i, &list.Monitors[list.Count++]);
        ReleaseSRWLockShared(&state->dataLock);
        status = WriteOutput(request, list, &written);
        break;
    }
    case IOCTL_QQIDD_GET_LOG: {
        void* buffer = nullptr;
        size_t length = 0;
        status = WdfRequestRetrieveOutputBuffer(request, sizeof(wchar_t), &buffer, &length);
        if (!NT_SUCCESS(status)) break;
        written = CopyLog(static_cast<wchar_t*>(buffer), length / sizeof(wchar_t)) * sizeof(wchar_t);
        break;
    }
    default:
        break;
    }
    WdfRequestCompleteWithInformation(request, status, written);
}

// A handle was closed (also when its process ended): its monitors go, on the orphan worker.
static VOID EvtFileCleanup(WDFFILEOBJECT file) {
    WDFDEVICE device = WdfFileObjectGetDevice(file);
    DeviceState* state = GetDeviceContext(device)->state;
    if (!state) return;
    UINT32 count = 0;
    AcquireSRWLockExclusive(&state->dataLock);
    for (auto& s : state->slots)
        if (s.used && s.owner == file) {
            s.owner = nullptr;  // the file object goes away; the slot no longer refers to it
            s.orphaned = true;
            ++count;
        }
    ReleaseSRWLockExclusive(&state->dataLock);
    if (count) {
        Log(L"handle closed: %u monitor(s) to remove", count);
        SetEvent(state->orphanEvent);
    }
}

// ------------------------------------------------------------------------------------------------------------------
// IddCx callbacks

static NTSTATUS EvtAdapterInitFinished(IDDCX_ADAPTER adapter, const IDARG_IN_ADAPTER_INIT_FINISHED* in) {
    DeviceState* state = GetAdapterContext(adapter)->state;
    IDARG_OUT_GETVERSION version = {};
    const NTSTATUS versionStatus = IddCxGetVersion(&version);
    AcquireSRWLockExclusive(&state->dataLock);
    state->adapterReady = NT_SUCCESS(in->AdapterInitStatus);
    state->iddCxVersion = NT_SUCCESS(versionStatus) ? version.IddCxVersion : 0;
    ReleaseSRWLockExclusive(&state->dataLock);
    Log(L"adapter init finished 0x%08x, IddCx version 0x%x", in->AdapterInitStatus, version.IddCxVersion);
    return STATUS_SUCCESS;
}

static NTSTATUS EvtAdapterCommitModes(IDDCX_ADAPTER adapter, const IDARG_IN_COMMITMODES* in) {
    UNREFERENCED_PARAMETER(adapter);
    for (UINT i = 0; i < in->PathCount; ++i) {
        const IDDCX_PATH& p = in->pPaths[i];
        const UINT32 id = p.MonitorObject ? GetMonitorContext(p.MonitorObject)->monitorId : 0;
        Log(L"commit: monitor %u %ls %ux%u", id, (p.Flags & IDDCX_PATH_FLAGS_ACTIVE) ? L"active" : L"inactive",
            p.TargetVideoSignalInfo.activeSize.cx, p.TargetVideoSignalInfo.activeSize.cy);
    }
    return STATUS_SUCCESS;
}

static void CopyModesOut(const std::vector<QQIDD_MODE>& modes, IDDCX_MONITOR_MODE* out) {
    for (size_t i = 0; i < modes.size(); ++i) FillMonitorMode(modes[i], &out[i]);
}

static NTSTATUS EvtParseMonitorDescription(const IDARG_IN_PARSEMONITORDESCRIPTION* in,
                                           IDARG_OUT_PARSEMONITORDESCRIPTION* out) {
    UINT32 id = 0;
    if (in->MonitorDescription.Type != IDDCX_MONITOR_DESCRIPTION_TYPE_EDID ||
        !EdidMonitorId(in->MonitorDescription.pData, in->MonitorDescription.DataSize, &id))
        return STATUS_INVALID_PARAMETER;
    NTSTATUS status = STATUS_INVALID_PARAMETER;
    AcquireSRWLockShared(&g_stateLock);
    DeviceState* state = g_state;
    if (state) {
        AcquireSRWLockShared(&state->dataLock);
        const int index = FindSlot(state, id);
        if (index >= 0 && memcmp(state->slots[index].edid, in->MonitorDescription.pData, 128) == 0) {
            const auto& modes = state->slots[index].modes;
            out->MonitorModeBufferOutputCount = static_cast<UINT>(modes.size());
            out->PreferredMonitorModeIdx = 0;
            if (in->MonitorModeBufferInputCount == 0) {
                status = STATUS_SUCCESS;  // the OS only asked how many
            } else if (in->MonitorModeBufferInputCount < modes.size()) {
                status = STATUS_BUFFER_TOO_SMALL;
            } else {
                CopyModesOut(modes, in->pMonitorModes);
                status = STATUS_SUCCESS;
            }
        }
        ReleaseSRWLockShared(&state->dataLock);
    }
    ReleaseSRWLockShared(&g_stateLock);
    if (!NT_SUCCESS(status)) Log(L"parse description for monitor %u: 0x%08x", id, status);
    return status;
}

static NTSTATUS EvtMonitorGetDefaultModes(IDDCX_MONITOR monitor, const IDARG_IN_GETDEFAULTDESCRIPTIONMODES* in,
                                          IDARG_OUT_GETDEFAULTDESCRIPTIONMODES* out) {
    // Only for monitors without a description; ours always have an EDID, but answer with the same list.
    MonitorContext* context = GetMonitorContext(monitor);
    DeviceState* state = context->state;
    NTSTATUS status = STATUS_SUCCESS;
    AcquireSRWLockShared(&state->dataLock);
    const auto& modes = state->slots[context->slot].modes;
    out->DefaultMonitorModeBufferOutputCount = static_cast<UINT>(modes.size());
    out->PreferredMonitorModeIdx = 0;
    if (in->DefaultMonitorModeBufferInputCount != 0) {
        if (in->DefaultMonitorModeBufferInputCount < modes.size())
            status = STATUS_BUFFER_TOO_SMALL;
        else
            CopyModesOut(modes, in->pDefaultMonitorModes);
    }
    ReleaseSRWLockShared(&state->dataLock);
    return status;
}

static NTSTATUS EvtMonitorQueryTargetModes(IDDCX_MONITOR monitor, const IDARG_IN_QUERYTARGETMODES* in,
                                           IDARG_OUT_QUERYTARGETMODES* out) {
    MonitorContext* context = GetMonitorContext(monitor);
    DeviceState* state = context->state;
    NTSTATUS status = STATUS_SUCCESS;
    AcquireSRWLockShared(&state->dataLock);
    const auto& modes = state->slots[context->slot].targetModes;
    out->TargetModeBufferOutputCount = static_cast<UINT>(modes.size());
    if (in->TargetModeBufferInputCount != 0) {
        if (in->TargetModeBufferInputCount < modes.size()) {
            status = STATUS_BUFFER_TOO_SMALL;
        } else {
            for (size_t i = 0; i < modes.size(); ++i) FillTargetMode(modes[i], &in->pTargetModes[i]);
        }
    }
    ReleaseSRWLockShared(&state->dataLock);
    return status;
}

static NTSTATUS EvtMonitorAssignSwapChain(IDDCX_MONITOR monitor, const IDARG_IN_SETSWAPCHAIN* in) {
    MonitorContext* context = GetMonitorContext(monitor);
    DeviceState* state = context->state;
    MonitorSlot& s = state->slots[context->slot];
    AcquireSRWLockExclusive(&state->dataLock);
    FrameConsumer* previous = s.consumer;
    s.consumer = nullptr;
    ReleaseSRWLockExclusive(&state->dataLock);
    if (previous) previous->StopAndDelete();

    FrameConsumer* consumer =
        FrameConsumer::Start(in->hSwapChain, in->RenderAdapterLuid, in->hNextSurfaceAvailable, &s.frames, context->monitorId);
    if (!consumer) {
        Log(L"monitor %u: no thread for the swap chain", context->monitorId);
        WdfObjectDelete(reinterpret_cast<WDFOBJECT>(in->hSwapChain));  // the OS retries with a new one
        return STATUS_SUCCESS;
    }
    AcquireSRWLockExclusive(&state->dataLock);
    s.consumer = consumer;
    ReleaseSRWLockExclusive(&state->dataLock);
    Log(L"monitor %u: swap chain assigned (render adapter %08x:%08x)", context->monitorId, in->RenderAdapterLuid.HighPart,
        in->RenderAdapterLuid.LowPart);
    return STATUS_SUCCESS;
}

static NTSTATUS EvtMonitorUnassignSwapChain(IDDCX_MONITOR monitor) {
    MonitorContext* context = GetMonitorContext(monitor);
    DeviceState* state = context->state;
    MonitorSlot& s = state->slots[context->slot];
    AcquireSRWLockExclusive(&state->dataLock);
    FrameConsumer* consumer = s.consumer;
    s.consumer = nullptr;
    ReleaseSRWLockExclusive(&state->dataLock);
    if (consumer) consumer->StopAndDelete();
    Log(L"monitor %u: swap chain unassigned", context->monitorId);
    return STATUS_SUCCESS;
}

// ------------------------------------------------------------------------------------------------------------------
// Device

static NTSTATUS EvtDeviceD0Entry(WDFDEVICE device, WDF_POWER_DEVICE_STATE previousState) {
    DeviceState* state = GetDeviceContext(device)->state;
    if (state->adapter) return STATUS_SUCCESS;  // the adapter lives until the device stops
    UNREFERENCED_PARAMETER(previousState);

    static IDDCX_ENDPOINT_VERSION version = {sizeof(IDDCX_ENDPOINT_VERSION), 0, 1, 0, 0};
    IDDCX_ADAPTER_CAPS caps = {};
    caps.Size = sizeof(caps);
    // Real mode changes rather than OS-scaled ones: the desktop image is never larger than the chosen mode.
    caps.Flags = IDDCX_ADAPTER_FLAGS_USE_SMALLEST_MODE;
    caps.MaxMonitorsSupported = QQIDD_MAX_MONITORS;
    caps.EndPointDiagnostics.Size = sizeof(caps.EndPointDiagnostics);
    caps.EndPointDiagnostics.GammaSupport = IDDCX_FEATURE_IMPLEMENTATION_NONE;
    caps.EndPointDiagnostics.TransmissionType = IDDCX_TRANSMISSION_TYPE_WIRED_OTHER;
    caps.EndPointDiagnostics.pEndPointFriendlyName = L"Queqiao Super Screen";
    caps.EndPointDiagnostics.pEndPointManufacturerName = L"Queqiao";
    caps.EndPointDiagnostics.pEndPointModelName = L"Queqiao Virtual Display";
    caps.EndPointDiagnostics.pFirmwareVersion = &version;
    caps.EndPointDiagnostics.pHardwareVersion = &version;

    WDF_OBJECT_ATTRIBUTES attributes;
    WDF_OBJECT_ATTRIBUTES_INIT_CONTEXT_TYPE(&attributes, AdapterContext);
    IDARG_IN_ADAPTER_INIT init = {};
    init.WdfDevice = device;
    init.pCaps = &caps;
    init.ObjectAttributes = &attributes;
    IDARG_OUT_ADAPTER_INIT initOut = {};
    NTSTATUS status = IddCxAdapterInitAsync(&init, &initOut);
    Log(L"IddCxAdapterInitAsync 0x%08x", status);
    if (NT_SUCCESS(status)) {
        GetAdapterContext(initOut.AdapterObject)->state = state;
        state->adapter = initOut.AdapterObject;
    }
    return status;
}

static NTSTATUS EvtDeviceAdd(WDFDRIVER driver, PWDFDEVICE_INIT init) {
    UNREFERENCED_PARAMETER(driver);
    WDF_PNPPOWER_EVENT_CALLBACKS pnp;
    WDF_PNPPOWER_EVENT_CALLBACKS_INIT(&pnp);
    pnp.EvtDeviceD0Entry = EvtDeviceD0Entry;
    WdfDeviceInitSetPnpPowerEventCallbacks(init, &pnp);

    WDF_FILEOBJECT_CONFIG fileConfig;
    WDF_FILEOBJECT_CONFIG_INIT(&fileConfig, WDF_NO_EVENT_CALLBACK, WDF_NO_EVENT_CALLBACK, EvtFileCleanup);
    WdfDeviceInitSetFileObjectConfig(init, &fileConfig, WDF_NO_OBJECT_ATTRIBUTES);

    IDD_CX_CLIENT_CONFIG config;
    IDD_CX_CLIENT_CONFIG_INIT(&config);
    config.EvtIddCxDeviceIoControl = EvtIoctl;  // IddCx owns the I/O queue and passes our IOCTLs here
    config.EvtIddCxAdapterInitFinished = EvtAdapterInitFinished;
    config.EvtIddCxAdapterCommitModes = EvtAdapterCommitModes;
    config.EvtIddCxParseMonitorDescription = EvtParseMonitorDescription;
    config.EvtIddCxMonitorGetDefaultDescriptionModes = EvtMonitorGetDefaultModes;
    config.EvtIddCxMonitorQueryTargetModes = EvtMonitorQueryTargetModes;
    config.EvtIddCxMonitorAssignSwapChain = EvtMonitorAssignSwapChain;
    config.EvtIddCxMonitorUnassignSwapChain = EvtMonitorUnassignSwapChain;
    NTSTATUS status = IddCxDeviceInitConfig(init, &config);
    if (!NT_SUCCESS(status)) {
        Log(L"IddCxDeviceInitConfig 0x%08x", status);
        return status;
    }

    WDF_OBJECT_ATTRIBUTES attributes;
    WDF_OBJECT_ATTRIBUTES_INIT_CONTEXT_TYPE(&attributes, DeviceContext);
    attributes.EvtCleanupCallback = [](WDFOBJECT object) {
        DeviceState* state = GetDeviceContext(object)->state;
        AcquireSRWLockExclusive(&g_stateLock);
        if (g_state == state) g_state = nullptr;
        ReleaseSRWLockExclusive(&g_stateLock);
        if (state) {
            if (state->orphanThread) {
                SetEvent(state->stopEvent);
                if (WaitForSingleObject(state->orphanThread, 5000) != WAIT_OBJECT_0) {
                    Log(L"device removed; orphan worker still busy, its state is kept");
                    return;  // never free what a running thread uses
                }
                CloseHandle(state->orphanThread);
            }
            if (state->orphanEvent) CloseHandle(state->orphanEvent);
            if (state->stopEvent) CloseHandle(state->stopEvent);
            for (auto& s : state->slots)
                if (s.consumer) s.consumer->StopAndDelete(false);
            delete state;
        }
        Log(L"device removed");
    };
    WDFDEVICE device = nullptr;
    status = WdfDeviceCreate(&init, &attributes, &device);
    if (!NT_SUCCESS(status)) {
        Log(L"WdfDeviceCreate 0x%08x", status);
        return status;
    }
    status = IddCxDeviceInitialize(device);
    if (!NT_SUCCESS(status)) {
        Log(L"IddCxDeviceInitialize 0x%08x", status);
        return status;
    }
    auto* state = new (std::nothrow) DeviceState();
    if (!state) return STATUS_INSUFFICIENT_RESOURCES;
    state->device = device;
    state->orphanEvent = CreateEventW(nullptr, FALSE, FALSE, nullptr);
    state->stopEvent = CreateEventW(nullptr, TRUE, FALSE, nullptr);
    if (state->orphanEvent && state->stopEvent)
        state->orphanThread = CreateThread(nullptr, 0, OrphanWorker, state, 0, nullptr);
    GetDeviceContext(device)->state = state;
    if (!state->orphanThread) return STATUS_INSUFFICIENT_RESOURCES;

    AcquireSRWLockExclusive(&g_stateLock);
    const bool first = g_state == nullptr;
    if (first) g_state = state;
    ReleaseSRWLockExclusive(&g_stateLock);
    if (!first) {
        Log(L"a second Queqiao virtual display device is not supported");
        return STATUS_DEVICE_CONFIGURATION_ERROR;
    }
    status = WdfDeviceCreateDeviceInterface(device, &kInterfaceGuid, nullptr);
    Log(L"device added, interface 0x%08x", status);
    return status;
}

extern "C" DRIVER_INITIALIZE DriverEntry;

extern "C" NTSTATUS DriverEntry(PDRIVER_OBJECT driverObject, PUNICODE_STRING registryPath) {
    WDF_DRIVER_CONFIG config;
    WDF_DRIVER_CONFIG_INIT(&config, EvtDeviceAdd);
    NTSTATUS status = WdfDriverCreate(driverObject, registryPath, WDF_NO_OBJECT_ATTRIBUTES, &config, WDF_NO_HANDLE);
    Log(L"driver %u.%u.%u loaded 0x%08x", QQIDD_DRIVER_VERSION >> 24, (QQIDD_DRIVER_VERSION >> 16) & 0xFF,
        QQIDD_DRIVER_VERSION & 0xFFFF, status);
    return status;
}
