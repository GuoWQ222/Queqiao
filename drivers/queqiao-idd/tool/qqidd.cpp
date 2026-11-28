// qqidd.exe: test tool for the Queqiao virtual display driver. Installs the root-enumerated device and the driver
// package, drives the driver through its IOCTLs, shows what Windows made of the virtual monitors, and removes it all.
// Written from the SetupAPI / Configuration Manager / CCD / DXGI documentation (the device creation follows the
// documented root-device sequence that Microsoft's devcon tool also uses).
//
// Copyright (c) 2026 GuoWQ222. Part of Queqiao; see the LICENSE file of the Queqiao repository.
//
// Usage: qqidd [--out <file>] <command> ...   (run "qqidd help")
#define NOMINMAX
#include <windows.h>
#include <cfgmgr32.h>
#include <d3d11.h>
#include <dxgi1_2.h>
#include <newdev.h>
#include <setupapi.h>
#include <shellscalingapi.h>
#include <wtsapi32.h>

#include <stdio.h>
#include <stdlib.h>
#include <wchar.h>

#include <string>
#include <vector>

#include "../include/queqiao_idd_ioctl.h"

#pragma comment(lib, "shcore.lib")

static const GUID kInterfaceGuid = QQIDD_INTERFACE_GUID_INIT;
// DEVPKEY_Device_DriverInfPath (devpkey.h), spelled out to avoid initguid.h.
static const DEVPROPKEY kDriverInfPath = {{0xa8b865dd, 0x2e3d, 0x4094, {0xad, 0x97, 0xe5, 0x93, 0xa7, 0x0c, 0x75, 0xd6}}, 5};

static void Fail(const wchar_t* what, DWORD error = GetLastError()) {
    wchar_t text[512] = L"";
    FormatMessageW(FORMAT_MESSAGE_FROM_SYSTEM | FORMAT_MESSAGE_IGNORE_INSERTS, nullptr, error, 0, text, 512, nullptr);
    for (wchar_t* p = text; *p; ++p)
        if (*p == L'\r' || *p == L'\n') *p = L' ';
    wprintf(L"FAIL %ls: error %lu (0x%08lx) %ls\n", what, error, error, text);
}

// ---------------------------------------------------------------------------------------------------------------
// Device lookup

struct FoundDevice {
    std::wstring instanceId;
    std::wstring infPath;  // oemNN.inf of the installed driver, if any
};

static bool HasOurHardwareId(HDEVINFO set, SP_DEVINFO_DATA* data) {
    wchar_t ids[1024] = {};
    if (!SetupDiGetDeviceRegistryPropertyW(set, data, SPDRP_HARDWAREID, nullptr, reinterpret_cast<BYTE*>(ids),
                                           sizeof(ids) - sizeof(wchar_t) * 2, nullptr))
        return false;
    for (const wchar_t* p = ids; *p; p += wcslen(p) + 1)
        if (_wcsicmp(p, QQIDD_HARDWARE_ID) == 0) return true;
    return false;
}

// All devices (present or not) with our hardware id; calls fn(set, data) on each.
template <typename F>
static int ForEachOurDevice(F fn) {
    HDEVINFO set = SetupDiGetClassDevsW(nullptr, L"ROOT", nullptr, DIGCF_ALLCLASSES);
    if (set == INVALID_HANDLE_VALUE) return 0;
    int count = 0;
    SP_DEVINFO_DATA data = {sizeof(data)};
    for (DWORD i = 0; SetupDiEnumDeviceInfo(set, i, &data); ++i) {
        if (!HasOurHardwareId(set, &data)) continue;
        ++count;
        fn(set, &data);
    }
    SetupDiDestroyDeviceInfoList(set);
    return count;
}

static FoundDevice Describe(HDEVINFO set, SP_DEVINFO_DATA* data) {
    FoundDevice d;
    wchar_t buffer[MAX_DEVICE_ID_LEN] = {};
    if (SetupDiGetDeviceInstanceIdW(set, data, buffer, MAX_DEVICE_ID_LEN, nullptr)) d.instanceId = buffer;
    DEVPROPTYPE type = 0;
    wchar_t inf[MAX_PATH] = {};
    if (SetupDiGetDevicePropertyW(set, data, &kDriverInfPath, &type, reinterpret_cast<BYTE*>(inf),
                                  sizeof(inf), nullptr, 0))
        d.infPath = inf;
    return d;
}

static std::wstring InterfacePath() {
    ULONG length = 0;
    if (CM_Get_Device_Interface_List_SizeW(&length, const_cast<GUID*>(&kInterfaceGuid), nullptr,
                                           CM_GET_DEVICE_INTERFACE_LIST_PRESENT) != CR_SUCCESS || length <= 1)
        return L"";
    std::vector<wchar_t> list(length);
    if (CM_Get_Device_Interface_ListW(const_cast<GUID*>(&kInterfaceGuid), nullptr, list.data(), length,
                                      CM_GET_DEVICE_INTERFACE_LIST_PRESENT) != CR_SUCCESS)
        return L"";
    return list.data();  // the first one (there is only one device)
}

static HANDLE OpenDriver(bool quiet = false) {
    std::wstring path = InterfacePath();
    if (path.empty()) {
        if (!quiet) wprintf(L"FAIL no Queqiao virtual display interface present (driver not installed or not started)\n");
        return INVALID_HANDLE_VALUE;
    }
    HANDLE h = CreateFileW(path.c_str(), GENERIC_READ | GENERIC_WRITE, 0, nullptr, OPEN_EXISTING, 0, nullptr);
    if (h == INVALID_HANDLE_VALUE && !quiet) Fail(L"CreateFileW on the driver interface");
    return h;
}

template <typename In, typename Out>
static bool Ioctl(HANDLE h, DWORD code, const In* in, Out* out, const wchar_t* what) {
    DWORD returned = 0;
    if (!DeviceIoControl(h, code, const_cast<In*>(in), in ? sizeof(In) : 0, out, out ? sizeof(Out) : 0, &returned, nullptr)) {
        Fail(what);
        return false;
    }
    return true;
}

// ---------------------------------------------------------------------------------------------------------------
// install / uninstall / status

