import SwiftUI

/// Full-screen progress for `model.importJob` (import → locate), with result actions.
struct ImportProgressView: View {
    @Environment(AppModel.self) private var model

    /// Last job seen, so the screen keeps its content while the cover animates away.
    @State private var lastJob: ImportJob?
    @State private var showCalibration = false
    @State private var calibrationFinished = false

    var body: some View {
        let job = model.importJob ?? lastJob
        ZStack {
            backdrop
            if let job {
                content(for: job)
                    .padding(.horizontal, 24)
                    .padding(.vertical, 20)
                    .frame(maxWidth: 520)
            }
        }
        .animation(.spring(response: 0.5, dampingFraction: 0.85), value: job?.phase)
        .onAppear { lastJob = model.importJob }
        .onChange(of: model.importJob) { _, newValue in
            if let newValue { lastJob = newValue }
        }
        .sensoryFeedback(trigger: job?.phase) { _, newPhase in
            switch newPhase {
            case .succeeded?: return .success
            case .needsCalibration?: return .warning
            case .failed?: return .error
            default: return nil
            }
        }
        .sheet(isPresented: $showCalibration, onDismiss: {
            if calibrationFinished {
                calibrationFinished = false
                model.finishImport(openMap: true)
            }
        }) {
            if let mapID = job?.mapID {
                CalibrationView(mapID: mapID, onDone: {
                    calibrationFinished = true
                    showCalibration = false
                })
                .environment(model)
            }
        }
    }

    private var backdrop: some View {
        ZStack {
            Theme.background
            RadialGradient(colors: [Theme.accent.opacity(0.10), Theme.background.opacity(0)],
                           center: .center, startRadius: 10, endRadius: 420)
        }
        .ignoresSafeArea()
    }

    @MainActor
    @ViewBuilder
    private func content(for job: ImportJob) -> some View {
        VStack(spacing: 0) {
            Spacer(minLength: 12)
            ZStack {
                ImportScanVisual(phase: job.phase)
                    .frame(width: 250, height: 250)
                phaseBadge(job.phase)
                    .offset(x: 88, y: 88)
            }
            .padding(.bottom, 28)

            Text(job.name)
                .font(.system(.title2, design: .rounded).weight(.bold))
                .foregroundStyle(Theme.textPrimary)
                .multilineTextAlignment(.center)
                .lineLimit(2)
                .padding(.bottom, 10)

            details(for: job)
                .frame(minHeight: 96, alignment: .top)

            Spacer(minLength: 20)
            actions(for: job)
        }
    }

    @ViewBuilder
    private func phaseBadge(_ phase: ImportJob.Phase) -> some View {
        switch phase {
        case .importing, .locating:
            EmptyView()
        case .succeeded:
            badge("checkmark", Theme.good)
        case .needsCalibration:
            badge("questionmark", Theme.warn)
        case .failed:
            badge("xmark", Theme.bad)
        }
    }

    private func badge(_ symbol: String, _ color: Color) -> some View {
        Image(systemName: symbol)
            .font(.system(size: 26, weight: .heavy))
            .foregroundStyle(Color.black)
            .frame(width: 58, height: 58)
            .background(color, in: Circle())
            .overlay(Circle().stroke(Theme.background, lineWidth: 4))
            .shadow(color: color.opacity(0.6), radius: 16)
            .transition(.scale(scale: 0.3).combined(with: .opacity))
    }

    @ViewBuilder
    private func details(for job: ImportJob) -> some View {
        switch job.phase {
        case .importing, .locating:
            VStack(spacing: 14) {
                Text(job.step)
                    .font(.callout.weight(.medium))
                    .foregroundStyle(Theme.textSecondary)
                    .multilineTextAlignment(.center)
                    .contentTransition(.opacity)
                    .animation(.easeInOut(duration: 0.2), value: job.step)
                ImportProgressBar(fraction: job.fraction)
                    .frame(maxWidth: 300)
                Text("\(Int((min(max(job.fraction, 0), 1) * 100).rounded()))%")
                    .font(.caption.weight(.semibold))
                    .foregroundStyle(Theme.textSecondary)
                    .monospacedDigit()
                    .contentTransition(.numericText())
            }
        case .succeeded:
            message(job.message ?? "Found where this map is.", title: "You're on the map", color: Theme.good)
        case .needsCalibration:
            message(job.message ?? "MapMe couldn't work out where this map is.",
                    title: "Couldn't place this map", color: Theme.warn)
        case .failed:
            message(job.message ?? "Something went wrong while importing.", title: "Import failed", color: Theme.bad)
        }
    }

    private func message(_ text: String, title: String, color: Color) -> some View {
        VStack(spacing: 8) {
            Text(title)
                .font(.headline)
                .foregroundStyle(color)
            Text(text)
                .font(.callout)
                .foregroundStyle(Theme.textSecondary)
                .multilineTextAlignment(.center)
                .fixedSize(horizontal: false, vertical: true)
        }
        .transition(.opacity.combined(with: .move(edge: .bottom)))
    }

    @MainActor
    @ViewBuilder
    private func actions(for job: ImportJob) -> some View {
        switch job.phase {
        case .importing, .locating:
            Button("Cancel") { model.cancelImport() }
                .buttonStyle(ImportActionButtonStyle(kind: .secondary))
                .accessibilityIdentifier("cancelImportButton")
        case .succeeded:
            Button {
                model.finishImport(openMap: true)
            } label: {
                Label("Open map", systemImage: "map.fill")
            }
            .buttonStyle(ImportActionButtonStyle(kind: .primary))
            .accessibilityIdentifier("openMapButton")
        case .needsCalibration:
            VStack(spacing: 10) {
                if job.mapID != nil {
                    Button {
                        showCalibration = true
                    } label: {
                        Label("Calibrate manually", systemImage: "hand.tap.fill")
                    }
                    .buttonStyle(ImportActionButtonStyle(kind: .primary))
                    .accessibilityIdentifier("calibrateButton")
                }
                Button("Open map anyway") { model.finishImport(openMap: true) }
                    .buttonStyle(ImportActionButtonStyle(kind: .secondary))
                    .accessibilityIdentifier("openMapButton")
                if let mapID = job.mapID {
                    Button {
                        model.relocate(mapID: mapID)
                    } label: {
                        Label("Try again", systemImage: "arrow.clockwise")
                    }
                    .buttonStyle(ImportActionButtonStyle(kind: .plain))
                    .accessibilityIdentifier("retryLocateButton")
                }
            }
        case .failed:
            Button("Close") { model.finishImport(openMap: false) }
                .buttonStyle(ImportActionButtonStyle(kind: .secondary))
                .accessibilityIdentifier("closeImportButton")
        }
    }
}
