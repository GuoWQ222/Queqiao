import SwiftUI

/// Queqiao's colours, as the desktop windows have them (main_window.rs, linux/theme.rs).
enum Q {
    static let night = Color(hex: 0x141A3B)
    static let nightSelected = Color(hex: 0x263169)
    static let nightText = Color(hex: 0xD2D7EE)
    static let nightMuted = Color(hex: 0x8890B5)
    static let nightChip = Color(hex: 0x2D3874)
    static let gold = Color(hex: 0xE3B45A)
    static let onGold = Color(hex: 0x1A1F3F)
    static let text = Color(hex: 0x1B2140)
    static let secondary = Color(hex: 0x646B8C)
    static let disabled = Color(hex: 0xB4B9CC)
    static let accent = Color(hex: 0x3D5FAE)
    static let online = Color(hex: 0x2FBF71)
    static let offline = Color(hex: 0x9AA1B8)
    static let border = Color(hex: 0xCBCFDA)
    static let heroTop = Color(hex: 0x253070)
    static let heroBottom = Color(hex: 0x0E1330)
    static let heroMuted = Color(hex: 0xA9B1D6)
    static let danger = Color(hex: 0xC6423B)
    /// The being-controlled banner's pale blue (as the desktop main window's).
    static let bannerBack = Color(hex: 0xEAF2FE)
}

extension Color {
    init(hex: UInt32) {
        self.init(red: Double((hex >> 16) & 0xFF) / 255, green: Double((hex >> 8) & 0xFF) / 255,
                  blue: Double(hex & 0xFF) / 255)
    }
}

/// The gold button of the main window's card.
struct GoldButton: View {
    let title: LocalizedStringKey
    var enabled = true
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            Text(title)
                .font(.system(size: 16, weight: .bold))
                .frame(maxWidth: .infinity, minHeight: 46)
                .foregroundColor(Q.onGold)
                .background(enabled ? Q.gold : Q.disabled)
                .cornerRadius(8)
        }
        .disabled(!enabled)
    }
}

/// The night-blue band with the magpie and the name, as the desktop sidebar's top.
struct Header<Trailing: View>: View {
    let subtitle: LocalizedStringKey
    @ViewBuilder var trailing: () -> Trailing

    var body: some View {
        HStack(spacing: 10) {
            Image("Magpie").resizable().frame(width: 36, height: 36)
            VStack(alignment: .leading, spacing: 2) {
                Text("Queqiao").font(.system(size: 20, weight: .bold)).foregroundColor(Q.nightText)
                Text(subtitle).font(.system(size: 13)).foregroundColor(Q.nightMuted)
            }
            Spacer()
            trailing()
        }
        .padding(.horizontal, 20)
        .padding(.vertical, 16)
        .background(LinearGradient(colors: [Q.heroTop, Q.night], startPoint: .top, endPoint: .bottom)
            .ignoresSafeArea(edges: .top))
    }
}

/// A button of the session's tools.
struct ToolChip: View {
    let title: LocalizedStringKey
    var selected = false
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            Text(title)
                .font(.system(size: 13))
                .lineLimit(1)
                .minimumScaleFactor(0.7)
                .frame(maxWidth: .infinity, minHeight: 40)
                .foregroundColor(selected ? Q.gold : Q.nightText)
                .background(selected ? Q.nightSelected : Q.nightChip)
                .overlay(RoundedRectangle(cornerRadius: 8).stroke(selected ? Q.gold : Q.nightChip, lineWidth: 1))
                .cornerRadius(8)
        }
    }
}

extension View {
    /// "Taking control will disconnect {controller}. Do you want to continue?" with Take Control and Cancel (as UU
    /// Remote's): one device controls a PC at a time.
    func takeOverAlert(controller: String, isPresented: Binding<Bool>, onCancel: @escaping () -> Void,
                       onTakeOver: @escaping () -> Void) -> some View {
        alert("Take Control", isPresented: isPresented) {
            Button("Take Control", action: onTakeOver)
            Button("Cancel", role: .cancel, action: onCancel)
        } message: {
            Text("Taking control will disconnect \(controller). Do you want to continue?")
        }
    }
}