static int CmdInstall(const wchar_t* inf, bool nonInteractive) {
    wchar_t full[MAX_PATH];
    if (!GetFullPathNameW(inf, MAX_PATH, full, nullptr)) return Fail(L"GetFullPathNameW"), 1;
    GUID classGuid;
    wchar_t className[MAX_CLASS_NAME_LEN];
    if (!SetupDiGetINFClassW(full, &classGuid, className, MAX_CLASS_NAME_LEN, nullptr))
        return Fail(L"SetupDiGetINFClassW (reading the INF)"), 1;

    bool created = false;
    std::wstring createdId;
    const int existing = ForEachOurDevice([](HDEVINFO, SP_DEVINFO_DATA*) {});
    if (existing == 0) {
        HDEVINFO set = SetupDiCreateDeviceInfoList(&classGuid, nullptr);
        if (set == INVALID_HANDLE_VALUE) return Fail(L"SetupDiCreateDeviceInfoList"), 1;
        SP_DEVINFO_DATA data = {sizeof(data)};
        // Device id "QueqiaoIdd" under ROOT with a generated instance number: ROOT\QUEQIAOIDD\0000.
        if (!SetupDiCreateDeviceInfoW(set, L"QueqiaoIdd", &classGuid, L"Queqiao Virtual Display", nullptr,
                                      DICD_GENERATE_ID, &data)) {
            Fail(L"SetupDiCreateDeviceInfoW");
            SetupDiDestroyDeviceInfoList(set);
            return 1;
        }
        static const wchar_t ids[] = QQIDD_HARDWARE_ID L"\0";
        if (!SetupDiSetDeviceRegistryPropertyW(set, &data, SPDRP_HARDWAREID, reinterpret_cast<const BYTE*>(ids), sizeof(ids))) {
            Fail(L"SetupDiSetDeviceRegistryPropertyW (hardware id)");
            SetupDiDestroyDeviceInfoList(set);
            return 1;
        }
        if (!SetupDiCallClassInstaller(DIF_REGISTERDEVICE, set, &data)) {
            Fail(L"SetupDiCallClassInstaller(DIF_REGISTERDEVICE)");
            SetupDiDestroyDeviceInfoList(set);
            return 1;
        }
        createdId = Describe(set, &data).instanceId;
        SetupDiDestroyDeviceInfoList(set);
        created = true;
        wprintf(L"created root device %ls\n", createdId.c_str());
    } else {
        wprintf(L"root device already present (%d)\n", existing);
    }

    BOOL reboot = FALSE;
    const DWORD flags = INSTALLFLAG_FORCE | (nonInteractive ? INSTALLFLAG_NONINTERACTIVE : 0);
    const ULONGLONG t0 = GetTickCount64();
    if (!UpdateDriverForPlugAndPlayDevicesW(nullptr, QQIDD_HARDWARE_ID, full, flags, &reboot)) {
        const DWORD error = GetLastError();
        Fail(L"UpdateDriverForPlugAndPlayDevicesW", error);
        if (created) {  // do not leave a driverless device behind
            ForEachOurDevice([](HDEVINFO set, SP_DEVINFO_DATA* data) { DiUninstallDevice(nullptr, set, data, 0, nullptr); });
            wprintf(L"removed the device created above\n");
        }
        return 1;
    }
    wprintf(L"driver installed in %llu ms%ls\n", GetTickCount64() - t0, reboot ? L" (Windows asks for a reboot)" : L"");
    return 0;
}

static int CmdUninstall() {
    std::vector<std::wstring> infs;
    int removed = 0;
    ForEachOurDevice([&](HDEVINFO set, SP_DEVINFO_DATA* data) {
        FoundDevice d = Describe(set, data);
        if (!d.infPath.empty()) infs.push_back(d.infPath);
        BOOL reboot = FALSE;
        if (DiUninstallDevice(nullptr, set, data, 0, &reboot)) {
            wprintf(L"removed device %ls%ls\n", d.instanceId.c_str(), reboot ? L" (reboot needed)" : L"");
            ++removed;
        } else {
            Fail(L"DiUninstallDevice");
        }
    });
    // Driver packages: the ones bound to the devices, and any other copy of QueqiaoIdd.inf in the driver store.
    wchar_t pattern[MAX_PATH];
    GetWindowsDirectoryW(pattern, MAX_PATH);
    std::wstring infDir = std::wstring(pattern) + L"\\INF\\";
    WIN32_FIND_DATAW fd;
    HANDLE find = FindFirstFileW((infDir + L"oem*.inf").c_str(), &fd);
    if (find != INVALID_HANDLE_VALUE) {
        do {
            std::wstring path = infDir + fd.cFileName;
            DWORD size = 0;
            if (!SetupGetInfInformationW(path.c_str(), INFINFO_INF_NAME_IS_ABSOLUTE, nullptr, 0, &size)) continue;
            std::vector<BYTE> buffer(size);
            auto* info = reinterpret_cast<SP_INF_INFORMATION*>(buffer.data());
            if (!SetupGetInfInformationW(path.c_str(), INFINFO_INF_NAME_IS_ABSOLUTE, info, size, nullptr)) continue;
            SP_ORIGINAL_FILE_INFO_W original = {sizeof(original)};
            if (SetupQueryInfOriginalFileInformationW(info, 0, nullptr, &original) &&
                _wcsicmp(original.OriginalInfName, L"QueqiaoIdd.inf") == 0) {
                bool known = false;
                for (auto& i : infs)
                    if (_wcsicmp(i.c_str(), fd.cFileName) == 0) known = true;
                if (!known) infs.push_back(fd.cFileName);
            }
        } while (FindNextFileW(find, &fd));
        FindClose(find);
    }
    int failures = 0;
    for (auto& inf : infs) {
        if (SetupUninstallOEMInfW(inf.c_str(), SUOI_FORCEDELETE, nullptr)) {
            wprintf(L"deleted driver package %ls\n", inf.c_str());
        } else {
            Fail((L"SetupUninstallOEMInfW " + inf).c_str());
            ++failures;
        }
    }
    wprintf(L"uninstall: %d device(s), %zu package(s)\n", removed, infs.size());
    return failures ? 1 : 0;
}

static int CmdStatus() {
    int n = ForEachOurDevice([](HDEVINFO set, SP_DEVINFO_DATA* data) {
        FoundDevice d = Describe(set, data);
        ULONG status = 0, problem = 0;
        CONFIGRET cr = CM_Get_DevNode_Status(&status, &problem, data->DevInst, 0);
        if (cr == CR_NO_SUCH_DEVINST)
            wprintf(L"device %ls: not present\n", d.instanceId.c_str());
        else
            wprintf(L"device %ls: %ls, problem %lu, driver %ls\n", d.instanceId.c_str(),
                    (status & DN_STARTED) ? L"started" : L"NOT started", problem,
                    d.infPath.empty() ? L"(none)" : d.infPath.c_str());
    });
    if (n == 0) wprintf(L"no Root\\QueqiaoIdd device\n");
    std::wstring path = InterfacePath();
    wprintf(L"interface: %ls\n", path.empty() ? L"(none present)" : path.c_str());
    return 0;
}

// ---------------------------------------------------------------------------------------------------------------
// IOCTL commands

static bool ParseMode(const wchar_t* text, QQIDD_MODE* mode) {
    unsigned w = 0, h = 0, r = 60;
    int n = swscanf_s(text, L"%ux%u@%u", &w, &h, &r);
    if (n < 2) return false;
    *mode = {w, h, r};
    return true;
}

static void PrintInfo(const QQIDD_MONITOR_INFO& m) {
    wprintf(L"monitor %u: connector %u, %ux%u@%u, %u modes, adapter %08lx:%08lx target %u, frames %llu%ls%ls%ls\n",
            m.MonitorId, m.ConnectorIndex, m.Preferred.Width, m.Preferred.Height, m.Preferred.RefreshHz, m.ModeCount,
            static_cast<unsigned long>(m.AdapterLuidHigh), static_cast<unsigned long>(m.AdapterLuidLow), m.TargetId,
            m.FramesConsumed, (m.Flags & QQIDD_INFO_SWAPCHAIN) ? L", swap chain" : L"",
            (m.Flags & QQIDD_INFO_KEEP_AFTER_CLOSE) ? L", kept after close" : L"",
            (m.Flags & QQIDD_INFO_REPLUGGED) ? L", replugged" : L"");
}

