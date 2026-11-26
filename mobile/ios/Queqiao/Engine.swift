import Foundation
import UIKit

/// One device of the account, as the list shows it.
struct Device: Identifiable, Equatable {
    let id: String
    let name: String
    let online: Bool
    let controllable: Bool
    /// Another device of the account controlling it now (UU Remote's "being controlled"), empty for none.
    var controlledBy = ""
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
    /// The relay's place ("Shanghai"; `relayZh` "上海"), empty when direct or unnamed.
    var relay = ""
    var relayZh = ""
    var message = ""
    var passwordNeeded = false
    /// "busy": another device controls the PC (empty: someone of another account helping).
    var controller = ""
    /// The person may take control from `controller` (a device of the account, not asked to already).
    var mayTakeOver = false
}

/// The session's figures, once a second (as the desktop's card).
struct Figures: Equatable {
    var fps = 0.0
    var mbps = 0.0
    var rttMs: Double?
    var relayed = false
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
    @Published var figures = Figures()
    /// The PC's screens: how many, which one is shown.
    @Published var screenCount = 1
    @Published var currentScreen = 0
    /// A help request waits for its answer.
    @Published var helping = false
    /// The user name of an account the login just made (one form for logging in and signing up), else empty.
    @Published var created = ""
    private(set) var ownDeviceId = ""
    /// Simulator tests: the device to connect to once listed, and text to type once its picture shows.
    private var testConnect = ProcessInfo.processInfo.environment["QUEQIAO_TEST_CONNECT"]
    private var testType = ProcessInfo.processInfo.environment["QUEQIAO_TEST_TYPE"]

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
        // Tests in the Simulator (.github/workflows/ios.yml): a login given as "server|pin|user|password" (simctl passes
        // it as SIMCTL_CHILD_QUEQIAO_TEST_LOGIN). Nothing sets it on a phone.
        if let test = ProcessInfo.processInfo.environment["QUEQIAO_TEST_LOGIN"] {
            let parts = test.components(separatedBy: "|")
            if parts.count == 4 {
                logIn(server: parts[0], pin: parts[1], user: parts[2], password: parts[3])
            }
        }
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

    /// A session with `device`; `takeOver`: from the device of the account controlling it (the person confirmed).
    func connect(_ device: Device, takeOver: Bool = false) {
        session = SessionState()
        figures = Figures()
        screen = .session(device)
        command(["connect": ["device": device.id, "take_over": takeOver]])
    }

    /// Ends another device's session with `device` (the being-controlled banner's Disconnect).
    func endControl(_ device: Device) { command(["end_control": device.id]) }

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

    /// Helping someone: their help ID and code (empty: the person there is asked).
    func help(id: String, code: String) {
        helping = true
        error = ""
        command(["help": ["id": id.trimmingCharacters(in: .whitespaces), "code": code.trimmingCharacters(in: .whitespaces)]])
    }

    func wake(_ device: Device) { command(["wake": device.id]) }

    func picture(fps: Int, quality: String) { command(["picture": ["fps": fps, "quality": quality]]) }

    func showScreen(_ n: Int) { command(["screen": n]) }

    func muteSpeakers(_ mute: Bool) { command(["mute_speakers": mute]) }

    /// The phone's clipboard text to the PC's clipboard; false when it holds no text.
    func sendPhoneClipboard() -> Bool {
        guard let text = UIPasteboard.general.string, !text.isEmpty else { return false }
        sendClipboard(text)
        return true
    }

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
            if loggedIn["created"] as? Bool == true { created = loggedIn["user"] as? String ?? "" }
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
                    controllable: $0["controllable"] as? Bool ?? false,
                    controlledBy: $0["controlled_by"] as? String ?? ""
                )
            }
            if let name = testConnect, screen == .devices, let device = devices.first(where: { $0.name == name && $0.online }) {
                testConnect = nil
                connect(device)
            }
        } else if let failure = event["error"] as? [String: Any] {
            busy = false
            helping = false
            error = failure["message"] as? String ?? ""
        } else if let s = event["session"] as? [String: Any] {
            var state = session
            switch s["state"] as? String ?? "" {
            case "path":
                state.relayed = s["relayed"] as? Bool ?? false
                state.path = s["path"] as? String ?? ""
                state.relay = s["relay"] as? String ?? ""
                state.relayZh = s["relay_zh"] as? String ?? ""
            case "picture":
                state.stage = "picture"
                if let text = testType {
                    testType = nil
                    DispatchQueue.main.asyncAfter(deadline: .now() + 2) { self.typeText(text) }
                }
            case "failed":
                state.stage = "failed"
                state.message = s["message"] as? String ?? ""
            case "busy":
                // Another device has the PC: the session view asks whether to take control.
                state.stage = "busy"
                state.controller = s["controller"] as? String ?? ""
                state.mayTakeOver = s["may_take_over"] as? Bool ?? false
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
        } else if let s = event["stats"] as? [String: Any] {
            figures = Figures(
                fps: s["fps"] as? Double ?? 0,
                mbps: s["mbps"] as? Double ?? 0,
                rttMs: s["rtt_ms"] as? Double,
                relayed: s["relayed"] as? Bool ?? false
            )
        } else if let s = event["screens"] as? [String: Any] {
            screenCount = max(1, (s["sizes"] as? [Any])?.count ?? 1)
            currentScreen = s["current"] as? Int ?? 0
        } else if let granted = event["help_granted"] as? [String: Any] {
            helping = false
            session = SessionState()
            figures = Figures()
            screen = .session(Device(id: granted["id"] as? String ?? "", name: granted["name"] as? String ?? "",
                                     online: true, controllable: true))
        }
    }
}
