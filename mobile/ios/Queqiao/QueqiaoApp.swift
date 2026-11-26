import SwiftUI

@main
struct QueqiaoApp: App {
    @StateObject private var engine = Engine.shared

    var body: some Scene {
        WindowGroup {
            Group {
                switch engine.screen {
                case .login: LoginView()
                case .devices: DevicesView()
                case .session(let device): SessionView(device: device)
                }
            }
            .environmentObject(engine)
            .preferredColorScheme(.light)
        }
    }
}

struct LoginView: View {
    @EnvironmentObject var engine: Engine
    @State private var server = Bundle.main.object(forInfoDictionaryKey: "QueqiaoServer") as? String ?? ""
    @State private var pin = Bundle.main.object(forInfoDictionaryKey: "QueqiaoPin") as? String ?? ""
    @State private var user = ""
    @State private var password = ""
    @State private var showServer = (Bundle.main.object(forInfoDictionaryKey: "QueqiaoServer") as? String ?? "").isEmpty

    var body: some View {
        VStack(spacing: 0) {
            Header(subtitle: "Remote control of your own devices") { EmptyView() }
            ScrollView {
                VStack(alignment: .leading, spacing: 14) {
                    Text("Log in").font(.system(size: 22, weight: .bold)).foregroundColor(Q.text)
                    // One form for logging in and signing up (the user, 2026-11-26): a new name makes the account.
                    Text("Log in, or sign up with a new user name.")
                        .font(.system(size: 14)).foregroundColor(Q.secondary)
                    field("Account", text: $user, id: "user")
                    // The keyboard's Go key logs in (the button may sit under the keyboard).
                    SecureField("Password", text: $password)
                        .textFieldStyle(.roundedBorder)
                        .submitLabel(.go)
                        .onSubmit(logIn)
                        .accessibilityIdentifier("password")
                    if showServer {
                        field("Server", text: $server, id: "server", keyboard: .URL)
                        field("Server key (SHA-256)", text: $pin, id: "pin")
                            .submitLabel(.go)
                            .onSubmit(logIn)
                    } else {
                        Button("Another server…") { showServer = true }
                            .font(.system(size: 14)).foregroundColor(Q.accent)
                    }
                    if !engine.error.isEmpty {
                        Text(engine.error).font(.system(size: 14)).foregroundColor(Q.danger)
                            .accessibilityIdentifier("error")
                    }
                    GoldButton(title: "Log in", enabled: ready, action: logIn)
                    .accessibilityIdentifier("login")
                    if engine.busy {
                        HStack { Spacer(); ProgressView(); Spacer() }
                    }
                }
                .padding(24)
            }
            Text("© 2026 GuoWQ222. All rights reserved.")
                .font(.system(size: 12)).foregroundColor(Q.secondary)
                .frame(maxWidth: .infinity)
                .padding(.bottom, 12)
        }
        .background(Color.white)
    }

    private var ready: Bool { !engine.busy && !user.isEmpty && !password.isEmpty && !server.isEmpty }

    private func logIn() {
        if ready { engine.logIn(server: server, pin: pin, user: user, password: password) }
    }

    private func field(_ title: LocalizedStringKey, text: Binding<String>, id: String,
                       keyboard: UIKeyboardType = .default) -> some View {
        TextField(title, text: text)
            .textFieldStyle(.roundedBorder)
            .keyboardType(keyboard)
            .textInputAutocapitalization(.never)
            .disableAutocorrection(true)
            .accessibilityIdentifier(id)
    }
}

struct DevicesView: View {
    @EnvironmentObject var engine: Engine
    @State private var removing: Device?
    /// Connect on a PC another device of the account controls: asked first (as UU Remote's Take Control).
    @State private var takingOver: Device?