// add/update arguments: <id> <WxH@R> [--mm W H] [--keep] [--extra WxH@R]... [--hold S]
static bool ParseConfig(int argc, wchar_t** argv, QQIDD_MONITOR_CONFIG* c, DWORD* holdSeconds) {
    if (argc < 2) return false;
    *c = {};
    c->Size = sizeof(*c);
    c->MonitorId = wcstoul(argv[0], nullptr, 10);
    if (!ParseMode(argv[1], &c->Preferred)) return false;
    for (int i = 2; i < argc; ++i) {
        if (!wcscmp(argv[i], L"--mm") && i + 2 < argc) {
            c->WidthMm = wcstoul(argv[++i], nullptr, 10);
            c->HeightMm = wcstoul(argv[++i], nullptr, 10);
        } else if (!wcscmp(argv[i], L"--keep")) {
            c->Flags |= QQIDD_CONFIG_KEEP_AFTER_CLOSE;
        } else if (!wcscmp(argv[i], L"--extra") && i + 1 < argc && c->ExtraModeCount < QQIDD_MAX_EXTRA_MODES) {
            if (!ParseMode(argv[++i], &c->ExtraModes[c->ExtraModeCount++])) return false;
        } else if (!wcscmp(argv[i], L"--hold") && i + 1 < argc && holdSeconds) {
            *holdSeconds = wcstoul(argv[++i], nullptr, 10);
        } else {
            return false;
        }
    }
    return true;
}

static bool ListMonitors(HANDLE h, QQIDD_MONITOR_LIST* list) {
    *list = {};
    return Ioctl<int, QQIDD_MONITOR_LIST>(h, IOCTL_QQIDD_LIST_MONITORS, nullptr, list, L"IOCTL_QQIDD_LIST_MONITORS");
}

static int CmdIoctl(const wchar_t* cmd, int argc, wchar_t** argv) {
    HANDLE h = OpenDriver();
    if (h == INVALID_HANDLE_VALUE) return 1;
    int rc = 0;
    if (!wcscmp(cmd, L"info")) {
        QQIDD_DRIVER_INFO info = {};
        if (Ioctl<int, QQIDD_DRIVER_INFO>(h, IOCTL_QQIDD_GET_INFO, nullptr, &info, L"IOCTL_QQIDD_GET_INFO"))
            wprintf(L"interface v%u, driver %u.%u.%u, IddCx 0x%x, adapter %ls, %u/%u monitors, %u extra modes max\n",
                    info.InterfaceVersion, info.DriverVersion >> 24, (info.DriverVersion >> 16) & 0xFF,
                    info.DriverVersion & 0xFFFF, info.IddCxVersion, info.AdapterReady ? L"ready" : L"NOT ready",
                    info.MonitorCount, info.MaxMonitors, info.MaxExtraModes);
        else
            rc = 1;
    } else if (!wcscmp(cmd, L"add") || !wcscmp(cmd, L"update")) {
        QQIDD_MONITOR_CONFIG config;
        DWORD hold = 0;
        if (!ParseConfig(argc, argv, &config, &hold)) {
            wprintf(L"FAIL usage: %ls <id> <WxH@R> [--mm W H] [--keep] [--extra WxH@R]... [--hold S]\n", cmd);
            rc = 2;
        } else {
            QQIDD_MONITOR_INFO info = {};
            const bool add = !wcscmp(cmd, L"add");
            const ULONGLONG t0 = GetTickCount64();
            if (Ioctl(h, add ? IOCTL_QQIDD_ADD_MONITOR : IOCTL_QQIDD_UPDATE_MODES, &config, &info,
                      add ? L"IOCTL_QQIDD_ADD_MONITOR" : L"IOCTL_QQIDD_UPDATE_MODES")) {
                wprintf(L"%ls in %llu ms: ", add ? L"added" : L"updated", GetTickCount64() - t0);
                PrintInfo(info);
                if (hold) {
                    wprintf(L"holding the handle for %lu s (the monitor goes when it closes)\n", hold);
                    fflush(stdout);
                    Sleep(hold * 1000);
                }
            } else {
                rc = 1;
            }
        }
    } else if (!wcscmp(cmd, L"remove")) {
        QQIDD_MONITOR_ID id = {};
        id.Size = sizeof(id);
        id.MonitorId = (argc > 0 && wcscmp(argv[0], L"all") != 0) ? static_cast<UINT32>(wcstoul(argv[0], nullptr, 10)) : 0;
        if (Ioctl<QQIDD_MONITOR_ID, int>(h, IOCTL_QQIDD_REMOVE_MONITOR, &id, nullptr, L"IOCTL_QQIDD_REMOVE_MONITOR"))
            wprintf(L"removed %ls\n", id.MonitorId ? argv[0] : L"all");
        else
            rc = 1;
    } else if (!wcscmp(cmd, L"list")) {
        QQIDD_MONITOR_LIST list;
        if (ListMonitors(h, &list)) {
            wprintf(L"%u monitor(s)\n", list.Count);
            for (UINT32 i = 0; i < list.Count; ++i) PrintInfo(list.Monitors[i]);
        } else {
            rc = 1;
        }
    } else if (!wcscmp(cmd, L"log")) {
        std::vector<wchar_t> text(64 * 1024);
        DWORD returned = 0;
        if (DeviceIoControl(h, IOCTL_QQIDD_GET_LOG, nullptr, 0, text.data(), static_cast<DWORD>(text.size() * sizeof(wchar_t)),
                            &returned, nullptr)) {
            text.back() = 0;
            wprintf(L"%ls", text.data());
        } else {
            Fail(L"IOCTL_QQIDD_GET_LOG");
            rc = 1;
        }
    }
    CloseHandle(h);
    return rc;
}

// ---------------------------------------------------------------------------------------------------------------
// What Windows shows (run these in the session whose displays you want: the console)

struct Resolved {
    bool found = false;
    bool active = false;
    std::wstring gdiName;  // \\.\DISPLAYn
    DISPLAYCONFIG_PATH_INFO path = {};
};

static Resolved ResolveMonitor(UINT32 monitorId) {
    Resolved r;
    HANDLE h = OpenDriver();
    if (h == INVALID_HANDLE_VALUE) return r;
    QQIDD_MONITOR_LIST list;
    bool ok = ListMonitors(h, &list);
    CloseHandle(h);
    if (!ok) return r;
    const QQIDD_MONITOR_INFO* m = nullptr;
    for (UINT32 i = 0; i < list.Count; ++i)
        if (list.Monitors[i].MonitorId == monitorId) m = &list.Monitors[i];
    if (!m) {
        wprintf(L"FAIL monitor %u is not in the driver's list\n", monitorId);
        return r;
    }
    UINT32 pathCount = 0, modeCount = 0;
    if (GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &pathCount, &modeCount) != ERROR_SUCCESS) return r;
    std::vector<DISPLAYCONFIG_PATH_INFO> paths(pathCount);
    std::vector<DISPLAYCONFIG_MODE_INFO> modes(modeCount);
    if (QueryDisplayConfig(QDC_ONLY_ACTIVE_PATHS, &pathCount, paths.data(), &modeCount, modes.data(), nullptr) != ERROR_SUCCESS)
        return r;
    for (UINT32 i = 0; i < pathCount; ++i) {
        const auto& p = paths[i];
        if (p.targetInfo.adapterId.LowPart == m->AdapterLuidLow && p.targetInfo.adapterId.HighPart == m->AdapterLuidHigh &&
            p.targetInfo.id == m->TargetId) {
            DISPLAYCONFIG_SOURCE_DEVICE_NAME source = {};
            source.header.type = DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME;
            source.header.size = sizeof(source);
            source.header.adapterId = p.sourceInfo.adapterId;
            source.header.id = p.sourceInfo.id;
            if (DisplayConfigGetDeviceInfo(&source.header) == ERROR_SUCCESS) r.gdiName = source.viewGdiDeviceName;
            r.found = true;
            r.active = true;
            r.path = p;
        }
    }
    if (!r.found) wprintf(L"monitor %u has no active display path in this session\n", monitorId);
    return r;
}

