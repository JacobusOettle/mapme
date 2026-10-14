import SwiftUI
import UIKit

/// "Adjust Corners" for a photo of a paper map, in the style of the system document scanner: the
/// photo on black with everything outside the sheet dimmed, a white outline with four round
/// handles, and a magnifier while a corner is dragged. Done hands the corners (photo pixels,
/// TL, TR, BR, BL) to the import, which flattens the sheet with them.
struct CornerEditorView: View {
    let photo: PendingPhoto
    var onCancel: () -> Void
    var onDone: (PhotoQuad) -> Void
    /// Page detection for "Auto" when nothing was found at import.
    var detect: () async -> PhotoQuad?

    /// Photo pixels, TL, TR, BR, BL.
    @State private var corners: [CGPoint]
    @State private var detected: PhotoQuad?
    @State private var activeCorner: Int?
    /// View position of the dragged handle when the drag began.
    @State private var dragOrigin: CGPoint?
    @State private var detecting = false
    @State private var notice: String?
    @State private var appeared = false
    @State private var quarterTurns = 0
    @State private var grabCount = 0
    @State private var snapCount = 0
    @State private var failCount = 0

    static let canvasInset: CGFloat = 22
    static let handleSize: CGFloat = 28
    static let handleHitSize: CGFloat = 64
    static let loupeSize: CGFloat = 124
    static let loupeZoom: CGFloat = 2.5
    static let loupeLift: CGFloat = 112
    private static let space = "cornerCanvas"
    private static let cornerNames = ["Top-left corner", "Top-right corner", "Bottom-right corner", "Bottom-left corner"]

    init(photo: PendingPhoto, onCancel: @escaping () -> Void, onDone: @escaping (PhotoQuad) -> Void,
         detect: @escaping () async -> PhotoQuad?) {
        self.photo = photo
        self.onCancel = onCancel
        self.onDone = onDone
        self.detect = detect
        _corners = State(initialValue: photo.initial.points)
        _detected = State(initialValue: photo.detected)
    }

    var body: some View {
        ZStack {
            Color.black.ignoresSafeArea()
            VStack(spacing: 0) {
                header
                    .padding(.horizontal, 16)
                    .padding(.top, 6)
                GeometryReader { geo in
                    canvas(size: geo.size)
                }
                .padding(.vertical, 6)
                hint
                    .padding(.horizontal, 28)
                    .padding(.bottom, 10)
                actionBar
                    .padding(.horizontal, 16)
                    .padding(.bottom, 10)
            }
            // Clear of the status bar and the home indicator even when the cover was laid out
            // without a safe area (it came up while the photo picker was still closing).
            .windowSafeAreaPadding()
        }
        .preferredColorScheme(.dark)
        .sensoryFeedback(.selection, trigger: grabCount)
        .sensoryFeedback(.impact(weight: .light), trigger: snapCount)
        .sensoryFeedback(.warning, trigger: failCount)
        .onAppear {
            withAnimation(.spring(response: 0.55, dampingFraction: 0.78).delay(0.15)) { appeared = true }
        }
    }

    // MARK: - Chrome

    /// Cancel, the title and Done. The two buttons sit in equally wide side columns, so the title
    /// stays centred; they keep their full size (large controls: at least 44 pt tall) and the
    /// photo's name gives way first on a narrow screen.
    private var header: some View {
        HStack(spacing: 8) {
            Button(action: onCancel) {
                Text("Cancel")
                    .foregroundStyle(Color.white)
            }
            .photoGlassButton()
            .fixedSize()
            .accessibilityIdentifier("cornersCancelButton")
            .frame(maxWidth: .infinity, alignment: .leading)
            VStack(spacing: 2) {
                Text("Adjust Corners")
                    .font(.headline)
                    .foregroundStyle(Color.white)
                    .lineLimit(1)
                    .accessibilityAddTraits(.isHeader)
                Text(photo.name)
                    .font(.caption)
                    .foregroundStyle(Color.white.opacity(0.6))
                    .lineLimit(1)
                    .truncationMode(.middle)
            }
            .layoutPriority(1)
            Button(action: finish) {
                Text("Done")
                    .fontWeight(.semibold)
                    .foregroundStyle(Color.black)
            }
            .photoGlassButton(prominent: true)
            .fixedSize()
            .disabled(!isValid)
            .accessibilityIdentifier("cornersDoneButton")
            .frame(maxWidth: .infinity, alignment: .trailing)
        }
        .controlSize(.large)
        .font(.body)
    }

