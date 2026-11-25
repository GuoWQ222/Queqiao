import AVFoundation
import UIKit

/// The view the picture is shown in: its layer decodes and shows the frames.
final class PictureView: UIView {
    override class var layerClass: AnyClass { AVSampleBufferDisplayLayer.self }
    var displayLayer: AVSampleBufferDisplayLayer { layer as! AVSampleBufferDisplayLayer }
}

/// The remote screen: the picture fitted into the view (zoomed and moved with two fingers), and the touches and typing
/// turned into the PC's mouse and keys. Two ways to use the mouse, as UU Remote has: touch (a finger is the pointer:
/// tap to click where it lands) and mouse (the screen is a touchpad moving a pointer drawn here).
final class RemoteScreenView: UIView, UIKeyInput {
    enum Mode { case touch, mouse }

    var mode: Mode = .touch {
        didSet { pointer.isHidden = mode != .mouse; placePointer() }
    }

    private let engine = Engine.shared
    private let picture = PictureView()
    private let pointer = UIImageView()
    private var decoder: VideoDecoder?
    private var pictureSize = CGSize(width: 16, height: 9)
    private var zoom: CGFloat = 1
    private var pan = CGPoint.zero
    private var pointerAt = CGPoint(x: 0.5, y: 0.5)

    override init(frame: CGRect) {
        super.init(frame: frame)
        backgroundColor = .black
        isMultipleTouchEnabled = true
        picture.displayLayer.videoGravity = .resize
        addSubview(picture)
        pointer.image = RemoteScreenView.arrow()
        pointer.isHidden = true
        addSubview(pointer)
        accessibilityIdentifier = "picture"
        isAccessibilityElement = true
        decoder = VideoDecoder(layer: picture.displayLayer) { [weak self] size in
            guard let self, size.width > 0, size.height > 0 else { return }
            self.pictureSize = size
            self.setNeedsLayout()
        }
    }

    required init?(coder: NSCoder) { fatalError("not from a storyboard") }

    deinit { decoder?.stop() }

    func stop() { decoder?.stop() }

    // MARK: Placing the picture

    private func fitted() -> CGRect {
        let scale = min(bounds.width / pictureSize.width, bounds.height / pictureSize.height)
        let w = pictureSize.width * scale
        let h = pictureSize.height * scale
        return CGRect(x: (bounds.width - w) / 2, y: (bounds.height - h) / 2, width: w, height: h)
    }

    private func shown() -> CGRect {
        let f = fitted()
        let w = f.width * zoom
        let h = f.height * zoom
        return CGRect(x: f.minX - (w - f.width) / 2 + pan.x, y: f.minY - (h - f.height) / 2 + pan.y, width: w, height: h)
    }

    private func clampPan() {
        let f = fitted()
        let limitX = max(0, (f.width * zoom - bounds.width) / 2)
        let limitY = max(0, (f.height * zoom - bounds.height) / 2)
        pan.x = min(max(pan.x, -limitX), limitX)
        pan.y = min(max(pan.y, -limitY), limitY)
    }

    override func layoutSubviews() {
        super.layoutSubviews()
        clampPan()
        picture.frame = shown()
        placePointer()
    }

    private func placePointer() {
        let s = shown()
        pointer.frame = CGRect(x: s.minX + pointerAt.x * s.width, y: s.minY + pointerAt.y * s.height, width: 14, height: 22)
    }

    private func toRemote(_ p: CGPoint) -> CGPoint {
        let s = shown()
        return CGPoint(x: min(max((p.x - s.minX) / s.width, 0), 1), y: min(max((p.y - s.minY) / s.height, 0), 1))
    }

    // MARK: Touches

    private var downTime = Date()
    private var downAt = CGPoint.zero
    private var last = CGPoint.zero
    private var fingers = 0
    private var mostFingers = 0
    private var moved = false
    private var leftDown = false
    private var lastTap = Date.distantPast
    private var pinchStart: CGFloat = 1
    private var zoomStart: CGFloat = 1
    private var twoStart = CGPoint.zero
    private var panStart = CGPoint.zero
    private var wheelRest: CGFloat = 0
    private var longPress: DispatchWorkItem?
    private let slop: CGFloat = 10

