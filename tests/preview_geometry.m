// macOS, no GUI interaction or permission needed:
// xcrun clang -fobjc-arc -mmacosx-version-min=13.0 tests/preview_geometry.m macos/Preview/Browser.m \
//   -framework AppKit -framework CoreGraphics -framework WebKit -o /tmp/preview-geometry
// /tmp/preview-geometry
#import <assert.h>
#import "../macos/Preview/Chrome.h"

// Exercise production remapping with deterministic chrome, not the user's settings.
static BOOL FixturePane(CGRect window, CGRect normalized, CGRect *pane) {
    MiloRexChrome chrome;
    return MiloDecodeRexChrome(@{}, &chrome) && MiloPaneInRexChrome(window, normalized, &chrome, pane);
}

#define MiloApproximatePane FixturePane
#import "../macos/Preview/Anchor.m"
#undef MiloApproximatePane

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
    Geometry fullScreen = {primary, CGRectMake(100, 50, 400, 400), 1};
    assert(!PlaceOnScreen(fullScreen, primary, screens, 3, &overlay, &display));
    screens[1].mirrored = YES;
    assert(!PlaceOnScreen(left, primary, screens, 3, &overlay, &display));
    screens[1].mirrored = NO;
    screens[2] = screens[1];
    assert(!PlaceOnScreen(left, primary, screens, 3, &overlay, &display));
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

static NSDictionary *Replacing(NSDictionary *metadata, CFStringRef key, id value) {
    NSMutableDictionary *copy = [metadata mutableCopy];
    if (value) copy[(__bridge NSString *)key] = value;
    else [copy removeObjectForKey:(__bridge NSString *)key];
    return copy;
}

static CGWindowID WindowID(NSArray *windows, CGWindowID pinned, const CGRect *normalized) {
    return SampleGeometry(windows, 100, pinned, 300, 4, normalized).windowID;
}

static void WindowPinning(void) {
    CGRect window = CGRectMake(20, 40, 1000, 800);
    CGRect right = CGRectMake(0.5, 0, 0.5, 1);
    CGRect overlap = CGRectMake(700, 180, 100, 100);
    NSDictionary *rex = Metadata(100, 1, window);
    NSDictionary *otherRex = Metadata(100, 2, CGRectMake(0, 0, 10, 10));
    NSDictionary *occluding = Metadata(200, 3, overlap);
    NSDictionary *panel = Metadata(300, 4, overlap);
    NSDictionary *harmless = Metadata(200, 5, CGRectMake(22, 180, 100, 100));
    NSDictionary *ownPopup = Metadata(300, 6, overlap);
    NSDictionary *wrongPID = Metadata(200, 1, window);
    NSDictionary *floating = Replacing(occluding, kCGWindowLayer, @(CGWindowLevelForKey(kCGNormalWindowLevelKey) + 25));

    assert(PinnedOwner(@[rex], 1) == 100);
    assert(!PinnedOwner(@[otherRex], 1));
    assert(!PinnedOwner(@[rex, otherRex], 1));
    assert(!PinnedOwner(@[Replacing(rex, kCGWindowOwnerPID, @"100")], 1));

    assert(WindowID(@[rex], 0, NULL) == 1);
    assert(!WindowID(@[harmless, rex], 0, NULL)); // Preflight has no pane, so checks the window.
    assert(!WindowID(@[rex, otherRex], 0, NULL)); // Even a disjoint second Rex window is ambiguous.
    assert(!WindowID(@[otherRex, rex], 1, &right));
    assert(!WindowID(@[wrongPID, otherRex], 1, &right));
    assert(!WindowID(@[otherRex], 1, &right)); // Never use a replacement window.
    assert(!WindowID(@[Replacing(rex, kCGWindowLayer, @25)], 1, &right));
    assert(!WindowID(@[Replacing(rex, kCGWindowAlpha, @0)], 1, &right));
    assert(WindowID(@[Replacing(otherRex, kCGWindowAlpha, @0), rex], 1, &right) == 1);
    assert(WindowID(@[panel, harmless, rex, occluding], 1, &right) == 1);
    assert(!WindowID(@[occluding, rex], 1, &right));
    assert(!WindowID(@[floating, rex], 1, &right));
    assert(!WindowID(@[ownPopup, rex], 1, &right));
    assert(!WindowID(@[Replacing(occluding, kCGWindowNumber, @4), rex], 1, &right));
    assert(WindowID(@[Replacing(occluding, kCGWindowAlpha, @0), rex], 1, &right) == 1);
    assert(!WindowID(@[Replacing(occluding, kCGWindowAlpha, nil), rex], 1, &right));
    assert(!WindowID(@[Replacing(harmless, kCGWindowBounds, nil), rex], 1, &right));
    assert(!WindowID(@[rex, Replacing(harmless, kCGWindowLayer, nil)], 1, &right));
    assert(!WindowID(@[rex, Replacing(harmless, kCGWindowOwnerPID, @100.5)], 1, &right));
}

