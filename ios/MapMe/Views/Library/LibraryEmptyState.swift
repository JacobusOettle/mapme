import SwiftUI

/// First-run hero: animated contour rings with a red dot, the pitch, and a big import button.
struct LibraryEmptyState: View {
    var onImportFile: () -> Void
    var onImportPhoto: () -> Void

    @State private var appeared = false

    var body: some View {
        ScrollView {
            VStack(spacing: 26) {
                ContourHeroView()
                    .frame(width: 270, height: 270)
                    .scaleEffect(appeared ? 1 : 0.86)
                    .opacity(appeared ? 1 : 0)

                VStack(spacing: 10) {
                    Text("Put yourself on the map")
                        .font(.system(.title, design: .rounded).weight(.bold))
                        .foregroundStyle(Theme.textPrimary)
                        .multilineTextAlignment(.center)
                    Text("Import a PDF or picture of any trail map — MapMe finds where it is and shows you on it.")
                        .font(.body)
                        .foregroundStyle(Theme.textSecondary)
                        .multilineTextAlignment(.center)
                        .fixedSize(horizontal: false, vertical: true)
                }
                .padding(.horizontal, 28)
                .opacity(appeared ? 1 : 0)
                .offset(y: appeared ? 0 : 12)

                Menu {
                    Button(action: onImportFile) {
                        Label("PDF or image from Files", systemImage: "folder")
                    }
                    Button(action: onImportPhoto) {
                        Label("Photo or screenshot", systemImage: "photo.on.rectangle.angled")
                    }
                } label: {
                    HStack(spacing: 10) {
                        Image(systemName: "square.and.arrow.down.fill")
                        Text("Import a map")
                    }
                    .font(.headline)
                    .foregroundStyle(Color.black)
                    .padding(.horizontal, 30)
                    .padding(.vertical, 16)
                    .background(Theme.accent, in: Capsule())
                    .shadow(color: Theme.accent.opacity(0.45), radius: 18, y: 6)
                }
                .accessibilityIdentifier("emptyImportButton")
                .opacity(appeared ? 1 : 0)

                HStack(alignment: .top, spacing: 10) {
                    feature("number", "Reads grid labels")
                    feature("mountain.2.fill", "Matches contours to terrain")
                    feature("figure.hiking", "Works offline on the trail")
                }
                .padding(.horizontal, 20)
                .opacity(appeared ? 1 : 0)
            }
            .padding(.vertical, 28)
            .frame(maxWidth: .infinity)
        }
        .scrollBounceBehavior(.basedOnSize)
        .onAppear {
            withAnimation(.spring(response: 0.8, dampingFraction: 0.82)) { appeared = true }
        }
    }

    private func feature(_ symbol: String, _ text: String) -> some View {
        VStack(spacing: 8) {
            Image(systemName: symbol)
                .font(.system(size: 18, weight: .semibold))
                .foregroundStyle(Theme.accent)
                .frame(width: 40, height: 40)
                .background(Theme.accent.opacity(0.12), in: Circle())
            Text(text)
                .font(.caption)
                .foregroundStyle(Theme.textSecondary)
                .multilineTextAlignment(.center)
                .fixedSize(horizontal: false, vertical: true)
        }
        .frame(maxWidth: .infinity)
        .padding(.vertical, 12)
        .padding(.horizontal, 6)
        .background(Theme.surface, in: RoundedRectangle(cornerRadius: 14, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: 14, style: .continuous).stroke(Theme.stroke, lineWidth: 1))
    }
}
