import SwiftUI
import UIKit

/// Keeping the chrome of full-screen surfaces clear of the status bar, the Dynamic Island and the
/// home indicator without relying on SwiftUI's safe area alone.
///
/// On a device the import cover came up with a zero safe area (its Cancel / Done header drawn
/// under the clock and the Wi-Fi symbol, its bottom bar on the home indicator): SwiftUI hands a
/// full-screen cover a zero safe area when it is laid out while another presentation (the photo
/// picker closing after a pick) is still on its way. The window's own `safeAreaInsets` (UIKit)
/// are right even then. So the chrome of full-screen surfaces measures where it really is in its
/// window and adds whatever part of the window's unsafe area SwiftUI's safe area left uncovered:
/// in effect max(SwiftUI's inset, the window's), re-read on rotation, resizing and once the
/// presentation it came in with has finished.
enum WindowSafeArea {
    /// The part of `frame` (window coordinates) that lies in the window's unsafe area on each
    /// edge, never more than that edge's inset. A frame that already respects the window's safe
    /// area gets zero everywhere; one that does not overlap the window at all (a cover still
    /// sliding in) gets zero too, and is measured again once it has arrived.
    static func overlap(of frame: CGRect, windowBounds: CGRect, insets: UIEdgeInsets) -> UIEdgeInsets {
        guard frame.width > 0, frame.height > 0, windowBounds.width > 0, windowBounds.height > 0,
              frame.intersects(windowBounds)
        else { return .zero }
        let safe = windowBounds.inset(by: insets)
        func clamp(_ value: CGFloat, _ limit: CGFloat) -> CGFloat {
            guard value.isFinite, limit.isFinite else { return 0 }
            return min(max(value, 0), max(limit, 0))
        }
        return UIEdgeInsets(top: clamp(safe.minY - frame.minY, insets.top),
                            left: clamp(safe.minX - frame.minX, insets.left),
                            bottom: clamp(frame.maxY - safe.maxY, insets.bottom),
                            right: clamp(frame.maxX - safe.maxX, insets.right))
    }

    /// `overlap` in SwiftUI's leading / trailing terms.
    static func edgeInsets(_ insets: UIEdgeInsets, rightToLeft: Bool) -> EdgeInsets {
        EdgeInsets(top: insets.top, leading: rightToLeft ? insets.right : insets.left,
                   bottom: insets.bottom, trailing: rightToLeft ? insets.left : insets.right)
    }
}

extension View {
    /// Pads this view on `edges` by whatever part of the window's unsafe area (status bar,
    /// Dynamic Island, home indicator, the sides in landscape) its frame still covers. Nothing
    /// when SwiftUI's safe area already keeps it clear, the full inset when SwiftUI gave it none.
    /// For the chrome container of a full-screen surface (not for backgrounds that ignore the
    /// safe area on purpose).
    func windowSafeAreaPadding(_ edges: Edge.Set = .all) -> some View {
        modifier(WindowSafeAreaPadding(edges: edges))
    }
}

private struct WindowSafeAreaPadding: ViewModifier {
    let edges: Edge.Set
    @State private var overlap = EdgeInsets()

    func body(content: Content) -> some View {
        content
            .padding(EdgeInsets(top: edges.contains(.top) ? overlap.top : 0,
                                leading: edges.contains(.leading) ? overlap.leading : 0,
                                bottom: edges.contains(.bottom) ? overlap.bottom : 0,
                                trailing: edges.contains(.trailing) ? overlap.trailing : 0))
            // Measures the padded view's own frame, which the padding does not change.
            .background {
                WindowSafeAreaProbe { overlap = $0 }
                    .allowsHitTesting(false)
                    .accessibilityHidden(true)
            }
    }
}

