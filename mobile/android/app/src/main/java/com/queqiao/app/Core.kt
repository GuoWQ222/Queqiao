package com.queqiao.app

import java.nio.ByteBuffer

/** The Rust core (crates/queqiao-mobile, libqueqiao_mobile.so): its native methods (android.rs). */
object Core {
    init {
        System.loadLibrary("queqiao_mobile")
    }

    /** Starts the core: [folder] for this device's files, [deviceName] as the device list shows the phone. */
    @JvmStatic external fun start(folder: String, deviceName: String): Long

    @JvmStatic external fun stop(handle: Long)

    /** A command as JSON (see Core::command in the core); false if it is not understood. */
    @JvmStatic external fun command(handle: Long, json: String): Boolean

    /** The next event as JSON, or null when none came within [timeoutMs]. */
    @JvmStatic external fun nextEvent(handle: Long, timeoutMs: Int): String?

    /**
     * The next frame (Annex B) into [out] (direct): its length, 0 if none came, minus its length if [out] is too
     * small. [info]: codec (0 H.264, 1 AV1, 2 H.265), after a loss (0/1), time in 90 kHz units.
     */
    @JvmStatic external fun nextFrame(handle: Long, timeoutMs: Int, out: ByteBuffer, info: LongArray): Int

    /** The next Opus packet into [out]: as [nextFrame]; time[0] in 48 kHz units. */
    @JvmStatic external fun nextAudio(handle: Long, timeoutMs: Int, out: ByteBuffer, time: LongArray): Int
}
