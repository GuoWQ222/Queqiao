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
                    Text("Log in with your Queqiao account to see your devices.")
                        .font(.system(size: 14)).foregroundColor(Q.secondary)
                    field("Account", text: $user, id: "user")
                    SecureField("Password", text: $password)
                        .textFieldStyle(.roundedBorder)
                        .accessibilityIdentifier("password")
                    if showServer {
                        field("Server", text: $server, id: "server", keyboard: .URL)
                        field("Server key (SHA-256)", text: $pin, id: "pin")
                    } else {
                        Button("Another server…") { showServer = true }
                            .font(.system(size: 14)).foregroundColor(Q.accent)
                    }
                    if !engine.error.isEmpty {
                        Text(engine.error).font(.system(size: 14)).foregroundColor(Q.danger)
                            .accessibilityIdentifier("error")
                    }
                    GoldButton(title: "Log in",
                               enabled: !engine.busy && !user.isEmpty && !password.isEmpty && !server.isEmpty) {
                        engine.logIn(server: server, pin: pin, user: user, password: password)
                    }
                    .accessibilityIdentifier("login")
                    if engine.busy {
                        HStack { Spacer(); ProgressView(); Spacer() }
                    }
                }
                .padding(24)
            }
        }
        .background(Color.white)
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
            if others.isEmpty {
                Spacer()
                Text("No other devices on this account yet. Install Queqiao on a PC and log in there with this account.")
                    .font(.system(size: 15)).foregroundColor(Q.secondary).multilineTextAlignment(.center).padding(32)
                Spacer()
            } else {
                ScrollView {
                    LazyVStack(spacing: 12) {
                        ForEach(others) { device in
                            DeviceCard(device: device, onConnect: { engine.connect(device) },
                                       onRemove: { removing = device })
                        }
                    }
                    .padding(16)
                }
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
    let onRemove: () -> Void

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
            GoldButton(title: "Connect", enabled: device.online && device.controllable, action: onConnect)
                .padding(.top, 10)
                .accessibilityIdentifier("connect:\(device.name)")
        }
        .padding(16)
        .background(LinearGradient(colors: [Q.heroTop, Q.heroBottom], startPoint: .top, endPoint: .bottom))
        .cornerRadius(12)
    }
}
