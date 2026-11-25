import AVFoundation
import CoreMedia

/// The session's picture: frames from the core (Annex B) handed to an AVSampleBufferDisplayLayer, which decodes them
/// with the phone's hardware (VideoToolbox) and shows each at once. The parameter sets make the format; a new one when
/// they change (the PC switched codec or size); after a loss, or when the layer failed, frames wait for a keyframe.
final class VideoDecoder {
    /// Frames handed to the layer since the app started (the tests read it).
    static var shownTotal = 0

    private let layer: AVSampleBufferDisplayLayer
    private let onSize: (CGSize) -> Void
    private var running = true
    private var thread: Thread?
    private var format: CMVideoFormatDescription?
    private var parameterSets: [Data] = []
    private var codec: UInt32 = 99
    private var waitingForKeyframe = true
    private var size = CGSize.zero

    init(layer: AVSampleBufferDisplayLayer, onSize: @escaping (CGSize) -> Void) {
        self.layer = layer
        self.onSize = onSize
        let thread = Thread { [weak self] in self?.run() }
        thread.name = "queqiao-video"
        thread.qualityOfService = .userInteractive
        self.thread = thread
        thread.start()
    }

    func stop() { running = false }

    private func run() {
        let capacity = 8 << 20
        let buffer = UnsafeMutablePointer<UInt8>.allocate(capacity: capacity)
        defer { buffer.deallocate() }
        var info = QqFrameInfo()
        while running {
            let length = qq_next_frame(Engine.shared.core, 20, buffer, capacity, &info)
            if length > 0 {
                feed(UnsafeBufferPointer(start: buffer, count: length), codec: info.codec,
                     afterLoss: info.after_loss != 0)
            }
        }
    }