static void UseInputDesktop() {
    HDESK desk = OpenInputDesktop(0, FALSE, GENERIC_ALL);
    if (desk && !SetThreadDesktop(desk)) CloseDesktop(desk);
}

static const wchar_t* TechName(DISPLAYCONFIG_VIDEO_OUTPUT_TECHNOLOGY t) {
    switch (t) {
    case DISPLAYCONFIG_OUTPUT_TECHNOLOGY_HDMI: return L"HDMI";
    case DISPLAYCONFIG_OUTPUT_TECHNOLOGY_DISPLAYPORT_EXTERNAL: return L"DisplayPort";
    case DISPLAYCONFIG_OUTPUT_TECHNOLOGY_INTERNAL: return L"internal";
    case DISPLAYCONFIG_OUTPUT_TECHNOLOGY_INDIRECT_WIRED: return L"indirect wired";
    case DISPLAYCONFIG_OUTPUT_TECHNOLOGY_INDIRECT_VIRTUAL: return L"indirect virtual";
    case DISPLAYCONFIG_OUTPUT_TECHNOLOGY_OTHER: return L"other";
    default: return L"?";
    }
}

static int CmdDisplays() {
    UseInputDesktop();
    DWORD session = 0;
    ProcessIdToSessionId(GetCurrentProcessId(), &session);
    wprintf(L"session %lu (console session %lu)\n", session, WTSGetActiveConsoleSessionId());
    UINT32 pathCount = 0, modeCount = 0;
    LONG e = GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &pathCount, &modeCount);
    std::vector<DISPLAYCONFIG_PATH_INFO> paths(pathCount);
    std::vector<DISPLAYCONFIG_MODE_INFO> modes(modeCount);
    if (e == ERROR_SUCCESS) e = QueryDisplayConfig(QDC_ONLY_ACTIVE_PATHS, &pathCount, paths.data(), &modeCount, modes.data(), nullptr);
    if (e != ERROR_SUCCESS) {
        Fail(L"QueryDisplayConfig", static_cast<DWORD>(e));
    } else {
        wprintf(L"QueryDisplayConfig: %u active path(s)\n", pathCount);
        for (UINT32 i = 0; i < pathCount; ++i) {
            const auto& p = paths[i];
            DISPLAYCONFIG_SOURCE_DEVICE_NAME source = {};
            source.header = {DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME, sizeof(source), p.sourceInfo.adapterId, p.sourceInfo.id};
            DisplayConfigGetDeviceInfo(&source.header);
            DISPLAYCONFIG_TARGET_DEVICE_NAME target = {};
            target.header = {DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME, sizeof(target), p.targetInfo.adapterId, p.targetInfo.id};
            DisplayConfigGetDeviceInfo(&target.header);
            UINT32 w = 0, h = 0;
            LONG x = 0, y = 0;
            if (p.sourceInfo.modeInfoIdx < modeCount && modes[p.sourceInfo.modeInfoIdx].infoType == DISPLAYCONFIG_MODE_INFO_TYPE_SOURCE) {
                const auto& sm = modes[p.sourceInfo.modeInfoIdx].sourceMode;
                w = sm.width;
                h = sm.height;
                x = sm.position.x;
                y = sm.position.y;
            }
            const double hz = p.targetInfo.refreshRate.Denominator
                                  ? static_cast<double>(p.targetInfo.refreshRate.Numerator) / p.targetInfo.refreshRate.Denominator
                                  : 0.0;
            wprintf(L"  %ls -> \"%ls\" (edid %04x/%04x, %ls) %ux%u at (%ld,%ld) %.2f Hz, target %08lx:%08lx/%u\n",
                    source.viewGdiDeviceName, target.monitorFriendlyDeviceName, target.edidManufactureId,
                    target.edidProductCodeId, TechName(target.outputTechnology), w, h, x, y, hz,
                    static_cast<unsigned long>(p.targetInfo.adapterId.HighPart), p.targetInfo.adapterId.LowPart,
                    p.targetInfo.id);
        }
    }
    UINT32 allPaths = 0, allModes = 0;
    if (GetDisplayConfigBufferSizes(QDC_ALL_PATHS, &allPaths, &allModes) == ERROR_SUCCESS) {
        std::vector<DISPLAYCONFIG_PATH_INFO> ap(allPaths);
        std::vector<DISPLAYCONFIG_MODE_INFO> am(allModes);
        if (QueryDisplayConfig(QDC_ALL_PATHS, &allPaths, ap.data(), &allModes, am.data(), nullptr) == ERROR_SUCCESS) {
            wprintf(L"connected targets (QDC_ALL_PATHS):\n");
            std::vector<std::pair<LUID, UINT32>> seen;
            for (UINT32 i = 0; i < allPaths; ++i) {
                const auto& t = ap[i].targetInfo;
                if (!t.targetAvailable) continue;
                bool dup = false;
                for (auto& x : seen)
                    if (x.first.LowPart == t.adapterId.LowPart && x.first.HighPart == t.adapterId.HighPart && x.second == t.id) dup = true;
                if (dup) continue;
                seen.push_back({t.adapterId, t.id});
                DISPLAYCONFIG_TARGET_DEVICE_NAME target = {};
                target.header = {DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME, sizeof(target), t.adapterId, t.id};
                DisplayConfigGetDeviceInfo(&target.header);
                bool active = false;
                for (UINT32 k = 0; k < allPaths; ++k)
                    if ((ap[k].flags & DISPLAYCONFIG_PATH_ACTIVE) && ap[k].targetInfo.adapterId.LowPart == t.adapterId.LowPart &&
                        ap[k].targetInfo.adapterId.HighPart == t.adapterId.HighPart && ap[k].targetInfo.id == t.id)
                        active = true;
                wprintf(L"  \"%ls\" target %08lx:%08lx/%u %ls\n", target.monitorFriendlyDeviceName,
                        static_cast<unsigned long>(t.adapterId.HighPart), t.adapterId.LowPart, t.id, active ? L"ACTIVE" : L"inactive");
            }
        }
    }
    SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    wprintf(L"EnumDisplayDevices:\n");
    DISPLAY_DEVICEW dd = {sizeof(dd)};
    for (DWORD i = 0; EnumDisplayDevicesW(nullptr, i, &dd, 0); ++i, dd.cb = sizeof(dd)) {
        if (!(dd.StateFlags & DISPLAY_DEVICE_ATTACHED_TO_DESKTOP)) {
            dd.cb = sizeof(dd);
            continue;
        }
        DEVMODEW dm = {};
        dm.dmSize = sizeof(dm);
        EnumDisplaySettingsW(dd.DeviceName, ENUM_CURRENT_SETTINGS, &dm);
        DISPLAY_DEVICEW mon = {sizeof(mon)};
        EnumDisplayDevicesW(dd.DeviceName, 0, &mon, 0);
        int modeTotal = 0;
        DEVMODEW any = {};
        any.dmSize = sizeof(any);
        for (DWORD k = 0; EnumDisplaySettingsW(dd.DeviceName, k, &any); ++k) ++modeTotal;
        POINT center = {dm.dmPosition.x + static_cast<LONG>(dm.dmPelsWidth) / 2, dm.dmPosition.y + static_cast<LONG>(dm.dmPelsHeight) / 2};
        UINT dpiX = 0, dpiY = 0;
        GetDpiForMonitor(MonitorFromPoint(center, MONITOR_DEFAULTTONEAREST), MDT_EFFECTIVE_DPI, &dpiX, &dpiY);
        wprintf(L"  %ls \"%ls\"%ls: %lux%lu@%lu at (%ld,%ld), %d modes listed, effective DPI %u (%u%%); monitor \"%ls\"\n",
                dd.DeviceName, dd.DeviceString, (dd.StateFlags & DISPLAY_DEVICE_PRIMARY_DEVICE) ? L" PRIMARY" : L"",
                dm.dmPelsWidth, dm.dmPelsHeight, dm.dmDisplayFrequency, dm.dmPosition.x, dm.dmPosition.y, modeTotal, dpiX,
                dpiX * 100 / 96, mon.DeviceString);
    }
    return 0;
}