    var body: some View {
        VStack(spacing: 0) {
            Header(subtitle: "My devices") {
                Menu {
                    Button("Log out") { engine.logOut() }
                } label: {
                    Image(systemName: "ellipsis.circle").font(.system(size: 22)).foregroundColor(Q.nightText)
                }
                .accessibilityIdentifier("menu")
            }
            if !engine.error.isEmpty {
                Text(engine.error).font(.system(size: 13)).foregroundColor(Q.danger).padding(.horizontal, 20)
                    .padding(.vertical, 8)
            }
            let others = engine.devices.filter { $0.id != engine.ownDeviceId }
            ScrollView {
                LazyVStack(spacing: 12) {
                    if others.isEmpty {
                        Text("No other devices on this account yet. Install Queqiao on a PC and log in there with this account.")
                            .font(.system(size: 15)).foregroundColor(Q.secondary).multilineTextAlignment(.center)
                            .padding(.vertical, 24)
                    }
                    ForEach(others) { device in
                        DeviceCard(device: device,
                                   onConnect: {
                                       if device.controlledBy.isEmpty { engine.connect(device) } else { takingOver = device }
                                   },
                                   onWake: { engine.wake(device) }, onRemove: { removing = device },
                                   onEndControl: { engine.endControl(device) })
                    }
                    HelpCard()
                }
                .padding(16)
            }
            .takeOverAlert(
                controller: takingOver?.controlledBy ?? "",
                isPresented: Binding(get: { takingOver != nil }, set: { if !$0 { takingOver = nil } }),
                onCancel: { takingOver = nil },
                onTakeOver: {
                    if let device = takingOver { engine.connect(device, takeOver: true) }
                    takingOver = nil
                })
            // The account this login made (a name typed wrong makes one too): said once.
            .alert("New account", isPresented: Binding(get: { !engine.created.isEmpty },
                                                       set: { if !$0 { engine.created = "" } })) {
                Button("OK") { engine.created = "" }
            } message: {
                Text("A new account \"\(engine.created)\" was made with this password, and you are logged in. If you meant an account you already have, log out and check the user name.")
            }
        }
        .background(Color.white)
        .alert(item: $removing) { device in
            Alert(
                title: Text("Remove \(device.name)?"),
                message: Text("It leaves this account. To add it back, log in on it again."),
                primaryButton: .destructive(Text("Remove")) { engine.remove(device) },
                secondaryButton: .cancel(Text("Cancel")))
        }
    }
}

struct DeviceCard: View {
    let device: Device
    let onConnect: () -> Void
    let onWake: () -> Void
    let onRemove: () -> Void
    let onEndControl: () -> Void
    @State private var wakeAsked = false

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack(spacing: 8) {
                Circle().fill(device.online ? Q.online : Q.offline).frame(width: 10, height: 10)
                Text(device.name).font(.system(size: 18, weight: .bold)).foregroundColor(.white).lineLimit(1)
                Spacer()
                Menu {
                    Button("Remove from this account", role: .destructive, action: onRemove)
                } label: {
                    Image(systemName: "ellipsis").font(.system(size: 20)).foregroundColor(Q.heroMuted).padding(8)
                }
            }
            Text(device.online ? "Online" : "Offline").font(.system(size: 13)).foregroundColor(Q.heroMuted)
            if !device.controlledBy.isEmpty {
                // The being-controlled banner (as UU Remote's): who controls it, and Disconnect.
                HStack {
                    VStack(alignment: .leading, spacing: 2) {
                        Text("Being controlled").font(.system(size: 14, weight: .bold)).foregroundColor(Q.accent)
                        Text("\(device.controlledBy) is controlling it").font(.system(size: 13)).foregroundColor(Q.text)
                            .lineLimit(2)
                    }
                    Spacer()
                    Button("Disconnect", action: onEndControl).font(.system(size: 13)).foregroundColor(Q.accent)
                        .accessibilityIdentifier("end_control:\(device.name)")
                }
                .padding(.horizontal, 12).padding(.vertical, 8)
                .background(Q.bannerBack)
                .cornerRadius(8)
                .padding(.top, 8)
                .accessibilityIdentifier("controlled:\(device.name)")
            }
            if device.online {
                GoldButton(title: "Connect", enabled: device.controllable, action: onConnect)
                    .padding(.top, 10)
                    .accessibilityIdentifier("connect:\(device.name)")
            } else {
                // Offline: woken up through another device of the account on its network (Wake-on-LAN).
                GoldButton(title: wakeAsked ? "Waking it up…" : "Wake up", enabled: !wakeAsked) {
                    wakeAsked = true
                    onWake()
                }
                .padding(.top, 10)
            }
        }
        .padding(16)
        .background(LinearGradient(colors: [Q.heroTop, Q.heroBottom], startPoint: .top, endPoint: .bottom))
        .cornerRadius(12)
    }
}

/// Help someone (as UU Remote's): the help ID and code the other PC shows.
struct HelpCard: View {
    @EnvironmentObject var engine: Engine
    @State private var id = ""
    @State private var code = ""

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text("Help someone").font(.system(size: 17, weight: .bold)).foregroundColor(Q.text)
            Text("Type the help ID and code their Queqiao shows. Without a code, they are asked.")
                .font(.system(size: 13)).foregroundColor(Q.secondary)
            TextField("Help ID", text: $id)
                .textFieldStyle(.roundedBorder)
                .keyboardType(.numberPad)
                .accessibilityIdentifier("help_id")
            TextField("Help code", text: $code)
                .textFieldStyle(.roundedBorder)
                .textInputAutocapitalization(.characters)
                .disableAutocorrection(true)
                .accessibilityIdentifier("help_code")
            GoldButton(title: engine.helping ? "Waiting for the answer…" : "Connect",
                       enabled: !engine.helping && !id.isEmpty) {
                engine.help(id: id, code: code)
            }
            .accessibilityIdentifier("help_connect")
        }
        .padding(16)
        .background(Color(hex: 0xF5F6FA))
        .cornerRadius(12)
    }
}
