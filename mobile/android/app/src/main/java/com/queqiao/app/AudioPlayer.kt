package com.queqiao.app

import android.media.AudioAttributes
import android.media.AudioFormat
import android.media.AudioTrack
import android.media.MediaCodec
import android.media.MediaFormat
import java.nio.ByteBuffer
import java.nio.ByteOrder
import kotlin.concurrent.thread

/** The PC's sound: Opus packets from the core decoded by the phone (MediaCodec) and played (AudioTrack). */
class AudioPlayer(private val handle: Long) {
    @Volatile private var running = true
    @Volatile var muted = false
    private val worker = thread(name = "queqiao-audio", isDaemon = true) { run() }

    fun stop() {
        running = false
        worker.join(1000)
    }

    private fun run() {
        val decoder = try {
            MediaCodec.createDecoderByType(MediaFormat.MIMETYPE_AUDIO_OPUS)
        } catch (_: Exception) {
            return
        }
        val format = MediaFormat.createAudioFormat(MediaFormat.MIMETYPE_AUDIO_OPUS, 48000, 2)
        // OpusHead (RFC 7845): version 1, 2 channels, pre-skip 312, 48 kHz, gain 0, mapping family 0.
        val head = ByteBuffer.allocate(19).order(ByteOrder.LITTLE_ENDIAN)
        head.put("OpusHead".toByteArray()).put(1).put(2).putShort(312).putInt(48000).putShort(0).put(0)
        head.flip()
        format.setByteBuffer("csd-0", head)
        val nanos = { value: Long -> ByteBuffer.allocate(8).order(ByteOrder.nativeOrder()).putLong(value).flip() as ByteBuffer }
        format.setByteBuffer("csd-1", nanos(312L * 1_000_000_000L / 48000))
        format.setByteBuffer("csd-2", nanos(80_000_000L))
        decoder.configure(format, null, null, 0)
        decoder.start()
        val track = AudioTrack.Builder()
            .setAudioAttributes(
                AudioAttributes.Builder().setUsage(AudioAttributes.USAGE_MEDIA)
                    .setContentType(AudioAttributes.CONTENT_TYPE_MOVIE).build(),
            )
            .setAudioFormat(
                AudioFormat.Builder().setEncoding(AudioFormat.ENCODING_PCM_16BIT).setSampleRate(48000)
                    .setChannelMask(AudioFormat.CHANNEL_OUT_STEREO).build(),
            )
            .setBufferSizeInBytes(48000 * 4 / 5)
            .setTransferMode(AudioTrack.MODE_STREAM)
            .build()
        track.play()
        val packet = ByteBuffer.allocateDirect(4096)
        val time = LongArray(1)
        val info = MediaCodec.BufferInfo()
        val pcm = ByteArray(48000 * 4 / 10)
        try {
            while (running) {
                val length = Core.nextAudio(handle, 20, packet, time)
                if (length > 0) {
                    val index = decoder.dequeueInputBuffer(5_000)
                    if (index >= 0) {
                        val input = decoder.getInputBuffer(index)!!
                        packet.position(0)
                        packet.limit(length)
                        input.clear()
                        input.put(packet)
                        packet.clear()
                        decoder.queueInputBuffer(index, 0, length, time[0] * 1_000_000 / 48000, 0)
                    }
                }
                while (true) {
                    val out = decoder.dequeueOutputBuffer(info, 0)
                    if (out < 0) break
                    val buffer = decoder.getOutputBuffer(out)!!
                    val count = minOf(info.size, pcm.size)
                    buffer.position(info.offset)
                    buffer.get(pcm, 0, count)
                    decoder.releaseOutputBuffer(out, false)
                    if (!muted) track.write(pcm, 0, count, AudioTrack.WRITE_NON_BLOCKING)
                }
            }
        } finally {
            track.stop()
            track.release()
            decoder.stop()
            decoder.release()
        }
    }
}