static int CmdModes(UINT32 id) {
    UseInputDesktop();
    Resolved r = ResolveMonitor(id);
    if (r.gdiName.empty()) return 1;
    DEVMODEW dm = {};
    dm.dmSize = sizeof(dm);
    wprintf(L"modes of %ls:", r.gdiName.c_str());
    std::wstring last;
    for (DWORD k = 0; EnumDisplaySettingsW(r.gdiName.c_str(), k, &dm); ++k) {
        wchar_t text[64];
        swprintf_s(text, L" %lux%lu@%lu", dm.dmPelsWidth, dm.dmPelsHeight, dm.dmDisplayFrequency);
        if (last != text) wprintf(L"%ls", text);
        last = text;
    }
    wprintf(L"\n");
    return 0;
}

static int CmdSetMode(UINT32 id, const wchar_t* modeText) {
    UseInputDesktop();
    QQIDD_MODE mode;
    if (!ParseMode(modeText, &mode)) return wprintf(L"FAIL mode WxH@R\n"), 2;
    Resolved r = ResolveMonitor(id);
    if (r.gdiName.empty()) return 1;
    DEVMODEW dm = {};
    dm.dmSize = sizeof(dm);
    dm.dmPelsWidth = mode.Width;
    dm.dmPelsHeight = mode.Height;
    dm.dmDisplayFrequency = mode.RefreshHz;
    dm.dmFields = DM_PELSWIDTH | DM_PELSHEIGHT | DM_DISPLAYFREQUENCY;
    const ULONGLONG t0 = GetTickCount64();
    LONG result = ChangeDisplaySettingsExW(r.gdiName.c_str(), &dm, nullptr, 0, nullptr);
    wprintf(L"ChangeDisplaySettingsExW(%ls, %ux%u@%u) = %ld (%ls) in %llu ms\n", r.gdiName.c_str(), mode.Width, mode.Height,
            mode.RefreshHz, result, result == DISP_CHANGE_SUCCESSFUL ? L"DISP_CHANGE_SUCCESSFUL" : L"failed",
            GetTickCount64() - t0);
    return result == DISP_CHANGE_SUCCESSFUL ? 0 : 1;
}

// Undocumented, but what every "set DPI scale" tool uses: DisplayConfig{Get,Set}DeviceInfo with types -3 / -4 and
// the scale given in steps relative to the recommended one. Per user (the console's user, or SYSTEM's settings at
// the logon screen).
struct DpiScaleGet {
    DISPLAYCONFIG_DEVICE_INFO_HEADER header;
    INT32 minRel, curRel, maxRel;
};
struct DpiScaleSet {
    DISPLAYCONFIG_DEVICE_INFO_HEADER header;
    INT32 rel;
};
static const UINT32 kScales[] = {100, 125, 150, 175, 200, 225, 250, 300, 350, 400, 450, 500};

static int CmdScale(UINT32 id, const wchar_t* percentText) {
    UseInputDesktop();
    Resolved r = ResolveMonitor(id);
    if (!r.found) return 1;
    DpiScaleGet get = {};
    get.header.type = static_cast<DISPLAYCONFIG_DEVICE_INFO_TYPE>(-3);
    get.header.size = sizeof(get);
    get.header.adapterId = r.path.sourceInfo.adapterId;
    get.header.id = r.path.sourceInfo.id;
    LONG e = DisplayConfigGetDeviceInfo(&get.header);
    if (e != ERROR_SUCCESS) return Fail(L"DisplayConfigGetDeviceInfo(DPI scale)", static_cast<DWORD>(e)), 1;
    // Index of the recommended scale = -minRel.
    const int recommended = -get.minRel;
    auto scaleAt = [&](int rel) { int i = recommended + rel; return (i >= 0 && i < 12) ? kScales[i] : 0u; };
    wprintf(L"scale of %ls: current %u%%, recommended %u%%, range %u%%..%u%%\n", r.gdiName.c_str(), scaleAt(get.curRel),
            scaleAt(0), scaleAt(get.minRel), scaleAt(get.maxRel));
    if (!percentText) return 0;
    const UINT32 want = wcstoul(percentText, nullptr, 10);
    int index = -1;
    for (int i = 0; i < 12; ++i)
        if (kScales[i] == want) index = i;
    if (index < 0) return wprintf(L"FAIL scale must be one of 100,125,150,...\n"), 2;
    int rel = index - recommended;
    if (rel < get.minRel) rel = get.minRel;
    if (rel > get.maxRel) rel = get.maxRel;
    DpiScaleSet set = {};
    set.header.type = static_cast<DISPLAYCONFIG_DEVICE_INFO_TYPE>(-4);
    set.header.size = sizeof(set);
    set.header.adapterId = r.path.sourceInfo.adapterId;
    set.header.id = r.path.sourceInfo.id;
    set.rel = rel;
    e = DisplayConfigSetDeviceInfo(&set.header);
    if (e != ERROR_SUCCESS) return Fail(L"DisplayConfigSetDeviceInfo(DPI scale)", static_cast<DWORD>(e)), 1;
    wprintf(L"scale set to %u%%\n", scaleAt(rel));
    return 0;
}

static bool WriteBmp(const wchar_t* file, const BYTE* bgra, UINT32 pitch, UINT32 width, UINT32 height, UINT32 scale) {
    const UINT32 ow = width / scale, oh = height / scale;
    const UINT32 rowBytes = (ow * 3 + 3) & ~3u;
    BITMAPFILEHEADER fh = {};
    BITMAPINFOHEADER ih = {};
    ih.biSize = sizeof(ih);
    ih.biWidth = static_cast<LONG>(ow);
    ih.biHeight = static_cast<LONG>(oh);
    ih.biPlanes = 1;
    ih.biBitCount = 24;
    ih.biSizeImage = rowBytes * oh;
    fh.bfType = 0x4D42;
    fh.bfOffBits = sizeof(fh) + sizeof(ih);
    fh.bfSize = fh.bfOffBits + ih.biSizeImage;
    FILE* f = nullptr;
    if (_wfopen_s(&f, file, L"wb") || !f) return false;
    fwrite(&fh, sizeof(fh), 1, f);
    fwrite(&ih, sizeof(ih), 1, f);
    std::vector<BYTE> row(rowBytes);
    for (UINT32 y = 0; y < oh; ++y) {
        const BYTE* src = bgra + static_cast<size_t>(height - 1 - y * scale) * pitch;
        for (UINT32 x = 0; x < ow; ++x) {
            const BYTE* px = src + static_cast<size_t>(x) * scale * 4;
            row[x * 3] = px[0];
            row[x * 3 + 1] = px[1];
            row[x * 3 + 2] = px[2];
        }
        fwrite(row.data(), rowBytes, 1, f);
    }
    fclose(f);
    return true;
}

