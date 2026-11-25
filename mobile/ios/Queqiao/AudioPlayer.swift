import AVFoundation

/// The PC's sound: the core decodes each Opus packet (libopus, qq_next_audio_pcm); AVAudioEngine plays the samples.
/// At most about 200 ms waits to be played: later packets are dropped rather than let the delay grow.
final class AudioPlayer {
    private let engine = AVAudioEngine()
    private let node = AVAudioPlayerNode()
    private let format = AVAudioFormat(standardFormatWithSampleRate: 48_000, channels: 2)!
    private var running = true
    private let lock = NSLock()
    private var queued = 0
    var muted = false

    init() {
        let session = AVAudioSession.sharedInstance()
        try? session.setCategory(.playback, options: [.mixWithOthers])
        try? session.setActive(true)
        engine.attach(node)
        engine.connect(node, to: engine.mainMixerNode, format: format)
        try? engine.start()
        node.play()
        let thread = Thread { [weak self] in self?.run() }
        thread.name = "queqiao-audio"
        thread.qualityOfService = .userInteractive
        thread.start()
    }

    func stop() {
        running = false
        node.stop()
        engine.stop()
    }

    private func run() {
        let capacity = 5760 * 2
        let pcm = UnsafeMutablePointer<Float>.allocate(capacity: capacity)
        defer { pcm.deallocate() }
        var time: UInt64 = 0
        while running {
            let samples = qq_next_audio_pcm(Engine.shared.core, 20, pcm, capacity, &time)
            guard samples > 0, !muted else { continue }
            lock.lock()
            let waiting = queued
            lock.unlock()
            if waiting >= 20 { continue } // ~200 ms behind: catch up.
            guard let buffer = AVAudioPCMBuffer(pcmFormat: format, frameCapacity: AVAudioFrameCount(samples)),
                  let channels = buffer.floatChannelData else { continue }
            buffer.frameLength = AVAudioFrameCount(samples)
            for i in 0..<samples {
                channels[0][i] = pcm[2 * i]
                channels[1][i] = pcm[2 * i + 1]
            }
            lock.lock()
            queued += 1
            lock.unlock()
            node.scheduleBuffer(buffer) { [weak self] in
                guard let self else { return }
                self.lock.lock()
                self.queued -= 1
                self.lock.unlock()
            }
        }
    }
}
