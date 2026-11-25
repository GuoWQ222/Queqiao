import SwiftUI

/// The remote screen in SwiftUI: the UIKit view that shows the picture and takes the touches.
struct RemoteScreen: UIViewRepresentable {
    let mode: RemoteScreenView.Mode
    let keyboard: Bool
    let made: (RemoteScreenView) -> Void

    func makeUIView(context: Context) -> RemoteScreenView {
        let view = RemoteScreenView(frame: .zero)
        made(view)
        return view
    }

    func updateUIView(_ view: RemoteScreenView, context: Context) {
        if view.mode != mode { view.mode = mode }
        if view.keyboardShown != keyboard { view.keyboardShown = keyboard }
    }

    static func dismantleUIView(_ view: RemoteScreenView, coordinator: ()) {
        view.stop()
    }
}

struct SessionView: View {
    @EnvironmentObject var engine: Engine
    let device: Device
    @State private var screenView: RemoteScreenView?
    @State private var tools = false
    @State private var keyboard = false
    @State private var keysOpen = false
    @State private var mode = RemoteScreenView.Mode.touch
    @State private var muted = false
    @State private var password = ""
    @State private var audio: AudioPlayer?

    var body: some View {
        ZStack {
            Color.black.ignoresSafeArea()
            RemoteScreen(mode: mode, keyboard: keyboard) { view in
                DispatchQueue.main.async { screenView = view }
            }
            .ignoresSafeArea()
            if engine.session.stage != "picture" {
                VStack(spacing: 12) {
                    if engine.session.stage != "failed" { ProgressView().tint(Q.gold) }
                    Text(stageText).font(.system(size: 15)).foregroundColor(Q.nightText)
                        .multilineTextAlignment(.center)
                        .accessibilityIdentifier("stage")
                    if engine.session.stage == "failed" {
                        GoldButton(title: "Back") { engine.disconnect() }.frame(width: 200)
                    }
                }
                .padding(24)
            }
            // The floating ball (as UU Remote's): opens the session's tools.
            HStack {
                Spacer()
                Button { tools.toggle() } label: {
                    Image("Magpie").resizable().frame(width: 26, height: 26)
                        .frame(width: 44, height: 44)
                        .background(Q.night.opacity(0.78))
                        .clipShape(Circle())
                        .overlay(Circle().stroke(Q.gold, lineWidth: 1.5))
                }
                .accessibilityIdentifier("ball")
                .padding(.trailing, 10)
            }
            if tools {
                VStack {
                    Spacer()
                    toolPanel
                }
                .ignoresSafeArea(.keyboard)
            }
        }
        .statusBarHidden(true)
        .onAppear {
            UIApplication.shared.isIdleTimerDisabled = true
            audio = AudioPlayer()
        }
        .onDisappear {
            UIApplication.shared.isIdleTimerDisabled = false
            audio?.stop()
            audio = nil
        }
        .alert("The PC is locked", isPresented: Binding(
            get: { engine.session.passwordNeeded }, set: { _ in })) {
            SecureField("Password", text: $password)
            Button("Unlock") {
                engine.password(password)
                password = ""
            }
            Button("Disconnect", role: .cancel) { engine.disconnect() }
        } message: {
            Text("Type the PC's Windows password to unlock it.")
        }
    }

    private var stageText: String {
        switch engine.session.stage {
        case "failed":
            return String(format: NSLocalizedString("Could not connect: %@", comment: ""), engine.session.message)
        case "connected":
            return NSLocalizedString("Connected, waiting for the picture…", comment: "")
        default:
            return String(format: NSLocalizedString("Connecting to %@…", comment: ""), device.name)
        }
    }

    private var toolPanel: some View {
        VStack(spacing: 12) {
            HStack {
                Text(engine.session.relayed ? "Through the server (relayed)" : "Direct connection")
                    .font(.system(size: 13)).foregroundColor(Q.nightMuted)
                Spacer()
                Button { tools = false } label: {
                    Image(systemName: "xmark").foregroundColor(Q.nightText).padding(4)
                }
            }
            HStack(spacing: 8) {
                ToolChip(title: "Touch", selected: mode == .touch) { mode = .touch }
                ToolChip(title: "Mouse", selected: mode == .mouse) { mode = .mouse }
            }
            HStack(spacing: 8) {
                ToolChip(title: "Keyboard", selected: keyboard) {
                    keyboard.toggle()
                    tools = false
                }
                .accessibilityIdentifier("keyboard")
                ToolChip(title: "Keys", selected: keysOpen) { keysOpen.toggle() }
                ToolChip(title: muted ? "Sound off" : "Sound on", selected: !muted) {
                    muted.toggle()
                    audio?.muted = muted
                }
            }
            if keysOpen {
                let keys: [(String, [Int])] = [
                    ("Esc", [Keys.esc]), ("Tab", [Keys.tab]), ("Win", [Keys.win]),
                    ("Ctrl+C", [Keys.ctrl, Keys.c]), ("Ctrl+V", [Keys.ctrl, Keys.v]),
                    ("Ctrl+Z", [Keys.ctrl, Keys.z]), ("Alt+Tab", [Keys.alt, Keys.tab]),
                    ("Alt+F4", [Keys.alt, Keys.f[3]]), ("Win+D", [Keys.win, Keys.d]), ("←", [Keys.left]),
                    ("↑", [Keys.up]), ("↓", [Keys.down]), ("→", [Keys.right]), ("Del", [Keys.delete]),
                    ("F5", [Keys.f[4]]),
                ]
                ForEach(0..<3, id: \.self) { row in
                    HStack(spacing: 6) {
                        ForEach(0..<5, id: \.self) { column in
                            let key = keys[row * 5 + column]
                            ToolChip(title: LocalizedStringKey(key.0)) { screenView?.combo(key.1) }
                        }
                    }
                }
            }
            HStack(spacing: 8) {
                ToolChip(title: "Ctrl+Alt+Del") { engine.remote("ctrl_alt_del") }
                ToolChip(title: "Lock") { engine.remote("lock") }
                ToolChip(title: "Privacy screen") { engine.remote("privacy_on") }
            }
            Button { engine.disconnect() } label: {
                Text("Disconnect").font(.system(size: 16, weight: .bold)).foregroundColor(.white)
                    .frame(maxWidth: .infinity, minHeight: 44)
                    .background(Q.danger).cornerRadius(8)
            }
            .accessibilityIdentifier("disconnect")
        }
        .padding(16)
        .background(Q.night)
        .cornerRadius(16)
    }
}