static void WakeDisplay() {
    SetThreadExecutionState(ES_DISPLAY_REQUIRED | ES_SYSTEM_REQUIRED);
    INPUT input = {};
    input.type = INPUT_MOUSE;
    input.mi.dwFlags = MOUSEEVENTF_MOVE;
    SendInput(1, &input, sizeof(input));
}

static int CmdCapture(UINT32 id, const wchar_t* file, UINT32 scale) {
    UseInputDesktop();
    WakeDisplay();  // a powered-off display composes black
    Sleep(1500);
    Resolved r = ResolveMonitor(id);
    if (r.gdiName.empty()) return 1;
    IDXGIFactory1* factory = nullptr;
    if (FAILED(CreateDXGIFactory1(__uuidof(IDXGIFactory1), reinterpret_cast<void**>(&factory)))) return Fail(L"CreateDXGIFactory1"), 1;
    IDXGIAdapter1* adapter = nullptr;
    IDXGIOutput1* output = nullptr;
    for (UINT a = 0; !output && factory->EnumAdapters1(a, &adapter) != DXGI_ERROR_NOT_FOUND; ++a) {
        DXGI_ADAPTER_DESC1 ad = {};
        adapter->GetDesc1(&ad);
        IDXGIOutput* o = nullptr;
        for (UINT k = 0; adapter->EnumOutputs(k, &o) != DXGI_ERROR_NOT_FOUND; ++k) {
            DXGI_OUTPUT_DESC od = {};
            o->GetDesc(&od);
            wprintf(L"DXGI adapter %u \"%ls\" output %u %ls\n", a, ad.Description, k, od.DeviceName);
            if (!output && _wcsicmp(od.DeviceName, r.gdiName.c_str()) == 0) o->QueryInterface(__uuidof(IDXGIOutput1), reinterpret_cast<void**>(&output));
            o->Release();
        }
        if (!output) {
            adapter->Release();
            adapter = nullptr;
        }
    }
    factory->Release();
    if (!output) return wprintf(L"FAIL no DXGI output named %ls\n", r.gdiName.c_str()), 1;
    ID3D11Device* device = nullptr;
    ID3D11DeviceContext* context = nullptr;
    HRESULT hr = D3D11CreateDevice(adapter, D3D_DRIVER_TYPE_UNKNOWN, nullptr, 0, nullptr, 0, D3D11_SDK_VERSION, &device, nullptr, &context);
    if (FAILED(hr)) return Fail(L"D3D11CreateDevice", static_cast<DWORD>(hr)), 1;
    IDXGIOutputDuplication* dup = nullptr;
    hr = output->DuplicateOutput(device, &dup);
    if (FAILED(hr)) return Fail(L"DuplicateOutput", static_cast<DWORD>(hr)), 1;
    DXGI_OUTDUPL_DESC dd = {};
    dup->GetDesc(&dd);
    wprintf(L"duplicating %ls: %ux%u\n", r.gdiName.c_str(), dd.ModeDesc.Width, dd.ModeDesc.Height);
    IDXGIResource* resource = nullptr;
    DXGI_OUTDUPL_FRAME_INFO fi = {};
    int frames = 0;
    ID3D11Texture2D* staging = nullptr;
    int rc = 1;
    for (int attempt = 0; attempt < 20; ++attempt) {
        hr = dup->AcquireNextFrame(500, &fi, &resource);
        if (hr == DXGI_ERROR_WAIT_TIMEOUT) continue;
        if (FAILED(hr)) {
            Fail(L"AcquireNextFrame", static_cast<DWORD>(hr));
            break;
        }
        ++frames;
        if (fi.LastPresentTime.QuadPart == 0 && frames < 3) {  // pointer-only update: wait for an image
            resource->Release();
            dup->ReleaseFrame();
            continue;
        }
        ID3D11Texture2D* texture = nullptr;
        resource->QueryInterface(__uuidof(ID3D11Texture2D), reinterpret_cast<void**>(&texture));
        D3D11_TEXTURE2D_DESC td = {};
        texture->GetDesc(&td);
        td.Usage = D3D11_USAGE_STAGING;
        td.BindFlags = 0;
        td.CPUAccessFlags = D3D11_CPU_ACCESS_READ;
        td.MiscFlags = 0;
        hr = device->CreateTexture2D(&td, nullptr, &staging);
        if (SUCCEEDED(hr)) context->CopyResource(staging, texture);
        texture->Release();
        resource->Release();
        dup->ReleaseFrame();
        if (FAILED(hr)) {
            Fail(L"CreateTexture2D", static_cast<DWORD>(hr));
            break;
        }
        D3D11_MAPPED_SUBRESOURCE map = {};
        hr = context->Map(staging, 0, D3D11_MAP_READ, 0, &map);
        if (SUCCEEDED(hr)) {
            const bool ok = WriteBmp(file, static_cast<const BYTE*>(map.pData), map.RowPitch, td.Width, td.Height, scale ? scale : 1);
            context->Unmap(staging, 0);
            wprintf(L"frame %ux%u (format %u) after %d acquire(s), written to %ls (1/%u size): %ls\n", td.Width, td.Height,
                    td.Format, frames, file, scale ? scale : 1, ok ? L"ok" : L"FAILED");
            rc = ok ? 0 : 1;
        } else {
            Fail(L"Map", static_cast<DWORD>(hr));
        }
        break;
    }
    if (rc && frames == 0) wprintf(L"FAIL no frame within 10 s\n");
    if (staging) staging->Release();
    dup->Release();
    context->Release();
    device->Release();
    output->Release();
    adapter->Release();
    return rc;
}

// Stress: monitors added through a handle that is closed at once (they must go with it), and monitors added and
// removed at once over one handle. Prints each round's time; a hang would show as a missing line.
static int CmdChurn(DWORD rounds) {
    for (DWORD i = 0; i < rounds; ++i) {
        const ULONGLONG t0 = GetTickCount64();
        HANDLE h = OpenDriver();
        if (h == INVALID_HANDLE_VALUE) return 1;
        QQIDD_MONITOR_CONFIG c = {};
        c.Size = sizeof(c);
        c.MonitorId = 100 + i;
        c.Preferred = {1920, 1080, 60};
        QQIDD_MONITOR_INFO info = {};
        bool added = Ioctl(h, IOCTL_QQIDD_ADD_MONITOR, &c, &info, L"add (closed at once)");
        CloseHandle(h);
        const ULONGLONG t1 = GetTickCount64();
        h = OpenDriver();
        if (h == INVALID_HANDLE_VALUE) return 1;
        c.MonitorId = 200 + i;
        c.Preferred = {2560, 1440, 60};
        bool added2 = Ioctl(h, IOCTL_QQIDD_ADD_MONITOR, &c, &info, L"add (removed at once)");
        QQIDD_MONITOR_ID id = {sizeof(id), c.MonitorId};
        bool removed = added2 && Ioctl<QQIDD_MONITOR_ID, int>(h, IOCTL_QQIDD_REMOVE_MONITOR, &id, nullptr, L"remove at once");
        CloseHandle(h);
        wprintf(L"round %lu: add+close %ls in %llu ms, add+remove %ls in %llu ms\n", i, added ? L"ok" : L"FAILED", t1 - t0,
                (added2 && removed) ? L"ok" : L"FAILED", GetTickCount64() - t1);
        fflush(stdout);
    }
    HANDLE h = OpenDriver();
    if (h == INVALID_HANDLE_VALUE) return 1;
    QQIDD_MONITOR_LIST list = {};
    for (int wait = 0; wait < 50; ++wait) {
        if (!ListMonitors(h, &list) || list.Count == 0) break;
        Sleep(200);
    }
    wprintf(L"monitors left after churn: %u\n", list.Count);
    CloseHandle(h);
    return list.Count == 0 ? 0 : 1;
}

