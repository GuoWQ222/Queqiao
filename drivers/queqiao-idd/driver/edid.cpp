// Queqiao virtual display driver: mode lists, display timings and the generated EDID.
// EDID layout from VESA E-EDID 1.4; timings approximate CVT reduced blanking (a virtual monitor has no real signal,
// but Windows and programs that read the EDID expect plausible numbers).
//
// Copyright (c) 2026 GuoWQ222. Part of Queqiao; see the LICENSE file of the Queqiao repository.
#include "driver.h"

namespace qqidd {

namespace {

// Offered on every monitor besides the requested modes, so that the agent can switch with ChangeDisplaySettingsExW
// without replugging the monitor.
const QQIDD_MODE kCommonModes[] = {
    {3840, 2160, 60}, {3440, 1440, 60}, {2560, 1600, 60}, {2560, 1440, 60}, {2560, 1080, 60},
    {1920, 1200, 60}, {1920, 1080, 60}, {1680, 1050, 60}, {1600, 900, 60},  {1440, 900, 60},
    {1366, 768, 60},  {1280, 1024, 60}, {1280, 800, 60},  {1280, 720, 60},  {1024, 768, 60},
    {800, 600, 60},
};

// "QQV" in the EDID's compressed ASCII (5 bits per letter, A = 1). Not assigned in the UEFI PNP ID registry
// (checked 2026-11-28).
constexpr BYTE kVendorHi = 0x46, kVendorLo = 0x36;
constexpr UINT16 kProductCode = 0x0001;

// {AE6DD388-E1E7-4124-B416-7BDE709A9926}: namespace for the monitors' container ids.
constexpr GUID kContainerNamespace = {0xae6dd388, 0xe1e7, 0x4124, {0xb4, 0x16, 0x7b, 0xde, 0x70, 0x9a, 0x99, 0x26}};

bool SameMode(const QQIDD_MODE& a, const QQIDD_MODE& b) {
    return a.Width == b.Width && a.Height == b.Height && a.RefreshHz == b.RefreshHz;
}

void AddUnique(std::vector<QQIDD_MODE>& modes, const QQIDD_MODE& mode) {
    if (!ModeIsValid(mode)) return;
    for (const auto& m : modes)
        if (SameMode(m, mode)) return;
    modes.push_back(mode);
}

UINT32 DefaultMm(UINT32 pixels) {  // the size at 96 DPI
    return (pixels * 254u + 480u) / 960u;
}

void WriteDescriptorText(BYTE* d, BYTE tag, const char* text) {
    // Display descriptor: 00 00 00 <tag> 00, then 13 bytes of text ended by 0x0A and padded with spaces.
    d[0] = d[1] = d[2] = 0;
    d[3] = tag;
    d[4] = 0;
    size_t i = 0;
    for (; i < 13 && text[i]; ++i) d[5 + i] = static_cast<BYTE>(text[i]);
    if (i < 13) d[5 + i++] = 0x0A;
    for (; i < 13; ++i) d[5 + i] = 0x20;
}

void WriteDetailedTiming(BYTE* d, const Timing& t, UINT32 widthMm, UINT32 heightMm) {
    const UINT64 clock10k = (t.pixelClockHz + 9999) / 10000;
    const UINT32 hBlank = t.hTotal - t.width, vBlank = t.vTotal - t.height;
    d[0] = static_cast<BYTE>(clock10k & 0xFF);
    d[1] = static_cast<BYTE>((clock10k >> 8) & 0xFF);
    d[2] = static_cast<BYTE>(t.width & 0xFF);
    d[3] = static_cast<BYTE>(hBlank & 0xFF);
    d[4] = static_cast<BYTE>(((t.width >> 8) & 0x0F) << 4 | ((hBlank >> 8) & 0x0F));
    d[5] = static_cast<BYTE>(t.height & 0xFF);
    d[6] = static_cast<BYTE>(vBlank & 0xFF);
    d[7] = static_cast<BYTE>(((t.height >> 8) & 0x0F) << 4 | ((vBlank >> 8) & 0x0F));
    d[8] = static_cast<BYTE>(t.hFront & 0xFF);
    d[9] = static_cast<BYTE>(t.hSync & 0xFF);
    d[10] = static_cast<BYTE>((t.vFront & 0x0F) << 4 | (t.vSync & 0x0F));
    d[11] = static_cast<BYTE>(((t.hFront >> 8) & 0x03) << 6 | ((t.hSync >> 8) & 0x03) << 4 |
                              ((t.vFront >> 4) & 0x03) << 2 | ((t.vSync >> 4) & 0x03));
    d[12] = static_cast<BYTE>(widthMm & 0xFF);
    d[13] = static_cast<BYTE>(heightMm & 0xFF);
    d[14] = static_cast<BYTE>(((widthMm >> 8) & 0x0F) << 4 | ((heightMm >> 8) & 0x0F));
    d[15] = 0;
    d[16] = 0;
    d[17] = 0x1A;  // digital separate sync, +hsync, -vsync (as CVT reduced blanking)
}

bool FitsDetailedTiming(const Timing& t) {
    return t.width <= 4095 && t.height <= 4095 && (t.pixelClockHz + 9999) / 10000 <= 0xFFFF;
}

void FillSignal(const QQIDD_MODE& mode, UINT32 divider, DISPLAYCONFIG_VIDEO_SIGNAL_INFO* s) {
    const Timing t = MakeTiming(mode);
    *s = {};
    s->pixelRate = t.pixelClockHz;
    s->hSyncFreq.Numerator = t.vTotal * t.refresh;
    s->hSyncFreq.Denominator = 1;
    s->vSyncFreq.Numerator = t.refresh;
    s->vSyncFreq.Denominator = 1;
    s->activeSize.cx = t.width;
    s->activeSize.cy = t.height;
    s->totalSize.cx = t.hTotal;
    s->totalSize.cy = t.vTotal;
    s->AdditionalSignalInfo.videoStandard = 255;  // D3DKMDT_VSS_OTHER
    s->AdditionalSignalInfo.vSyncFreqDivider = divider;
    s->scanLineOrdering = DISPLAYCONFIG_SCANLINE_ORDERING_PROGRESSIVE;
}

}  // namespace

bool ModeIsValid(const QQIDD_MODE& mode) {
    return mode.Width >= QQIDD_MIN_SIZE && mode.Width <= QQIDD_MAX_SIZE && mode.Height >= QQIDD_MIN_SIZE &&
           mode.Height <= QQIDD_MAX_SIZE && mode.RefreshHz >= 24 && mode.RefreshHz <= QQIDD_MAX_REFRESH;
}

Timing MakeTiming(const QQIDD_MODE& mode) {
    Timing t = {};
    t.width = mode.Width;
    t.height = mode.Height;
    t.refresh = mode.RefreshHz;
    // Reduced blanking: 160 pixels of horizontal blanking, at least 460 us of vertical blanking.
    t.hFront = 48;
    t.hSync = 32;
    t.hTotal = t.width + 160;
    t.vFront = 3;
    t.vSync = 5;
    const double blankShare = 460e-6 * t.refresh;  // < 0.12 for refresh <= 240
    UINT32 vTotal = static_cast<UINT32>(t.height / (1.0 - blankShare)) + 1;
    if (vTotal < t.height + 14) vTotal = t.height + 14;
    t.vTotal = vTotal;
    t.pixelClockHz = static_cast<UINT64>(t.hTotal) * t.vTotal * t.refresh;
    return t;
}

std::vector<QQIDD_MODE> BuildModeList(const QQIDD_MONITOR_CONFIG& config) {
    std::vector<QQIDD_MODE> modes;
    AddUnique(modes, config.Preferred);
    for (UINT32 i = 0; i < config.ExtraModeCount && i < QQIDD_MAX_EXTRA_MODES; ++i) AddUnique(modes, config.ExtraModes[i]);
    for (const auto& common : kCommonModes) {
        AddUnique(modes, common);
        if (config.Preferred.RefreshHz != 60)
            AddUnique(modes, QQIDD_MODE{common.Width, common.Height, config.Preferred.RefreshHz});
    }
    return modes;
}

void BuildEdid(const QQIDD_MONITOR_CONFIG& config, BYTE (&e)[128]) {
    memset(e, 0, sizeof(e));
    const BYTE header[8] = {0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00};
    memcpy(e, header, sizeof(header));
    e[8] = kVendorHi;
    e[9] = kVendorLo;
    e[10] = static_cast<BYTE>(kProductCode & 0xFF);
    e[11] = static_cast<BYTE>(kProductCode >> 8);
    e[12] = static_cast<BYTE>(config.MonitorId & 0xFF);
    e[13] = static_cast<BYTE>((config.MonitorId >> 8) & 0xFF);
    e[14] = static_cast<BYTE>((config.MonitorId >> 16) & 0xFF);
    e[15] = static_cast<BYTE>((config.MonitorId >> 24) & 0xFF);
    e[16] = 0;              // week unspecified
    e[17] = 2026 - 1990;    // year of manufacture
    e[18] = 1;              // EDID 1.4
    e[19] = 4;
    e[20] = 0xA5;           // digital input, 8 bits per colour, DisplayPort

    UINT32 widthMm = config.WidthMm ? config.WidthMm : DefaultMm(config.Preferred.Width);
    UINT32 heightMm = config.HeightMm ? config.HeightMm : DefaultMm(config.Preferred.Height);
    if (widthMm > 4095) widthMm = 4095;
    if (heightMm > 4095) heightMm = 4095;
    UINT32 widthCm = (widthMm + 5) / 10, heightCm = (heightMm + 5) / 10;
    e[21] = static_cast<BYTE>(widthCm < 1 ? 1 : (widthCm > 255 ? 255 : widthCm));
    e[22] = static_cast<BYTE>(heightCm < 1 ? 1 : (heightCm > 255 ? 255 : heightCm));
    e[23] = 120;            // gamma 2.2
    e[24] = 0x06;           // RGB 4:4:4, sRGB default colour space, preferred timing is the native mode

    // sRGB chromaticity (10-bit fractions of 1024).
    const UINT32 rx = 655, ry = 338, gx = 307, gy = 614, bx = 154, by = 61, wx = 320, wy = 337;
    e[25] = static_cast<BYTE>((rx & 3) << 6 | (ry & 3) << 4 | (gx & 3) << 2 | (gy & 3));
    e[26] = static_cast<BYTE>((bx & 3) << 6 | (by & 3) << 4 | (wx & 3) << 2 | (wy & 3));
    e[27] = static_cast<BYTE>(rx >> 2);
    e[28] = static_cast<BYTE>(ry >> 2);
    e[29] = static_cast<BYTE>(gx >> 2);
    e[30] = static_cast<BYTE>(gy >> 2);
    e[31] = static_cast<BYTE>(bx >> 2);
    e[32] = static_cast<BYTE>(by >> 2);
    e[33] = static_cast<BYTE>(wx >> 2);
    e[34] = static_cast<BYTE>(wy >> 2);
    // 35..37: no established timings. 38..53: no standard timings.
    for (int i = 38; i < 54; ++i) e[i] = 0x01;

    // Descriptor 1: the preferred mode (or, if it does not fit a detailed timing, the largest 60 Hz one that does).
    Timing preferred = MakeTiming(config.Preferred);
    if (!FitsDetailedTiming(preferred)) preferred = MakeTiming(QQIDD_MODE{config.Preferred.Width, config.Preferred.Height, 60});
    if (!FitsDetailedTiming(preferred)) preferred = MakeTiming(QQIDD_MODE{3840, 2160, 60});
    WriteDetailedTiming(e + 54, preferred, widthMm, heightMm);

    // Descriptor 2: range limits (refresh 24..max, horizontal 1..510 kHz, pixel clock up to 2550 MHz).
    BYTE* r = e + 72;
    UINT32 maxRefresh = config.Preferred.RefreshHz;
    for (UINT32 i = 0; i < config.ExtraModeCount && i < QQIDD_MAX_EXTRA_MODES; ++i)
        if (config.ExtraModes[i].RefreshHz > maxRefresh) maxRefresh = config.ExtraModes[i].RefreshHz;
    if (maxRefresh < 60) maxRefresh = 60;
    r[3] = 0xFD;
    r[4] = 0x08;            // maximum horizontal rate + 255 kHz
    r[5] = 24;
    r[6] = static_cast<BYTE>(maxRefresh);
    r[7] = 1;
    r[8] = 255;
    r[9] = 255;
    r[10] = 0x01;           // range limits only
    r[11] = 0x0A;
    for (int i = 12; i < 18; ++i) r[i] = 0x20;

    // Descriptor 3: name. Descriptor 4: serial string (the monitor id again, readable).
    WriteDescriptorText(e + 90, 0xFC, "Queqiao");
    char serial[14];
    static const char hex[] = "0123456789ABCDEF";
    serial[0] = 'Q';
    serial[1] = 'Q';
    serial[2] = 'V';
    serial[3] = '-';
    for (int i = 0; i < 8; ++i) serial[4 + i] = hex[(config.MonitorId >> (28 - 4 * i)) & 0xF];
    serial[12] = 0;
    WriteDescriptorText(e + 108, 0xFF, serial);

    e[126] = 0;             // no extension blocks
    BYTE sum = 0;
    for (int i = 0; i < 127; ++i) sum = static_cast<BYTE>(sum + e[i]);
    e[127] = static_cast<BYTE>(0x100 - sum);
}

bool EdidMonitorId(const void* data, UINT size, UINT32* monitorId) {
    if (!data || size < 128) return false;
    const BYTE* e = static_cast<const BYTE*>(data);
    if (e[0] != 0 || e[1] != 0xFF || e[7] != 0 || e[8] != kVendorHi || e[9] != kVendorLo) return false;
    *monitorId = static_cast<UINT32>(e[12]) | static_cast<UINT32>(e[13]) << 8 | static_cast<UINT32>(e[14]) << 16 |
                 static_cast<UINT32>(e[15]) << 24;
    return true;
}

void FillMonitorMode(const QQIDD_MODE& mode, IDDCX_MONITOR_MODE* out) {
    *out = {};
    out->Size = sizeof(*out);
    out->Origin = IDDCX_MONITOR_MODE_ORIGIN_MONITORDESCRIPTOR;
    FillSignal(mode, 0, &out->MonitorVideoSignalInfo);  // monitor modes: divider must be 0
}

void FillTargetMode(const QQIDD_MODE& mode, IDDCX_TARGET_MODE* out) {
    *out = {};
    out->Size = sizeof(*out);
    FillSignal(mode, 1, &out->TargetVideoSignalInfo.targetVideoSignalInfo);  // desktop updates at the full rate
    out->RequiredBandwidth = 0;
}

GUID ContainerIdFor(UINT32 monitorId) {
    GUID g = kContainerNamespace;
    g.Data4[4] ^= static_cast<BYTE>(monitorId >> 24);
    g.Data4[5] ^= static_cast<BYTE>(monitorId >> 16);
    g.Data4[6] ^= static_cast<BYTE>(monitorId >> 8);
    g.Data4[7] ^= static_cast<BYTE>(monitorId);
    return g;
}

}  // namespace qqidd
