import SwiftUI
import XCTest
@testable import MapMe

/// How the explorer features fit together on one screen and one terrain store
/// (docs/design/explorer.md, Map screen and Terrain): the map screen's floating controls on the
/// smallest and largest iPhones, and terrain arriving for every view drawn without it.
final class ExplorerIntegrationTests: XCTestCase {
    private let button = CGSize(width: 44, height: 44)

    // MARK: - Side column

    func testSideColumnStacksWhileItFits() {
        let sizes = Array(repeating: button, count: 5)
        // Five buttons need 5 × 44 + 4 × 10 = 260 pt.
        XCTAssertEqual(SideColumnLayout.columns(sizes, maxHeight: 260, spacing: 10), [[0, 1, 2, 3, 4]])
        XCTAssertEqual(SideColumnLayout.columns(sizes, maxHeight: 259, spacing: 10), [[0, 1, 2, 3], [4]])
        XCTAssertEqual(SideColumnLayout.columns(sizes, maxHeight: 100, spacing: 10), [[0, 1], [2, 3], [4]])
        // Never fewer than one button a column, however short.
        XCTAssertEqual(SideColumnLayout.columns(sizes, maxHeight: 10, spacing: 10).count, 5)
        let size = SideColumnLayout.size(of: [[0, 1, 2, 3], [4]], sizes: sizes, spacing: 10)
        XCTAssertEqual(size.width, 98)
        XCTAssertEqual(size.height, 206)
    }

    func testEmptyButtonsTakeNoRoom() {
        // The battery saver's badge while the saver is off.
        let sizes = [button, button, button, button, .zero]
        XCTAssertEqual(SideColumnLayout.columns(sizes, maxHeight: 206, spacing: 10), [[0, 1, 2, 3]])
        XCTAssertEqual(SideColumnLayout.size(of: [[0, 1, 2, 3]], sizes: sizes, spacing: 10).height, 206)
    }

    // MARK: - Map chrome on iPhone SE and Pro Max

    private func layout(_ screen: CGSize, safe: EdgeInsets, panel: CGFloat, compass: Bool = false) -> MapChromeLayout {
        // As MapScreen: the panel sits 8 pt above the bottom, or the home indicator's inset − 16.
        MapChromeLayout(screen: screen, safe: safe, bottomGap: max(8, safe.bottom - 16), bottomPanelHeight: panel,
                        compassShown: compass)
    }

    private func columns(_ l: MapChromeLayout, buttons: Int = 5) -> Int {
        SideColumnLayout.columns(Array(repeating: button, count: buttons), maxHeight: l.sideColumnMaxHeight,
                                 spacing: MapChromeLayout.spacing).count
    }

    func testSEPortraitKeepsTheColumnAboveThePanel() {
        let se = CGSize(width: 375, height: 667)
        let safe = EdgeInsets(top: 20, leading: 0, bottom: 0, trailing: 0)
        let collapsed = layout(se, safe: safe, panel: 150)
        XCTAssertEqual(collapsed.sideColumnMaxHeight, 374.5, accuracy: 0.01)
        XCTAssertEqual(columns(collapsed), 1)
        // A dropped pin's card over the stats panel: the fifth button moves left.
        let card = layout(se, safe: safe, panel: 310)
        XCTAssertEqual(card.sideColumnMaxHeight, 214.5, accuracy: 0.01)
        XCTAssertEqual(columns(card), 2)
        XCTAssertEqual(columns(card, buttons: 4), 1)
        // The column's bottom stays above the panel.
        let bottom = card.top + MapChromeLayout.controlStackHeight + MapChromeLayout.spacing + 206
        XCTAssertLessThanOrEqual(bottom, card.panelTop)
        XCTAssertGreaterThan(card.bannersMaxHeight, 250, "two banners and a toast fit under the title")
    }

    func testProMaxPortraitIsOneColumn() {
        let proMax = CGSize(width: 440, height: 956)
        let safe = EdgeInsets(top: 62, leading: 0, bottom: 34, trailing: 0)
        XCTAssertEqual(columns(layout(proMax, safe: safe, panel: 150)), 1)
        XCTAssertEqual(columns(layout(proMax, safe: safe, panel: 310, compass: true)), 1)
        XCTAssertGreaterThan(layout(proMax, safe: safe, panel: 310).sideColumnMaxHeight, 440)
    }

    func testLandscapeColumnClearsTheCentredPanel() {
        let se = layout(CGSize(width: 667, height: 375), safe: EdgeInsets(), panel: 150)
        XCTAssertFalse(se.panelSpan.overlaps(se.rightColumnSpan))
        XCTAssertEqual(se.sideColumnMaxHeight, 264.5, accuracy: 0.01)
        XCTAssertEqual(columns(se), 1)
        let proMax = layout(CGSize(width: 956, height: 440),
                            safe: EdgeInsets(top: 0, leading: 62, bottom: 21, trailing: 62), panel: 150)
        XCTAssertFalse(proMax.panelSpan.overlaps(proMax.rightColumnSpan))
        XCTAssertEqual(columns(proMax), 1)
        // The banners always stop above the panel (they span its width); landscape scrolls them.
        XCTAssertEqual(se.bannersMaxHeight, 375 - 8 - 150 - 12 - 4 - 52, accuracy: 0.01)
    }

    // MARK: - The terrain contract

    func testContourLabelsAreTheCoresUnlessAsked() throws {
        var request = ContourRenderRequest(space: .mercator, rect: [0, 0, 1, 1], style: .dark)
        let plain = try XCTUnwrap(JSONSerialization.jsonObject(with: JSONEncoder().encode(request)) as? [String: Any])
        XCTAssertNil(plain["labels"], "left to the core's default (on)")
        request.labels = false
        let off = try XCTUnwrap(JSONSerialization.jsonObject(with: JSONEncoder().encode(request)) as? [String: Any])
        XCTAssertEqual(off["labels"] as? Bool, false)
    }

    func testTilePlanFailuresCarryTheCoresReason() {
        // A quad needs four corners: the core refuses, and both wrappers say so.
        let request = TilePlanRequest(areas: [TilePlanRequest.Area(quad: [[0, 0], [0, 1]], zooms: [10])])
        XCTAssertNil(MapMeCore.tilePlan(request))
        guard case .failed(let message) = MapMeCore.tilePlanAnswer(request).status else {
            return XCTFail("a two-corner quad was planned")
        }
        XCTAssertFalse(message.isEmpty)
        if case .success = OfflinePlanner.plan(request) { XCTFail("OfflinePlanner planned a two-corner quad") }
    }

    // MARK: - Terrain arriving

    func testTerrainArrivalIsAnnounced() {
        let provider = TerrainProvider(store: nil)
        let before = provider.generation
        let posted = expectation(forNotification: TerrainProvider.terrainDidArriveNotification, object: provider)
        provider.terrainDidArrive()
        wait(for: [posted], timeout: 2)
        XCTAssertEqual(provider.generation, before + 1)
    }
}