    private var hint: some View {
        Text(hintText)
            .font(.footnote)
            .foregroundStyle(isValid || activeCorner != nil ? Color.white.opacity(0.72) : Theme.warn)
            .multilineTextAlignment(.center)
            .frame(maxWidth: .infinity, minHeight: 36)
            .contentTransition(.opacity)
            .animation(.easeInOut(duration: 0.2), value: hintText)
            .accessibilityIdentifier("cornersHint")
    }

    private var hintText: String {
        if let notice { return notice }
        if activeCorner != nil { return "Line the handle up with the corner of the paper." }
        if !isValid { return "The corners must outline the four-sided sheet." }
        return "Drag the corners to the corners of the map sheet. MapMe flattens it before reading it."
    }

    private var autoTitle: String { detecting ? "Finding…" : "Auto" }

    /// Bottom buttons (not named `toolbar`, which would overload View's `toolbar(...)` modifiers).
    private var actionBar: some View {
        HStack(spacing: 10) {
            Button(action: autoDetect) {
                Label(autoTitle, systemImage: "wand.and.stars")
                    .foregroundStyle(Color.white)
            }
            .photoGlassButton()
            .disabled(detecting)
            .accessibilityIdentifier("cornersAutoButton")
            Button {
                setCorners(PhotoQuad.full(photo.imageSize))
            } label: {
                Label("Full Image", systemImage: "arrow.up.left.and.arrow.down.right")
                    .foregroundStyle(Color.white)
            }
            .photoGlassButton()
            .accessibilityIdentifier("cornersFullImageButton")
            Button(action: rotate) {
                Label("Rotate", systemImage: "rotate.right")
                    .foregroundStyle(Color.white)
            }
            .photoGlassButton()
            .accessibilityIdentifier("cornersRotateButton")
        }
        .labelStyle(.titleAndIcon)
        .font(.footnote.weight(.semibold))
        .lineLimit(1)
    }

    // MARK: - Canvas

    @ViewBuilder
    private func canvas(size: CGSize) -> some View {
        let bounds = CGRect(origin: .zero, size: size).insetBy(dx: Self.canvasInset, dy: Self.canvasInset)
        let fit = Self.fitRect(image: photo.imageSize, in: bounds)
        let points = corners.map { Self.viewPoint($0, imageSize: photo.imageSize, fit: fit) }
        ZStack(alignment: .topLeading) {
            Image(uiImage: photo.preview)
                .resizable()
                .interpolation(.high)
                .frame(width: fit.width, height: fit.height)
                .offset(x: fit.minX, y: fit.minY)
                .accessibilityLabel("Photo of the map")
            QuadMaskShape(quad: QuadVector(points))
                .fill(Color.black.opacity(0.58), style: FillStyle(eoFill: true))
                .allowsHitTesting(false)
            QuadOutlineShape(quad: QuadVector(points))
                .stroke(Color.white, style: StrokeStyle(lineWidth: 2, lineJoin: .round))
                .shadow(color: Color.black.opacity(0.45), radius: 3)
                .opacity(appeared ? 1 : 0)
                .allowsHitTesting(false)
            topMarker(points)
            ForEach(0..<4, id: \.self) { index in
                handle(index, at: points[index], fit: fit)
            }
            if let index = activeCorner {
                loupe(at: points[index], fit: fit, canvas: size, points: points)
            }
        }
        .frame(width: size.width, height: size.height, alignment: .topLeading)
        .coordinateSpace(.named(Self.space))
        .animation(.spring(response: 0.25, dampingFraction: 0.8), value: activeCorner)
    }

    private func handle(_ index: Int, at point: CGPoint, fit: CGRect) -> some View {
        let active = activeCorner == index
        return ZStack {
            Circle().fill(.ultraThinMaterial)
            Circle().fill(Color.white.opacity(active ? 0.5 : 0.22))
            Circle().stroke(Color.white, lineWidth: 3)
        }
        .frame(width: Self.handleSize, height: Self.handleSize)
        .scaleEffect(active ? 1.3 : (appeared ? 1 : 0.3))
        .animation(.spring(response: 0.3, dampingFraction: 0.65), value: active)
        .shadow(color: Color.black.opacity(0.55), radius: 6, y: 2)
        .frame(width: Self.handleHitSize, height: Self.handleHitSize)
        .contentShape(Circle())
        .gesture(drag(index, fit: fit))
        .position(point)
        .accessibilityElement()
        .accessibilityLabel(Self.cornerNames[index])
        .accessibilityIdentifier("cornerHandle\(index)")
    }

