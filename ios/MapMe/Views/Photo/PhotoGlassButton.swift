import SwiftUI

extension View {
    /// Floating button chrome for the photo screens: Liquid Glass on iOS 26 (prominent = tinted with
    /// the accent), a frosted material capsule with a soft shadow on iOS 17 and 18.
    @ViewBuilder
    func photoGlassButton(prominent: Bool = false) -> some View {
        #if compiler(>=6.2)
        if #available(iOS 26.0, *) {
            if prominent {
                self.buttonStyle(.glassProminent).tint(Theme.accent)
            } else {
                self.buttonStyle(.glass)
            }
        } else {
            self.buttonStyle(PhotoMaterialButtonStyle(prominent: prominent))
        }
        #else
        self.buttonStyle(PhotoMaterialButtonStyle(prominent: prominent))
        #endif
    }
}

/// The pre-iOS 26 look of `photoGlassButton`.
struct PhotoMaterialButtonStyle: ButtonStyle {
    var prominent = false

    func makeBody(configuration: Configuration) -> some View {
        PhotoMaterialButtonBody(configuration: configuration, prominent: prominent)
    }
}

private struct PhotoMaterialButtonBody: View {
    let configuration: ButtonStyleConfiguration
    let prominent: Bool
    @Environment(\.isEnabled) private var isEnabled

    var body: some View {
        configuration.label
            .padding(.horizontal, 16)
            .padding(.vertical, 10)
            .foregroundStyle(prominent ? Color.black : Color.white)
            .background {
                if prominent {
                    Capsule().fill(Theme.accent)
                } else {
                    Capsule().fill(.regularMaterial)
                }
            }
            .overlay(Capsule().stroke(Color.white.opacity(prominent ? 0.0 : 0.14), lineWidth: 1))
            .shadow(color: Color.black.opacity(0.35), radius: 8, y: 3)
            .scaleEffect(configuration.isPressed ? 0.95 : 1)
            .opacity(isEnabled ? (configuration.isPressed ? 0.85 : 1) : 0.4)
            .animation(.spring(response: 0.25, dampingFraction: 0.7), value: configuration.isPressed)
    }
}