    /// The NAL units of an Annex B frame (without their start codes).
    private static func units(_ data: UnsafeBufferPointer<UInt8>) -> [UnsafeBufferPointer<UInt8>] {
        var starts: [(Int, Int)] = [] // (start code position, payload position)
        var i = 0
        let n = data.count
        while i + 2 < n {
            if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
                let at = (i > 0 && data[i - 1] == 0) ? i - 1 : i
                starts.append((at, i + 3))
                i += 3
            } else {
                i += 1
            }
        }
        var result: [UnsafeBufferPointer<UInt8>] = []
        for (k, start) in starts.enumerated() {
            let end = k + 1 < starts.count ? starts[k + 1].0 : n
            if end > start.1 {
                result.append(UnsafeBufferPointer(rebasing: data[start.1..<end]))
            }
        }
        return result
    }

    private func feed(_ data: UnsafeBufferPointer<UInt8>, codec: UInt32, afterLoss: Bool) {
        let hevc = codec == 2
        guard codec == 0 || hevc else { return } // AV1 is not asked for (the core offers H.264 and H.265).
        if codec != self.codec {
            self.codec = codec
            format = nil
            parameterSets = []
            waitingForKeyframe = true
        }
        if afterLoss { waitingForKeyframe = true }
        var sets: [Data] = []
        var slices: [UnsafeBufferPointer<UInt8>] = []
        var keyframe = false
        for unit in VideoDecoder.units(data) {
            guard let header = unit.first else { continue }
            if hevc {
                let type = (header >> 1) & 0x3F
                switch type {
                case 32, 33, 34: sets.append(Data(unit))
                case 35: break // access unit delimiter
                default:
                    if (16...21).contains(type) { keyframe = true }
                    slices.append(unit)
                }
            } else {
                let type = header & 0x1F
                switch type {
                case 7, 8: sets.append(Data(unit))
                case 9: break
                default:
                    if type == 5 { keyframe = true }
                    slices.append(unit)
                }
            }
        }
        if !sets.isEmpty && sets != parameterSets {
            if let made = VideoDecoder.makeFormat(sets, hevc: hevc) {
                format = made
                parameterSets = sets
                let dimensions = CMVideoFormatDescriptionGetPresentationDimensions(
                    made, usePixelAspectRatio: false, useCleanAperture: true)
                if dimensions != size {
                    size = dimensions
                    DispatchQueue.main.async { self.onSize(dimensions) }
                }
            }
        }
        if layerFailed() {
            flushLayer()
            waitingForKeyframe = true
        }
        if waitingForKeyframe {
            guard keyframe else { return }
            waitingForKeyframe = false
        }
        guard let format, !slices.isEmpty, let sample = VideoDecoder.makeSample(slices, format: format) else { return }
        enqueue(sample)
        VideoDecoder.shownTotal += 1
    }

    private func layerFailed() -> Bool {
        if #available(iOS 17.0, *) {
            return layer.sampleBufferRenderer.status == .failed
        }
        return layer.status == .failed
    }

    private func flushLayer() {
        if #available(iOS 17.0, *) {
            layer.sampleBufferRenderer.flush()
        } else {
            layer.flush()
        }
    }

    private func enqueue(_ sample: CMSampleBuffer) {
        if #available(iOS 17.0, *) {
            layer.sampleBufferRenderer.enqueue(sample)
        } else {
            layer.enqueue(sample)
        }
    }

    private static func makeFormat(_ sets: [Data], hevc: Bool) -> CMVideoFormatDescription? {
        let copies = sets.map { data -> UnsafeMutablePointer<UInt8> in
            let copy = UnsafeMutablePointer<UInt8>.allocate(capacity: max(1, data.count))
            data.copyBytes(to: copy, count: data.count)
            return copy
        }
        defer { copies.forEach { $0.deallocate() } }
        let pointers: [UnsafePointer<UInt8>] = copies.map { UnsafePointer($0) }
        let sizes = sets.map { $0.count }
        var description: CMFormatDescription?
        let status = pointers.withUnsafeBufferPointer { p -> OSStatus in
            sizes.withUnsafeBufferPointer { s -> OSStatus in
                if hevc {
                    return CMVideoFormatDescriptionCreateFromHEVCParameterSets(
                        allocator: kCFAllocatorDefault, parameterSetCount: sets.count,
                        parameterSetPointers: p.baseAddress!, parameterSetSizes: s.baseAddress!,
                        nalUnitHeaderLength: 4, extensions: nil, formatDescriptionOut: &description)
                }
                return CMVideoFormatDescriptionCreateFromH264ParameterSets(
                    allocator: kCFAllocatorDefault, parameterSetCount: sets.count,
                    parameterSetPointers: p.baseAddress!, parameterSetSizes: s.baseAddress!,
                    nalUnitHeaderLength: 4, formatDescriptionOut: &description)
            }
        }
        return status == noErr ? description : nil
    }

    /// The slices as one sample: each NAL unit after its length (4 bytes, big-endian), shown at once.
    private static func makeSample(_ slices: [UnsafeBufferPointer<UInt8>], format: CMFormatDescription) -> CMSampleBuffer? {
        var bytes = Data()
        for slice in slices {
            var length = UInt32(slice.count).bigEndian
            withUnsafeBytes(of: &length) { bytes.append(contentsOf: $0) }
            bytes.append(slice)
        }
        var block: CMBlockBuffer?
        guard CMBlockBufferCreateWithMemoryBlock(
            allocator: kCFAllocatorDefault, memoryBlock: nil, blockLength: bytes.count,
            blockAllocator: kCFAllocatorDefault, customBlockSource: nil, offsetToData: 0,
            dataLength: bytes.count, flags: kCMBlockBufferAssureMemoryNowFlag, blockBufferOut: &block) == noErr,
            let block else { return nil }
        let copied = bytes.withUnsafeBytes { raw -> OSStatus in
            CMBlockBufferReplaceDataBytes(with: raw.baseAddress!, blockBuffer: block,
                                          offsetIntoDestination: 0, dataLength: bytes.count)
        }
        guard copied == noErr else { return nil }
        var sample: CMSampleBuffer?
        var sampleSize = bytes.count
        guard CMSampleBufferCreateReady(
            allocator: kCFAllocatorDefault, dataBuffer: block, formatDescription: format, sampleCount: 1,
            sampleTimingEntryCount: 0, sampleTimingArray: nil, sampleSizeEntryCount: 1,
            sampleSizeArray: &sampleSize, sampleBufferOut: &sample) == noErr, let sample else { return nil }
        if let attachments = CMSampleBufferGetSampleAttachmentsArray(sample, createIfNecessary: true),
           CFArrayGetCount(attachments) > 0 {
            let dictionary = unsafeBitCast(CFArrayGetValueAtIndex(attachments, 0), to: CFMutableDictionary.self)
            CFDictionarySetValue(dictionary,
                                 Unmanaged.passUnretained(kCMSampleAttachmentKey_DisplayImmediately).toOpaque(),
                                 Unmanaged.passUnretained(kCFBooleanTrue).toOpaque())
        }
        return sample
    }
}
