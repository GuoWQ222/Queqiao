package com.queqiao.app

import android.media.MediaCodec
import android.media.MediaFormat
import android.os.Build
import android.view.Surface
import java.nio.ByteBuffer
import kotlin.concurrent.thread

/**
 * The session's picture: frames from the core decoded by the phone's hardware (MediaCodec) straight onto [surface].
 * A new decoder when the codec changes (the PC switched); after a loss frames wait for the next keyframe. Reports
 * the picture's size (for fitting it and for where a touch lands) through [onSize].
 */
class VideoDecoder(
    private val handle: Long,
    private val surface: Surface,
    private val onSize: (Int, Int) -> Unit,
) {
    @Volatile private var running = true
    private var codec: MediaCodec? = null
    private var codecNumber = -1
    private var waitingForKeyframe = false
    @Volatile var framesShown = 0L
        private set

    private val worker = thread(name = "queqiao-video", isDaemon = true) { run() }

    fun stop() {
        running = false
        worker.join(1000)
        codec?.let {
            try {
                it.stop()
            } catch (_: Exception) {
            }
            it.release()
        }
        codec = null
    }

    private fun run() {
        val buffer = ByteBuffer.allocateDirect(8 * 1024 * 1024)
        val info = LongArray(3)
        val output = MediaCodec.BufferInfo()
        while (running) {
            val length = Core.nextFrame(handle, 20, buffer, info)
            if (length > 0) {
                feed(buffer, length, info[0].toInt(), info[1] != 0L, info[2])
            }
            drain(output)
        }
    }

    private fun feed(buffer: ByteBuffer, length: Int, number: Int, afterLoss: Boolean, time90khz: Long) {
        if (number != codecNumber) {
            open(number)
            waitingForKeyframe = true
        }
        if (afterLoss) waitingForKeyframe = true
        if (waitingForKeyframe) {
            if (!isKeyframe(buffer, length, number)) return
            waitingForKeyframe = false
        }
        val decoder = codec ?: return
        val index = try {
            decoder.dequeueInputBuffer(10_000)
        } catch (_: Exception) {
            -1
        }
        if (index < 0) return
        val input = decoder.getInputBuffer(index) ?: return
        buffer.position(0)
        buffer.limit(length)
        input.clear()
        input.put(buffer)
        buffer.clear()
        decoder.queueInputBuffer(index, 0, length, time90khz * 100 / 9, 0)
    }

    private fun drain(output: MediaCodec.BufferInfo) {
        val decoder = codec ?: return
        while (true) {
            val index = try {
                decoder.dequeueOutputBuffer(output, 0)
            } catch (_: Exception) {
                return
            }
            when {
                index >= 0 -> {
                    decoder.releaseOutputBuffer(index, true)
                    framesShown++
                    shownTotal++
                }
                index == MediaCodec.INFO_OUTPUT_FORMAT_CHANGED -> {
                    val format = decoder.outputFormat
                    val width = if (format.containsKey("crop-right")) {
                        format.getInteger("crop-right") - format.getInteger("crop-left") + 1
                    } else {
                        format.getInteger(MediaFormat.KEY_WIDTH)
                    }
                    val height = if (format.containsKey("crop-bottom")) {
                        format.getInteger("crop-bottom") - format.getInteger("crop-top") + 1
                    } else {
                        format.getInteger(MediaFormat.KEY_HEIGHT)
                    }
                    onSize(width, height)
                }
                else -> return
            }
        }
    }

    private fun open(number: Int) {
        codec?.let {
            try {
                it.stop()
            } catch (_: Exception) {
            }
            it.release()
        }
        codec = null
        codecNumber = number
        val mime = when (number) {
            2 -> MediaFormat.MIMETYPE_VIDEO_HEVC
            1 -> MediaFormat.MIMETYPE_VIDEO_AV1
            else -> MediaFormat.MIMETYPE_VIDEO_AVC
        }
        val format = MediaFormat.createVideoFormat(mime, 1920, 1080)
        if (Build.VERSION.SDK_INT >= 30) format.setInteger(MediaFormat.KEY_LOW_LATENCY, 1)
        format.setInteger(MediaFormat.KEY_PRIORITY, 0)
        codec = MediaCodec.createDecoderByType(mime).also {
            it.configure(format, surface, null, 0)
            it.start()
        }
    }

    companion object {
        /** Frames shown by every decoder since the app started (the tests read it). */
        @Volatile @JvmStatic var shownTotal = 0L
        /** Whether an Annex B frame holds a keyframe: an IDR slice (H.264 type 5; H.265 types 16-21). */
        fun isKeyframe(buffer: ByteBuffer, length: Int, codec: Int): Boolean {
            var i = 0
            while (i + 3 < length) {
                if (buffer.get(i).toInt() == 0 && buffer.get(i + 1).toInt() == 0 && buffer.get(i + 2).toInt() == 1) {
                    val header = buffer.get(i + 3).toInt() and 0xFF
                    if (codec == 2) {
                        val type = (header shr 1) and 0x3F
                        if (type in 16..21) return true
                    } else if (codec == 0) {
                        if (header and 0x1F == 5) return true
                    } else {
                        return true
                    }
                    i += 3
                } else {
                    i++
                }
            }
            return false
        }
    }
}