// Keeps the console's display on (as an agent does while streaming) and prints the monitor's consumed frames each
// second. With the display powered off (idle timeout, 1 min at the logon screen) Windows commits the virtual monitor's
// path as inactive and the driver gets no swap chain.
static int CmdAwake(UINT32 id, DWORD seconds) {
    UseInputDesktop();
    SetThreadExecutionState(ES_CONTINUOUS | ES_DISPLAY_REQUIRED | ES_SYSTEM_REQUIRED);
    INPUT input = {};
    input.type = INPUT_MOUSE;
    input.mi.dwFlags = MOUSEEVENTF_MOVE;  // a zero move: counts as user input, wakes the display
    UINT sent = SendInput(1, &input, sizeof(input));
    wprintf(L"display required; SendInput %u\n", sent);
    HANDLE h = OpenDriver();
    if (h == INVALID_HANDLE_VALUE) return 1;
    for (DWORD i = 0; i <= seconds; ++i) {
        QQIDD_MONITOR_LIST list;
        if (ListMonitors(h, &list))
            for (UINT32 k = 0; k < list.Count; ++k)
                if (list.Monitors[k].MonitorId == id)
                    wprintf(L"  t=%2lus monitor %u: frames %llu%ls\n", i, id, list.Monitors[k].FramesConsumed,
                            (list.Monitors[k].Flags & QQIDD_INFO_SWAPCHAIN) ? L", swap chain" : L"");
        if (i < seconds) Sleep(1000);
    }
    CloseHandle(h);
    SetThreadExecutionState(ES_CONTINUOUS);
    return 0;
}

// Display topology with the CCD API (what Super Screen does): extend to the virtual monitor, or make it the only
// active display. Not saved to Windows' display database, so when the virtual monitor goes Windows returns to the
// configuration it has stored for the remaining monitors.
static int CmdTopology(const wchar_t* which) {
    UseInputDesktop();
    UINT32 flags = SDC_APPLY;
    if (!wcscmp(which, L"extend")) flags |= SDC_TOPOLOGY_EXTEND;
    else if (!wcscmp(which, L"clone")) flags |= SDC_TOPOLOGY_CLONE;
    else if (!wcscmp(which, L"internal")) flags |= SDC_TOPOLOGY_INTERNAL;
    else if (!wcscmp(which, L"external")) flags |= SDC_TOPOLOGY_EXTERNAL;
    else if (!wcscmp(which, L"database")) flags |= SDC_USE_DATABASE_CURRENT;
    else return wprintf(L"FAIL topology extend|clone|internal|external|database\n"), 2;
    const ULONGLONG t0 = GetTickCount64();
    LONG e = SetDisplayConfig(0, nullptr, 0, nullptr, flags);
    wprintf(L"SetDisplayConfig(%ls) = %ld in %llu ms\n", which, e, GetTickCount64() - t0);
    return e == ERROR_SUCCESS ? 0 : 1;
}

static int CmdOnly(UINT32 id) {
    UseInputDesktop();
    HANDLE h = OpenDriver();
    if (h == INVALID_HANDLE_VALUE) return 1;
    QQIDD_MONITOR_LIST list;
    bool ok = ListMonitors(h, &list);
    CloseHandle(h);
    const QQIDD_MONITOR_INFO* m = nullptr;
    for (UINT32 i = 0; ok && i < list.Count; ++i)
        if (list.Monitors[i].MonitorId == id) m = &list.Monitors[i];
    if (!m) return wprintf(L"FAIL monitor %u not found\n", id), 1;
    UINT32 pathCount = 0, modeCount = 0;
    if (GetDisplayConfigBufferSizes(QDC_ALL_PATHS, &pathCount, &modeCount) != ERROR_SUCCESS) return Fail(L"GetDisplayConfigBufferSizes"), 1;
    std::vector<DISPLAYCONFIG_PATH_INFO> paths(pathCount);
    std::vector<DISPLAYCONFIG_MODE_INFO> modes(modeCount);
    LONG e = QueryDisplayConfig(QDC_ALL_PATHS, &pathCount, paths.data(), &modeCount, modes.data(), nullptr);
    if (e != ERROR_SUCCESS) return Fail(L"QueryDisplayConfig", static_cast<DWORD>(e)), 1;
    for (UINT32 i = 0; i < pathCount; ++i) {
        DISPLAYCONFIG_PATH_INFO p = paths[i];
        if (p.targetInfo.adapterId.LowPart != m->AdapterLuidLow || p.targetInfo.adapterId.HighPart != m->AdapterLuidHigh ||
            p.targetInfo.id != m->TargetId || !p.targetInfo.targetAvailable)
            continue;
        p.flags = DISPLAYCONFIG_PATH_ACTIVE;
        p.sourceInfo.modeInfoIdx = DISPLAYCONFIG_PATH_MODE_IDX_INVALID;
        p.targetInfo.modeInfoIdx = DISPLAYCONFIG_PATH_MODE_IDX_INVALID;
        const ULONGLONG t0 = GetTickCount64();
        e = SetDisplayConfig(1, &p, 0, nullptr, SDC_APPLY | SDC_USE_SUPPLIED_DISPLAY_CONFIG | SDC_ALLOW_CHANGES);
        wprintf(L"SetDisplayConfig(only monitor %u, source %u) = %ld in %llu ms\n", id, p.sourceInfo.id, e, GetTickCount64() - t0);
        return e == ERROR_SUCCESS ? 0 : 1;
    }
    return wprintf(L"FAIL no available display path to monitor %u\n", id), 1;
}

// ---------------------------------------------------------------------------------------------------------------
// console: as SYSTEM, run "qqidd --out <file> <command...>" in the active console session and print its output.

