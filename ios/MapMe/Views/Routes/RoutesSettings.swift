import SwiftUI

/// Settings → Routes: walking pace (learned or set), off-route alerts (docs/design/explorer.md,
/// Walking time).
@MainActor
struct RoutesSettingsSection: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        @Bindable var settings = model.settings
        let learned = model.navigator.learnedPace
        Section {
            Picker(selection: $settings.usesLearnedPace) {
                Text("Learned").tag(true)
                Text("Custom").tag(false)
            } label: {
                SettingsLabel("Walking pace", symbol: "figure.hiking", color: Theme.route)
            }
            .pickerStyle(.menu)
            .accessibilityIdentifier("walkingPacePicker")
            if settings.usesLearnedPace {
                HStack {
                    Text(String(format: "%.2f× Tobler", learned?.factor ?? 1))
                        .monospacedDigit()
                    Spacer()
                    Text(learnedDetail(learned))
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                        .multilineTextAlignment(.trailing)
                }
                .accessibilityElement(children: .combine)
                .accessibilityIdentifier("learnedPaceRow")
            } else {
                VStack(alignment: .leading, spacing: 6) {
                    HStack {
                        Text(String(format: "%.2f× Tobler", settings.customPaceFactor))
                            .monospacedDigit()
                        Spacer()
                        Text("\(GeoFormat.speed(PaceEstimate(factor: settings.customPaceFactor, source: .custom).flatSpeed, units: settings.units)) on the flat")
                            .font(.footnote)
                            .foregroundStyle(.secondary)
                            .monospacedDigit()
                    }
                    Slider(value: $settings.customPaceFactor, in: PaceModel.factorRange, step: 0.05) {
                        Text("Pace factor")
                    } minimumValueLabel: {
                        Image(systemName: "hare.fill").foregroundStyle(.secondary)
                    } maximumValueLabel: {
                        Image(systemName: "tortoise.fill").foregroundStyle(.secondary)
                    }
                    .tint(Theme.route)
                    .accessibilityIdentifier("walkingPaceSlider")
                }
            }
            Toggle(isOn: $settings.offRouteAlerts) {
                SettingsLabel("Off-route alerts", symbol: "exclamationmark.triangle.fill", color: Theme.bad)
            }
            .accessibilityIdentifier("offRouteAlertsToggle")
        } header: {
            Text("Routes")
        } footer: {
            Text("Walking times use Tobler's hiking function (5 km/h on the flat, slower uphill and on steep descents) times your pace, learned from your recorded tracks: the median of the last ten with at least 1 km of walking. While you follow a route, your pace over the last 45 minutes counts too. Times are moving time: breaks are not included. Off-route alerts warn when you are more than 40 m from the route you follow for 20 seconds, with a notification when MapMe is in the background.")
        }
        .onAppear { model.navigator.refreshLearnedPace() }
        .onChange(of: settings.usesLearnedPace) { _, usesLearned in
            // A custom pace starts from the learned one.
            if !usesLearned, let learned { model.settings.customPaceFactor = learned.factor }
        }
    }

    private func learnedDetail(_ pace: PaceEstimate?) -> String {
        guard let pace, case .learned(let n) = pace.source else { return "No recorded tracks yet: Tobler's pace" }
        return "learned from \(n) track\(n == 1 ? "" : "s")"
    }
}

extension AppSettings {
    /// Settings → Routes: the pace is learned from your tracks (`walkingPaceFactor` nil).
    var usesLearnedPace: Bool {
        get { walkingPaceFactor == nil }
        set {
            guard newValue != usesLearnedPace else { return }
            walkingPaceFactor = newValue ? nil : 1
        }
    }

    /// Settings → Routes: the custom pace, in steps of 0.05 within 0.6…2.
    var customPaceFactor: Double {
        get { walkingPaceFactor ?? 1 }
        set { walkingPaceFactor = PaceModel.clamp((newValue * 20).rounded() / 20) }
    }
}
