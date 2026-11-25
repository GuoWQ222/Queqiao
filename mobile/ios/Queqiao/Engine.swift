import Foundation
import UIKit

/// One device of the account, as the list shows it.
struct Device: Identifiable, Equatable {
    let id: String
    let name: String
    let online: Bool
    let controllable: Bool
}

/// Where the app is: logging in, the device list, a session.
enum Screen: Equatable {
    case login
    case devices
    case session(Device)
}

/// What a session shows over its picture.
struct SessionState: Equatable {
    var stage = "connecting"
    var relayed = false
    var path = ""
    var message = ""
    var passwordNeeded = false
}

/// The app's side of the Rust core (crates/queqiao-mobile, through include/queqiao_mobile.h): starts it, turns its
/// events into state the views observe, and sends commands. One per app.
final class Engine: ObservableObject {
    static let shared = Engine()

    let core: OpaquePointer?
    @Published var screen: Screen = .login
    @Published var devices: [Device] = []
    @Published var error = ""
    @Published var busy = true
    @Published var session = SessionState()
    @Published var notice = ""
    private(set) var ownDeviceId = ""

    private init() {
        let support = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
        let folder = support.appendingPathComponent("Queqiao", isDirectory: true)
        try? FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        core = qq_start(folder.path, UIDevice.current.name)
        let thread = Thread { [weak self] in self?.readEvents() }
        thread.name = "queqiao-events"
        thread.start()
        // The saved login, if there is one.
        command(["resume": [String: Any]()])
    }

    @discardableResult
    func command(_ object: [String: Any]) -> Bool {
        guard let data = try? JSONSerialization.data(withJSONObject: object),
              let text = String(data: data, encoding: .utf8) else { return false }
        return qq_command(core, text) != 0
    }

    func logIn(server: String, pin: String, user: String, password: String) {
        busy = true
        error = ""
        command(["login": [
            "server": server.trimmingCharacters(in: .whitespaces),
            "pin": pin.trimmingCharacters(in: .whitespaces).lowercased(),
            "user": user.trimmingCharacters(in: .whitespaces),
            "password": password,
        ]])
    }

    func logOut() { command(["log_out": [String: Any]()]) }

    func connect(_ device: Device) {
        session = SessionState()
        screen = .session(device)
        command(["connect": ["device": device.id]])
    }

    func disconnect() {
        command(["disconnect": [String: Any]()])
        screen = .devices
    }

    func remove(_ device: Device) { command(["remove_device": device.id]) }

    // Input: the picture's 0..1 coordinates, buttons, the wheel, keys by scancode.
    func move(_ x: CGFloat, _ y: CGFloat) {
        command(["move": ["x": Double(min(max(x, 0), 1)), "y": Double(min(max(y, 0), 1))]])
    }

    func button(_ name: String, down: Bool) { command(["button": ["button": name, "down": down]]) }

    func wheel(_ delta: Int, horizontal: Bool = false) {
        command(["wheel": ["delta": delta, "horizontal": horizontal]])
    }

    func key(_ scancode: Int, down: Bool) {
        command(["key": ["scancode": scancode, "extended": Keys.extended(scancode), "down": down]])
    }

    func remote(_ name: String) { command(["command": name]) }

    func password(_ text: String) { command(["password": text]) }

    func sendClipboard(_ text: String) { command(["clipboard": text]) }

    /// Text typed on the PC as characters (any language).
    func typeText(_ text: String) { command(["text": text]) }

    private func readEvents() {
        var buffer = [UInt8](repeating: 0, count: 1 << 20)
        while true {
            let length = buffer.withUnsafeMutableBufferPointer { qq_next_event(core, 500, $0.baseAddress, $0.count) }
            if length <= 0 { continue }
            let data = Data(buffer[0..<length])
            guard let event = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else { continue }
            DispatchQueue.main.async { self.onEvent(event) }
        }
    }

    private func onEvent(_ event: [String: Any]) {
        if let loggedIn = event["logged_in"] as? [String: Any] {
            ownDeviceId = loggedIn["device"] as? String ?? ""
            busy = false
            error = ""
            if screen == .login { screen = .devices }
        } else if event["logged_out"] != nil {
            busy = false
            screen = .login
        } else if let list = event["devices"] as? [[String: Any]] {
            devices = list.map {
                Device(
                    id: $0["id"] as? String ?? "",
                    name: $0["name"] as? String ?? "",
                    online: $0["online"] as? Bool ?? false,
                    controllable: $0["controllable"] as? Bool ?? false
                )
            }
        } else if let failure = event["error"] as? [String: Any] {
            busy = false
            error = failure["message"] as? String ?? ""
        } else if let s = event["session"] as? [String: Any] {
            var state = session
            switch s["state"] as? String ?? "" {
            case "path":
                state.relayed = s["relayed"] as? Bool ?? false
                state.path = s["path"] as? String ?? ""
            case "failed":
                state.stage = "failed"
                state.message = s["message"] as? String ?? ""
            case "ended":
                state.stage = "ended"
                state.message = s["why"] as? String ?? ""
                if case .session = screen { screen = .devices }
            case let other:
                state.stage = other
            }
            session = state
        } else if event["password_needed"] != nil {
            session.passwordNeeded = true
        } else if let result = event["password_result"] as? [String: Any] {
            if result["ok"] as? Bool == true { session.passwordNeeded = false }
        } else if let text = event["clipboard"] as? String {
            // The PC's clipboard on the phone (as UU Remote shares it).
            UIPasteboard.general.string = text
        } else if let text = event["notice"] as? String {
            notice = text
        }
    }
}