    private func drag(_ index: Int, fit: CGRect) -> some Gesture {
        DragGesture(minimumDistance: 0, coordinateSpace: .named(Self.space))
            .onChanged { value in
                if activeCorner == nil {
                    activeCorner = index
                    dragOrigin = Self.viewPoint(corners[index], imageSize: photo.imageSize, fit: fit)
                    notice = nil
                    grabCount += 1
                }
                guard activeCorner == index, let origin = dragOrigin else { return }
                let target = CGPoint(x: origin.x + value.translation.width, y: origin.y + value.translation.height)
                corners[index] = Self.imagePoint(target, imageSize: photo.imageSize, fit: fit)
            }
            .onEnded { _ in
                guard activeCorner == index else { return }
                activeCorner = nil
                dragOrigin = nil
                tidyCorners()
            }
    }

    /// Magnified view of the photo around the dragged corner, lifted above the finger.
    private func loupe(at point: CGPoint, fit: CGRect, canvas: CGSize, points: [CGPoint]) -> some View {
        let d = Self.loupeSize
        let zoom = Self.loupeZoom
        var center = CGPoint(x: point.x, y: point.y - Self.loupeLift)
        if center.y - d / 2 < 0 { center.y = point.y + Self.loupeLift }
        center.x = min(max(center.x, d / 2), max(d / 2, canvas.width - d / 2))
        let zoomed = points.map { CGPoint(x: d / 2 + ($0.x - point.x) * zoom, y: d / 2 + ($0.y - point.y) * zoom) }
        return ZStack(alignment: .topLeading) {
            Color.black
            Image(uiImage: photo.preview)
                .resizable()
                .interpolation(.high)
                .frame(width: fit.width * zoom, height: fit.height * zoom)
                .offset(x: d / 2 - (point.x - fit.minX) * zoom, y: d / 2 - (point.y - fit.minY) * zoom)
                .frame(width: d, height: d, alignment: .topLeading)
            QuadOutlineShape(quad: QuadVector(zoomed))
                .stroke(Color.white.opacity(0.9), lineWidth: 1.5)
            Circle()
                .stroke(Theme.accent, lineWidth: 2)
                .frame(width: 12, height: 12)
                .position(x: d / 2, y: d / 2)
        }
        .frame(width: d, height: d)
        .clipShape(Circle())
        .overlay(Circle().stroke(Color.white, lineWidth: 3))
        .shadow(color: Color.black.opacity(0.5), radius: 12, y: 5)
        .position(center)
        .allowsHitTesting(false)
        .transition(.scale(scale: 0.5).combined(with: .opacity))
        .accessibilityHidden(true)
    }

    /// Small arrow inside the edge that becomes the top of the flattened map.
    private func topMarker(_ points: [CGPoint]) -> some View {
        let a = points[0]
        let b = points[1]
        let mid = CGPoint(x: (a.x + b.x) / 2, y: (a.y + b.y) / 2)
        let cx = points.reduce(0) { $0 + $1.x } / 4
        let cy = points.reduce(0) { $0 + $1.y } / 4
        let dx = cx - mid.x
        let dy = cy - mid.y
        let length = max(1, (dx * dx + dy * dy).squareRoot())
        let spot = CGPoint(x: mid.x + dx / length * 26, y: mid.y + dy / length * 26)
        let angle = atan2(Double(b.y - a.y), Double(b.x - a.x))
        return Image(systemName: "arrow.up")
            .font(.system(size: 12, weight: .bold))
            .foregroundStyle(Color.white)
            .frame(width: 26, height: 26)
            .background(.ultraThinMaterial, in: Circle())
            .rotationEffect(.radians(angle))
            .position(spot)
            .animation(.spring(response: 0.45, dampingFraction: 0.8), value: quarterTurns)
            .opacity(appeared && activeCorner == nil ? 1 : 0)
            .allowsHitTesting(false)
            .accessibilityHidden(true)
    }

    // MARK: - Actions

    private var quad: PhotoQuad? { PhotoQuad(points: corners) }

    private var isValid: Bool {
        guard let quad else { return false }
        let area = Double(photo.imageSize.width * photo.imageSize.height)
        return quad.isConvexClockwise && quad.area >= 0.01 * area
    }

    private func finish() {
        guard let quad, isValid else {
            failCount += 1
            return
        }
        onDone(quad)
    }

    private func setCorners(_ quad: PhotoQuad) {
        snapCount += 1
        notice = nil
        withAnimation(.spring(response: 0.45, dampingFraction: 0.82)) {
            corners = quad.points
        }
    }