static int CmdConsole(DWORD timeoutSeconds, int argc, wchar_t** argv) {
    const DWORD session = WTSGetActiveConsoleSessionId();
    if (session == 0xFFFFFFFF) return wprintf(L"FAIL no console session\n"), 1;
    HANDLE self = nullptr, primary = nullptr;
    if (!OpenProcessToken(GetCurrentProcess(), TOKEN_DUPLICATE | TOKEN_QUERY | TOKEN_ASSIGN_PRIMARY | TOKEN_ADJUST_SESSIONID, &self))
        return Fail(L"OpenProcessToken"), 1;
    if (!DuplicateTokenEx(self, MAXIMUM_ALLOWED, nullptr, SecurityImpersonation, TokenPrimary, &primary))
        return Fail(L"DuplicateTokenEx"), 1;
    CloseHandle(self);
    DWORD s = session;
    if (!SetTokenInformation(primary, TokenSessionId, &s, sizeof(s))) return Fail(L"SetTokenInformation(TokenSessionId) (needs SYSTEM)"), 1;

    wchar_t* user = nullptr;
    DWORD bytes = 0;
    bool loggedOn = WTSQuerySessionInformationW(WTS_CURRENT_SERVER_HANDLE, session, WTSUserName, &user, &bytes) && user && *user;
    if (user) WTSFreeMemory(user);

    wchar_t exe[MAX_PATH], temp[MAX_PATH], out[MAX_PATH];
    GetModuleFileNameW(nullptr, exe, MAX_PATH);
    GetTempPathW(MAX_PATH, temp);
    swprintf_s(out, L"%lsqqidd_console_%lu.txt", temp, GetCurrentProcessId());
    std::wstring cmd = L"\"" + std::wstring(exe) + L"\" --out \"" + out + L"\"";
    for (int i = 0; i < argc; ++i) cmd += L" \"" + std::wstring(argv[i]) + L"\"";
    STARTUPINFOW si = {sizeof(si)};
    wchar_t desktop[32];
    wcscpy_s(desktop, loggedOn ? L"winsta0\\default" : L"winsta0\\winlogon");
    si.lpDesktop = desktop;
    PROCESS_INFORMATION pi = {};
    std::vector<wchar_t> line(cmd.begin(), cmd.end());
    line.push_back(0);
    if (!CreateProcessAsUserW(primary, exe, line.data(), nullptr, nullptr, FALSE, CREATE_NO_WINDOW, nullptr, nullptr, &si, &pi))
        return Fail(L"CreateProcessAsUserW"), 1;
    CloseHandle(primary);
    DWORD code = 1;
    if (WaitForSingleObject(pi.hProcess, timeoutSeconds * 1000) == WAIT_TIMEOUT) {
        wprintf(L"FAIL timed out after %lu s\n", timeoutSeconds);
        TerminateProcess(pi.hProcess, 124);
    }
    GetExitCodeProcess(pi.hProcess, &code);
    CloseHandle(pi.hThread);
    CloseHandle(pi.hProcess);
    FILE* f = nullptr;
    if (!_wfopen_s(&f, out, L"rb") && f) {  // the child's UTF-8 output, printed through our (wide) stdout
        std::string raw;
        char buffer[4096];
        size_t got;
        while ((got = fread(buffer, 1, sizeof(buffer), f)) > 0) raw.append(buffer, got);
        fclose(f);
        DeleteFileW(out);
        size_t start = raw.compare(0, 3, "\xEF\xBB\xBF") == 0 ? 3 : 0;
        int wide = MultiByteToWideChar(CP_UTF8, 0, raw.data() + start, static_cast<int>(raw.size() - start), nullptr, 0);
        std::wstring text(static_cast<size_t>(wide), L'\0');
        MultiByteToWideChar(CP_UTF8, 0, raw.data() + start, static_cast<int>(raw.size() - start), &text[0], wide);
        wprintf(L"%ls", text.c_str());
    }
    wprintf(L"(ran in console session %lu on %ls, exit %lu)\n", session, desktop, code);
    return static_cast<int>(code);
}

static void Help() {
    wprintf(L"qqidd [--out <file>] <command>\n"
            L"  install <inf> [--noninteractive]   create Root\\QueqiaoIdd and install the driver package on it\n"
            L"  uninstall                          remove the device and delete the driver package(s)\n"
            L"  status                             device, driver and interface\n"
            L"  info | list | log                  driver info / monitors / the driver's recent log\n"
            L"  add <id> <WxH@R> [--mm W H] [--keep] [--extra WxH@R]... [--hold S]\n"
            L"  update <id> <WxH@R> [--mm W H] [--extra WxH@R]...\n"
            L"  remove <id|all>\n"
            L"  displays                           what this session's Windows shows (CCD, GDI, DPI)\n"
            L"  modes <id> | setmode <id> <WxH@R> | scale <id> [percent] | capture <id> <file.bmp> [1/n]\n"
            L"  topology extend|clone|internal|external|database | only <id>   display topology (CCD, not saved)\n"
            L"  awake <id> <seconds>               keep the display on and print the monitor's consumed frames\n"
            L"  churn <rounds>                     stress: add+close handle at once, add+remove at once\n"
            L"  console <timeout s> <command...>   (as SYSTEM) run a command in the console session\n");
}

int wmain(int argc, wchar_t** argv) {
    int a = 1;
    if (a + 1 < argc && !wcscmp(argv[a], L"--out")) {
        FILE* f = nullptr;
        _wfreopen_s(&f, argv[a + 1], L"w, ccs=UTF-8", stdout);
        a += 2;
    }
    if (a >= argc) return Help(), 2;
    const wchar_t* cmd = argv[a++];
    const int n = argc - a;
    wchar_t** rest = argv + a;
    int rc = 2;
    if (!wcscmp(cmd, L"install") && n >= 1) rc = CmdInstall(rest[0], n >= 2 && !wcscmp(rest[1], L"--noninteractive"));
    else if (!wcscmp(cmd, L"uninstall")) rc = CmdUninstall();
    else if (!wcscmp(cmd, L"status")) rc = CmdStatus();
    else if (!wcscmp(cmd, L"info") || !wcscmp(cmd, L"add") || !wcscmp(cmd, L"update") || !wcscmp(cmd, L"remove") ||
             !wcscmp(cmd, L"list") || !wcscmp(cmd, L"log")) rc = CmdIoctl(cmd, n, rest);
    else if (!wcscmp(cmd, L"displays")) rc = CmdDisplays();
    else if (!wcscmp(cmd, L"modes") && n >= 1) rc = CmdModes(wcstoul(rest[0], nullptr, 10));
    else if (!wcscmp(cmd, L"setmode") && n >= 2) rc = CmdSetMode(wcstoul(rest[0], nullptr, 10), rest[1]);
    else if (!wcscmp(cmd, L"scale") && n >= 1) rc = CmdScale(wcstoul(rest[0], nullptr, 10), n >= 2 ? rest[1] : nullptr);
    else if (!wcscmp(cmd, L"capture") && n >= 2) rc = CmdCapture(wcstoul(rest[0], nullptr, 10), rest[1], n >= 3 ? wcstoul(rest[2], nullptr, 10) : 4);
    else if (!wcscmp(cmd, L"churn") && n >= 1) rc = CmdChurn(wcstoul(rest[0], nullptr, 10));
    else if (!wcscmp(cmd, L"awake") && n >= 2) rc = CmdAwake(wcstoul(rest[0], nullptr, 10), wcstoul(rest[1], nullptr, 10));
    else if (!wcscmp(cmd, L"topology") && n >= 1) rc = CmdTopology(rest[0]);
    else if (!wcscmp(cmd, L"only") && n >= 1) rc = CmdOnly(wcstoul(rest[0], nullptr, 10));
    else if (!wcscmp(cmd, L"console") && n >= 2) rc = CmdConsole(wcstoul(rest[0], nullptr, 10), n - 1, rest + 1);
    else Help();
    fflush(stdout);
    return rc;
}