    private func active(_ event: UIEvent?) -> [UITouch] {
        (event?.allTouches ?? []).filter { $0.phase != .ended && $0.phase != .cancelled }
    }

    override func touchesBegan(_ touches: Set<UITouch>, with event: UIEvent?) {
        let now = active(event)
        if fingers == 0, let touch = touches.first {
            becomeFirstResponderIfKeyboard()
            let p = touch.location(in: self)
            downTime = Date()
            downAt = p
            last = p
            moved = false
            mostFingers = 1
            if mode == .mouse && Date().timeIntervalSince(lastTap) < 0.3 {
                // Double tap and hold: drag (as a touchpad).
                press(true)
            } else {
                let work = DispatchWorkItem { [weak self] in self?.onLongPress() }
                longPress = work
                DispatchQueue.main.asyncAfter(deadline: .now() + 0.5, execute: work)
            }
            if mode == .touch {
                let r = toRemote(p)
                engine.move(r.x, r.y)
            }
        }
        fingers = now.count
        mostFingers = max(mostFingers, fingers)
        if fingers >= 2 {
            longPress?.cancel()
            if leftDown && mode == .touch { press(false) }
            let a = now[0].location(in: self)
            let b = now[1].location(in: self)
            pinchStart = hypot(a.x - b.x, a.y - b.y)
            zoomStart = zoom
            twoStart = CGPoint(x: (a.x + b.x) / 2, y: (a.y + b.y) / 2)
            panStart = pan
            last = twoStart
            wheelRest = 0
        }
    }

    override func touchesMoved(_ touches: Set<UITouch>, with event: UIEvent?) {
        let now = active(event)
        if now.count >= 2 {
            twoFingers(now[0].location(in: self), now[1].location(in: self))
            return
        }
        guard fingers == 1, let touch = now.first else { return }
        let p = touch.location(in: self)
        if !moved && hypot(p.x - downAt.x, p.y - downAt.y) > slop {
            moved = true
            longPress?.cancel()
            if mode == .touch && !leftDown { press(true) }
        }
        if moved {
            if mode == .touch {
                let r = toRemote(p)
                engine.move(r.x, r.y)
            } else {
                let s = shown()
                let speed = hypot(p.x - last.x, p.y - last.y)
                let gain = 1.2 + min(2, speed / 12)
                pointerAt.x = min(max(pointerAt.x + (p.x - last.x) / s.width * gain, 0), 1)
                pointerAt.y = min(max(pointerAt.y + (p.y - last.y) / s.height * gain, 0), 1)
                engine.move(pointerAt.x, pointerAt.y)
                placePointer()
            }
        }
        last = p
    }

    override func touchesEnded(_ touches: Set<UITouch>, with event: UIEvent?) {
        let remaining = active(event).count
        fingers = remaining
        guard remaining == 0 else { return }
        longPress?.cancel()
        let quick = Date().timeIntervalSince(downTime) < 0.25
        if mostFingers == 2 && quick && !moved {
            click("right")
        } else if mostFingers == 1 {
            if leftDown {
                press(false)
            } else if !moved {
                if mode == .touch, let touch = touches.first {
                    let r = toRemote(touch.location(in: self))
                    engine.move(r.x, r.y)
                }
                click("left")
                lastTap = Date()
            }
        }
        if leftDown { press(false) }
    }

    override func touchesCancelled(_ touches: Set<UITouch>, with event: UIEvent?) {
        touchesEnded(touches, with: event)
    }

    /// Two fingers: pinch zooms, moving together scrolls (or moves the zoomed picture).
    private func twoFingers(_ a: CGPoint, _ b: CGPoint) {
        let centre = CGPoint(x: (a.x + b.x) / 2, y: (a.y + b.y) / 2)
        let ratio = hypot(a.x - b.x, a.y - b.y) / max(1, pinchStart)
        if abs(ratio - 1) > 0.08 || zoom != zoomStart || zoom > 1 {
            moved = true
            zoom = min(max(zoomStart * ratio, 1), 5)
            pan = CGPoint(x: panStart.x + centre.x - twoStart.x, y: panStart.y + centre.y - twoStart.y)
            setNeedsLayout()
        } else {
            if abs(centre.y - twoStart.y) > slop { moved = true }
            if moved {
                // About one notch per 40 points, the way the fingers go (as the phone's own lists scroll).
                wheelRest += (centre.y - last.y) / 40 * 120
                let notches = Int(wheelRest)
                if notches != 0 {
                    engine.wheel(notches)
                    wheelRest -= CGFloat(notches)
                }
            }
        }
        last = centre
    }