/// A `GeometryReader` for full-screen layouts that place their chrome by hand over content that
/// ignores the safe area (the map screen). `insets` are SwiftUI's safe-area insets raised to the
/// window's where SwiftUI's fall short; `size` is the size of the region inside them.
struct SafeAreaReader<Content: View>: View {
    @ViewBuilder var content: (_ size: CGSize, _ insets: EdgeInsets) -> Content
    @State private var overlap = EdgeInsets()

    var body: some View {
        GeometryReader { geo in
            let s = geo.safeAreaInsets
            let o = overlap
            content(CGSize(width: max(0, geo.size.width - o.leading - o.trailing),
                           height: max(0, geo.size.height - o.top - o.bottom)),
                    EdgeInsets(top: s.top + o.top, leading: s.leading + o.leading,
                               bottom: s.bottom + o.bottom, trailing: s.trailing + o.trailing))
        }
        // The reader fills SwiftUI's safe region: whatever of it is still unsafe in the window
        // is what SwiftUI's insets missed.
        .background {
            WindowSafeAreaProbe { overlap = $0 }
                .allowsHitTesting(false)
                .accessibilityHidden(true)
        }
    }
}

/// An invisible UIKit view the size of the view it backs, reporting `WindowSafeArea.overlap` of
/// its frame whenever that may have changed.
private struct WindowSafeAreaProbe: UIViewRepresentable {
    var onChange: (EdgeInsets) -> Void

    func makeUIView(context: Context) -> WindowSafeAreaProbeView {
        let view = WindowSafeAreaProbeView()
        view.onChange = onChange
        return view
    }

    func updateUIView(_ view: WindowSafeAreaProbeView, context: Context) {
        view.onChange = onChange
        view.measure()
    }
}

/// Measured when it joins a window, when its size or safe area changes (rotation, split screen),
/// on the next turn of the run loop and when the presentation it is part of has finished (its
/// final place in the window).
final class WindowSafeAreaProbeView: UIView {
    var onChange: ((EdgeInsets) -> Void)?
    private var latest = EdgeInsets()
    private var reported: EdgeInsets?
    private var reportScheduled = false

    override init(frame: CGRect) {
        super.init(frame: frame)
        isUserInteractionEnabled = false
        isAccessibilityElement = false
        backgroundColor = .clear
    }

    required init?(coder: NSCoder) {
        super.init(coder: coder)
        isUserInteractionEnabled = false
        isAccessibilityElement = false
    }

    override func didMoveToWindow() {
        super.didMoveToWindow()
        guard window != nil else { return }
        measure()
        // A presented view joins the window at its starting point (a cover below the screen);
        // measure again where it ends up.
        DispatchQueue.main.async { [weak self] in self?.measure() }
        if let coordinator = owningViewController?.transitionCoordinator {
            _ = coordinator.animate(alongsideTransition: nil) { [weak self] _ in self?.measure() }
        }
    }

    override func layoutSubviews() {
        super.layoutSubviews()
        measure()
    }

    override func safeAreaInsetsDidChange() {
        super.safeAreaInsetsDidChange()
        measure()
    }

    func measure() {
        guard let window else { return }
        let frame = convert(bounds, to: window)
        let overlap = WindowSafeArea.overlap(of: frame, windowBounds: window.bounds, insets: window.safeAreaInsets)
        report(WindowSafeArea.edgeInsets(overlap, rightToLeft: effectiveUserInterfaceLayoutDirection == .rightToLeft))
    }

    /// Hands the value to SwiftUI on the next turn of the main queue (never in the middle of a
    /// SwiftUI update), once per change.
    private func report(_ insets: EdgeInsets) {
        latest = insets
        guard !reportScheduled else { return }
        reportScheduled = true
        DispatchQueue.main.async { [weak self] in
            guard let self else { return }
            self.reportScheduled = false
            guard self.latest != self.reported else { return }
            self.reported = self.latest
            self.onChange?(self.latest)
        }
    }

    private var owningViewController: UIViewController? {
        var responder: UIResponder? = next
        while let current = responder {
            if let controller = current as? UIViewController { return controller }
            responder = current.next
        }
        return nil
    }
}