static void RemapPinnedWindow(void) {
    CGRect right = CGRectMake(0.5, 0, 0.5, 1);
    CGRect window = CGRectMake(20, 40, 1000, 800);
    Geometry initial = SampleGeometry(@[Metadata(100, 1, window)], 100, 1, 300, 4, &right);
    assert(initial.windowID == 1 && CGRectEqualToRect(initial.window, window));
    assert(CGRectEqualToRect(initial.pane, CGRectMake(547, 152, 451, 670)));

    CGRect moved = CGRectMake(-1200, 220, 800, 600);
    Geometry current = SampleGeometry(@[Metadata(100, 1, moved)], 100, 1, 300, 4, &right);
    assert(current.windowID == initial.windowID && CGRectEqualToRect(current.window, moved));
    assert(!SameGeometry(initial, current) && CGRectEqualToRect(current.pane, CGRectMake(-773, 332, 351, 470)));
    PreviewScreen screens[] = {
        {CGRectMake(0, 0, 1440, 900), CGRectMake(0, 0, 1440, 875), 1, 1, NO},
        {CGRectMake(-1280, -200, 1280, 1024), CGRectMake(-1280, -180, 1280, 1004), 2, 2, NO},
    };
    NSRect frame;
    CGDirectDisplayID display;
    assert(PlaceOnScreen(current, screens[0].frame, screens, 2, &frame, &display));
    assert(display == 2 && NSEqualRects(frame, NSMakeRect(-773, 98, 351, 470)));
    assert(!WindowID(@[Metadata(100, 2, moved)], 1, &right));

    CGRect tiny = CGRectMake(0, 0, 0.001, 0.001);
    assert(!WindowID(@[Metadata(100, 1, window)], 1, &tiny));
}

static void WatcherProtocol(void) {
    pid_t pid;
    CGRect rect;
    const char tile[] = "show 123 tile 500000 250000 500000 750000";
    assert(ReadShow(tile, sizeof(tile) - 1, &pid, &rect));
    assert(pid == 123 && CGRectEqualToRect(rect, CGRectMake(0.5, 0.25, 0.5, 0.75)));
    const char *invalid[] = {
        "show 123 tile", "show 123 float 0 0 1000000 1000000",
        "show 1 tile 0 0 10 10", "show +123 tile 0 0 10 10", "show 2147483648 tile 0 0 10 10",
        "show 123 tile 0 0 0 1", "show 123 tile 0 0 1 0", "show 123 tile 0 0 1000001 1",
        "show 123 tile 900000 0 100001 1", "show 123 tile 0 900000 1 100001",
        "show 123 tile 0.5 0 10 10", "show 123 tile -1 0 10 10", "show 123 tile 0  0 10 10",
        "show 123 tile 0 0 10 10 ", "show 123 tile 0 0 10 10\r", "show 123 tile 0 0 10 10 10",
    };
    for (size_t i = 0; i < sizeof(invalid) / sizeof(invalid[0]); ++i) {
        assert(!ReadShow(invalid[i], strlen(invalid[i]), &pid, &rect));
    }
    const char embedded[] = "show 123 tile 0 0 10 10\0hide";
    assert(!ReadShow(embedded, sizeof(embedded) - 1, &pid, &rect));
    assert(SampleFresh(1.5, 1) && !SampleFresh(1.75, 1) && !SampleFresh(1, 1.01));
}

int main(void) {
    @autoreleasepool {
        ScreenPlacement();
        WindowPinning();
        RemapPinnedWindow();
        WatcherProtocol();
        puts("preview geometry tests passed");
    }
    return 0;
}
