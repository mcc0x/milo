// macOS, no GUI interaction or Accessibility permission needed:
// xcrun clang -fobjc-arc -mmacosx-version-min=13.0 tests/preview_geometry.m macos/Preview/Browser.m \
//   -framework AppKit -framework ApplicationServices -framework CoreGraphics -framework WebKit -o /tmp/preview-geometry
// /tmp/preview-geometry
#import "../macos/Preview/Anchor.m"
#import <assert.h>

static void ScreenPlacement(void) {
    CGRect primary = CGRectMake(0, 0, 1440, 900);
    PreviewScreen screens[] = {
        {CGRectMake(0, 0, 1440, 900), CGRectMake(0, 0, 1440, 875), 1, 1, NO},
        {CGRectMake(-1280, -200, 1280, 1024), CGRectMake(-1280, -180, 1280, 1004), 2, 2, NO},
        {CGRectMake(1440, 900, 1920, 1080), CGRectMake(1440, 900, 1920, 1055), 2, 3, NO},
    };
    NSRect overlay;
    CGDirectDisplayID display;
    Geometry left = {CGRectMake(-1200, 220, 700, 500), CGRectMake(-1199.75, 230.25, 500.5, 400.25), 1};
    assert(PlaceOnScreen(left, primary, screens, 3, &overlay, &display));
    assert(display == 2 && NSEqualRects(overlay, NSMakeRect(-1199.5, 269.5, 500, 400)));

    Geometry above = {CGRectMake(1500, -920, 800, 600), CGRectMake(1510, -880, 790, 560), 1};
    assert(PlaceOnScreen(above, primary, screens, 3, &overlay, &display));
    assert(display == 3 && NSEqualRects(overlay, NSMakeRect(1510, 1220, 790, 560)));

    Geometry crosses = {CGRectMake(-200, 220, 700, 500), CGRectMake(0, 220, 400, 500), 1};
    assert(!PlaceOnScreen(crosses, primary, screens, 3, &overlay, &display));
    Geometry menuBar = {CGRectMake(100, 0, 700, 500), CGRectMake(100, 12, 400, 400), 1};
    assert(!PlaceOnScreen(menuBar, primary, screens, 3, &overlay, &display));
    screens[1].mirrored = YES;
    assert(!PlaceOnScreen(left, primary, screens, 3, &overlay, &display));
    screens[1].mirrored = NO;
    screens[2] = screens[1];
    assert(!PlaceOnScreen(left, primary, screens, 3, &overlay, &display));
}

static void AXPaneOverlap(void) {
    CGRect window = CGRectMake(0, 0, 1200, 900);
    CGRect tiled = CGRectMake(0, 0, 600, 900);
    CGRect pane = CGRectMake(480, 200, 280, 400);
    AXTree tree = {.nodes = {
        {window, NoNode, NO, NO, NO},
        {tiled, 0, YES, YES, YES},
        {tiled, 1, NO, NO, YES},
        {pane, 0, NO, NO, YES},
    }, .count = 4, .markerNode = 3};
    CGRect backgrounds[MaxBackgroundChecks];
    size_t count;
    assert(!ClearAXPane(&tree, NO, backgrounds, &count));
    assert(ClearAXPane(&tree, YES, backgrounds, &count));
    assert(count == 1 && CGRectEqualToRect(backgrounds[0], CGRectMake(480, 200, 120, 400)));

    // A separate AX branch or a control other than ordinary tiled background is uncertain.
    tree.nodes[4] = (AXNode){CGRectMake(520, 201, 10, 20), 0, NO, NO, YES};
    tree.count = 5;
    assert(!ClearAXPane(&tree, YES, backgrounds, &count));
    tree.nodes[4].parent = 1;
    tree.nodes[4].tiledControl = NO;
    assert(!ClearAXPane(&tree, YES, backgrounds, &count));
    tree.nodes[4].frame = CGRectNull;
    assert(!ClearAXPane(&tree, YES, backgrounds, &count));
    tree.count = 4;

    // Even a reported float cannot be painted over a partly clipped terminal.
    tree.nodes[3].parent = 1;
    assert(!ClearAXPane(&tree, YES, backgrounds, &count));
    tree.nodes[3].parent = 0;
    tree.nodes[3].frame = CGRectMake(600, 200, 280, 400);
    assert(ClearAXPane(&tree, NO, backgrounds, &count) && count == 0);
}

static NSDictionary *Metadata(pid_t pid, CGWindowID id, CGRect rect) {
    return @{
        (__bridge NSString *)kCGWindowOwnerPID: @(pid),
        (__bridge NSString *)kCGWindowNumber: @(id),
        (__bridge NSString *)kCGWindowLayer: @(CGWindowLevelForKey(kCGNormalWindowLevelKey)),
        (__bridge NSString *)kCGWindowAlpha: @1,
        (__bridge NSString *)kCGWindowBounds: CFBridgingRelease(CGRectCreateDictionaryRepresentation(rect)),
    };
}

static void WindowOrdering(void) {
    CGRect window = CGRectMake(20, 40, 1000, 800), pane = CGRectMake(700, 100, 240, 600);
    CGRect offPane = CGRectMake(0, 0, 400, 400);
    NSDictionary *rex = Metadata(100, 1, window);
    NSDictionary *otherRexWindow = Metadata(100, 2, offPane);
    NSDictionary *frontApp = Metadata(200, 3, pane);
    NSDictionary *ownPanel = Metadata(300, 4, pane);
    NSDictionary *harmless = Metadata(200, 5, offPane);
    NSDictionary *ambiguous = Metadata(100, 6, window);
    assert(MatchingVisibleWindow(@[ownPanel, harmless, rex, otherRexWindow], 100, 300, 4, window, pane) == 1);
    assert(MatchingVisibleWindow(@[frontApp, rex], 100, 300, 3, window, pane) == kCGNullWindowID);
    assert(MatchingVisibleWindow(@[otherRexWindow, rex], 100, 300, 4, window, pane) == kCGNullWindowID);
    assert(MatchingVisibleWindow(@[rex, ambiguous], 100, 300, 4, window, pane) == kCGNullWindowID);
    assert(MatchingVisibleWindow(@[rex, frontApp], 100, 300, 4, window, pane) == 1);
    assert(MatchingVisibleWindow(@[Metadata(200, 1, window)], 100, 300, 4, window, pane) == kCGNullWindowID);
}

int main(void) {
    @autoreleasepool {
        ScreenPlacement();
        AXPaneOverlap();
        WindowOrdering();
        puts("preview geometry tests passed");
    }
    return 0;
}