    private func onLongPress() {
        guard !moved, fingers == 1 else { return }
        UIImpactFeedbackGenerator(style: .medium).impactOccurred()
        moved = true
        if mode == .touch {
            click("right")
        } else {
            press(true)
        }
    }

    private func press(_ down: Bool) {
        leftDown = down
        engine.button("left", down: down)
    }

    private func click(_ button: String) {
        engine.button(button, down: true)
        engine.button(button, down: false)
    }

    // MARK: Typing

    /// Whether the phone's keyboard is up (the toolbar's Keyboard button).
    var keyboardShown = false {
        didSet {
            if keyboardShown { becomeFirstResponder() } else { resignFirstResponder() }
        }
    }

    private func becomeFirstResponderIfKeyboard() {
        if keyboardShown && !isFirstResponder { becomeFirstResponder() }
    }

    override var canBecomeFirstResponder: Bool { true }
    var hasText: Bool { true }
    var autocorrectionType: UITextAutocorrectionType = .no
    var autocapitalizationType: UITextAutocapitalizationType = .none
    var spellCheckingType: UITextSpellCheckingType = .no
    var smartQuotesType: UITextSmartQuotesType = .no
    var smartDashesType: UITextSmartDashesType = .no
    var keyboardType: UIKeyboardType = .default

    func insertText(_ text: String) { type(text) }

    func deleteBackward() { tap(Keys.backspace) }

    /// Types `text` on the PC as characters, in any language and whatever the PC's keyboard layout (the agent types them
    /// as such: TypeText).
    func type(_ text: String) {
        if !text.isEmpty { engine.typeText(text) }
    }

    func tap(_ code: Int) {
        engine.key(code, down: true)
        engine.key(code, down: false)
    }

    /// Key combinations: all down in order, then up in reverse.
    func combo(_ codes: [Int]) {
        for c in codes { engine.key(c, down: true) }
        for c in codes.reversed() { engine.key(c, down: false) }
    }

    // A hardware keyboard: each key as it is pressed and let go.
    override func pressesBegan(_ presses: Set<UIPress>, with event: UIPressesEvent?) {
        var handled = false
        for press in presses {
            if let key = press.key, let code = Keys.forHID(key.keyCode.rawValue) {
                engine.key(code, down: true)
                handled = true
            }
        }
        if !handled { super.pressesBegan(presses, with: event) }
    }

    override func pressesEnded(_ presses: Set<UIPress>, with event: UIPressesEvent?) {
        var handled = false
        for press in presses {
            if let key = press.key, let code = Keys.forHID(key.keyCode.rawValue) {
                engine.key(code, down: false)
                handled = true
            }
        }
        if !handled { super.pressesEnded(presses, with: event) }
    }

    override func pressesCancelled(_ presses: Set<UIPress>, with event: UIPressesEvent?) {
        pressesEnded(presses, with: event)
    }

    /// The pointer drawn in mouse mode (the PC's own pointer is not in the picture).
    private static func arrow() -> UIImage {
        let size = CGSize(width: 14, height: 22)
        return UIGraphicsImageRenderer(size: size).image { context in
            let path = UIBezierPath()
            path.move(to: CGPoint(x: 1, y: 1))
            path.addLine(to: CGPoint(x: 1, y: 18))
            path.addLine(to: CGPoint(x: 5.5, y: 14))
            path.addLine(to: CGPoint(x: 8.5, y: 21))
            path.addLine(to: CGPoint(x: 11, y: 20))
            path.addLine(to: CGPoint(x: 8, y: 13))
            path.addLine(to: CGPoint(x: 13, y: 13))
            path.close()
            UIColor.white.setFill()
            path.fill()
            UIColor.black.setStroke()
            path.lineWidth = 1.2
            path.stroke()
            _ = context
        }
    }
}