    /// Turn the result a quarter turn: the same outline, the next edge becomes the top.
    private func rotate() {
        guard let quad else { return }
        snapCount += 1
        corners = quad.rotatedClockwise.points
        quarterTurns += 1
    }

    private func autoDetect() {
        if let detected {
            setCorners(detected)
            return
        }
        detecting = true
        Task { @MainActor in
            let found = await detect()
            detecting = false
            if let found {
                detected = found
                setCorners(found)
            } else {
                failCount += 1
                notice = "No sheet edges found. Drag the corners by hand."
            }
        }
    }

    /// Corners dragged past each other: put them back in TL, TR, BR, BL order.
    private func tidyCorners() {
        guard let quad, !quad.isConvexClockwise,
              let ordered = PhotoQuad.ordered(corners), ordered.isConvexClockwise
        else { return }
        withAnimation(.spring(response: 0.35, dampingFraction: 0.8)) {
            corners = ordered.points
        }
    }

    // MARK: - Geometry (pure)

    /// The image's aspect-fit rectangle inside `bounds`.
    nonisolated static func fitRect(image: CGSize, in bounds: CGRect) -> CGRect {
        guard image.width > 0, image.height > 0, bounds.width > 0, bounds.height > 0 else { return bounds }
        let s = min(bounds.width / image.width, bounds.height / image.height)
        let w = image.width * s
        let h = image.height * s
        return CGRect(x: bounds.midX - w / 2, y: bounds.midY - h / 2, width: w, height: h)
    }

    /// Photo pixel → view point.
    nonisolated static func viewPoint(_ p: CGPoint, imageSize: CGSize, fit: CGRect) -> CGPoint {
        guard imageSize.width > 0, imageSize.height > 0 else { return fit.origin }
        return CGPoint(x: fit.minX + p.x / imageSize.width * fit.width,
                       y: fit.minY + p.y / imageSize.height * fit.height)
    }

    /// View point → photo pixel, kept inside the photo.
    nonisolated static func imagePoint(_ p: CGPoint, imageSize: CGSize, fit: CGRect) -> CGPoint {
        guard fit.width > 0, fit.height > 0 else { return .zero }
        let x = (p.x - fit.minX) / fit.width * imageSize.width
        let y = (p.y - fit.minY) / fit.height * imageSize.height
        return CGPoint(x: min(max(x, 0), imageSize.width), y: min(max(y, 0), imageSize.height))
    }
}

/// Four corners as one animatable value, so outlines morph smoothly with the handles.
struct QuadVector: VectorArithmetic {
    var values: SIMD8<Double>

    init(values: SIMD8<Double>) {
        self.values = values
    }

    init(_ points: [CGPoint]) {
        var v = SIMD8<Double>()
        for i in 0..<min(4, points.count) {
            v[2 * i] = Double(points[i].x)
            v[2 * i + 1] = Double(points[i].y)
        }
        values = v
    }

    var points: [CGPoint] {
        (0..<4).map { CGPoint(x: values[2 * $0], y: values[2 * $0 + 1]) }
    }

    static var zero: QuadVector { QuadVector(values: SIMD8<Double>()) }

    static func + (lhs: QuadVector, rhs: QuadVector) -> QuadVector { QuadVector(values: lhs.values + rhs.values) }
    static func - (lhs: QuadVector, rhs: QuadVector) -> QuadVector { QuadVector(values: lhs.values - rhs.values) }
    static func += (lhs: inout QuadVector, rhs: QuadVector) { lhs.values += rhs.values }
    static func -= (lhs: inout QuadVector, rhs: QuadVector) { lhs.values -= rhs.values }

    mutating func scale(by rhs: Double) { values *= rhs }

    var magnitudeSquared: Double { (values * values).sum() }
}

/// Everything except the quad (fill with the even-odd rule to dim the outside).
struct QuadMaskShape: Shape {
    var quad: QuadVector

    var animatableData: QuadVector {
        get { quad }
        set { quad = newValue }
    }

    func path(in rect: CGRect) -> Path {
        var path = Path()
        path.addRect(rect)
        path.addLines(quad.points)
        path.closeSubpath()
        return path
    }
}

/// The quad's outline.
struct QuadOutlineShape: Shape {
    var quad: QuadVector

    var animatableData: QuadVector {
        get { quad }
        set { quad = newValue }
    }

    func path(in rect: CGRect) -> Path {
        var path = Path()
        path.addLines(quad.points)
        path.closeSubpath()
        return path
    }
}
