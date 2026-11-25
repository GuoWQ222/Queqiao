// The Queqiao core's C interface (crates/queqiao-mobile/src/ffi.rs), for the iOS app (Swift imports it through its
// bridging header). Strings are UTF-8 and NUL-terminated; buffers are the caller's.
#ifndef QUEQIAO_MOBILE_H
#define QUEQIAO_MOBILE_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct QqCore QqCore;

typedef struct {
    uint32_t codec;      // 0 H.264, 1 AV1, 2 H.265
    uint32_t after_loss; // 1: frames were lost before this one (wait for a keyframe)
    uint64_t time_90khz;
} QqFrameInfo;

// Starts the core: folder for this device's files (the app's own), device_name as the device list shows the phone.
QqCore *qq_start(const char *folder, const char *device_name);
void qq_stop(QqCore *core);
// A command as JSON (see Core::command); 1 if understood, 0 if not.
int32_t qq_command(QqCore *core, const char *json);
// The next event as JSON into out (NUL-terminated): its length, 0 if none came, minus the room it needs if too small.
intptr_t qq_next_event(QqCore *core, uint32_t timeout_ms, uint8_t *out, size_t capacity);
// The next frame (Annex B) into out: its length, 0 if none came, minus its length if out is too small (it is dropped).
intptr_t qq_next_frame(QqCore *core, uint32_t timeout_ms, uint8_t *out, size_t capacity, QqFrameInfo *info);
// The next Opus packet: as qq_next_frame; time in 48 kHz units.
intptr_t qq_next_audio(QqCore *core, uint32_t timeout_ms, uint8_t *out, size_t capacity, uint64_t *time_48khz);
// The next sound packet decoded: 48 kHz stereo float samples, interleaved; returns samples per channel, 0 if none.
intptr_t qq_next_audio_pcm(QqCore *core, uint32_t timeout_ms, float *out, size_t capacity, uint64_t *time_48khz);

#ifdef __cplusplus
}
#endif

#endif
