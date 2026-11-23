// macOS, no GUI interaction or permission needed:
// xcrun clang -fobjc-arc -mmacosx-version-min=13.0 tests/preview_geometry.m macos/Preview/Browser.m \
//   -framework AppKit -framework CoreGraphics -framework WebKit -o /tmp/preview-geometry
// /tmp/preview-geometry
// /tmp/preview-geometry --panel-staging additionally checks Quartz in a logged-in GUI session.
#import <assert.h>
#import <stdio.h>
#import <time.h>
#import "../macos/Preview/Chrome.h"

// Exercise production remapping with deterministic chrome, not the user's settings.
static BOOL FixturePane(CGRect window, CGRect normalized, CGRect *pane) {
    MiloRexChrome chrome;
    return MiloDecodeRexChrome(@{}, &chrome) && MiloPaneInRexChrome(window, normalized, &chrome, pane);
}

static struct {
    uint64_t continuous, realtime;
    BOOL enabled;
} fixtureClock;

static uint64_t FixtureClockNanos(clockid_t clock) {
    if (!fixtureClock.enabled) return clock_gettime_nsec_np(clock);
    return clock == CLOCK_REALTIME ? fixtureClock.realtime : fixtureClock.continuous;
}

#define clock_gettime_nsec_np FixtureClockNanos

#define MiloApproximatePane FixturePane
#import "../macos/Preview/Anchor.m"
#undef MiloApproximatePane
#undef clock_gettime_nsec_np

static void ScreenPlacement(void) {
    CGRect primary = CGRectMake(0, 0, 1440, 900);
    PreviewScreen screens[] = {
        {CGRectMake(0, 0, 1440, 900), CGRectMake(0, 0, 1440, 875), 1, 1, NO},
        {CGRectMake(-1280, -200, 1280, 1024), CGRectMake(-1280, -180, 1280, 1004), 2, 2, NO},
        {CGRectMake(1440, 900, 1920, 1080), CGRectMake(1440, 900, 1920, 1055), 2, 3, NO},
    };
    NSRect overlay;
    CGDirectDisplayID display;
    Geometry left = {CGRectMake(-1200, 220, 700, 500), CGRectMake(-1199.75, 230.25, 500.625, 400.625), 1, NULL};
    assert(!ScreenPlacementFailure(left, primary, screens, 3, &overlay, &display));
    assert(display == 2 && NSEqualRects(overlay, NSMakeRect(-1199.5, 269.5, 500, 400)));

    Geometry above = {CGRectMake(1500, -920, 800, 600), CGRectMake(1510, -880, 790, 560), 1, NULL};
    assert(!ScreenPlacementFailure(above, primary, screens, 3, &overlay, &display));
    assert(display == 3 && NSEqualRects(overlay, NSMakeRect(1510, 1220, 790, 560)));

    Geometry crosses = {CGRectMake(-200, 220, 700, 500), CGRectMake(0, 220, 400, 500), 1, NULL};
    assert(ScreenPlacementFailure(crosses, primary, screens, 3, &overlay, &display));
    Geometry menuBar = {CGRectMake(100, 0, 700, 500), CGRectMake(100, 12, 400, 400), 1, NULL};
    assert(ScreenPlacementFailure(menuBar, primary, screens, 3, &overlay, &display));
    Geometry fullScreen = {primary, CGRectMake(100, 50, 400, 400), 1, NULL};
    assert(ScreenPlacementFailure(fullScreen, primary, screens, 3, &overlay, &display));
    screens[1].mirrored = YES;
    assert(ScreenPlacementFailure(left, primary, screens, 3, &overlay, &display));
    screens[1].mirrored = NO;
    screens[2] = screens[1];
    assert(ScreenPlacementFailure(left, primary, screens, 3, &overlay, &display));
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
    return SampleGeometry(windows, 100, pinned, 300, NULL, normalized).windowID;
}

static void PinLookupResults(void) {
    CGRect frame = CGRectMake(20, 40, 1000, 800);
    NSDictionary *rex = Metadata(100, UINT32_MAX, frame);
    pid_t owner = 0;
    // An unavailable connection, malformed response, and a missing window
    // have different consequences for a running but concealed anchor.
    assert(PinnedOwner(nil, UINT32_MAX, &owner) == PinUnavailable && owner == 0);
    assert(PinnedOwner(@[], UINT32_MAX, &owner) == PinMissing && owner == 0);
    assert(PinnedOwner(@[rex], 1, &owner) == PinUnavailable && owner == 0);
    assert(PinnedOwner(@[rex, rex], UINT32_MAX, &owner) == PinUnavailable && owner == 0);
    assert(PinnedOwner(@[Replacing(rex, kCGWindowOwnerPID, @"100")], UINT32_MAX, &owner) == PinUnavailable && owner == 0);
    // A pinned resource need not currently be onscreen to count as live.
    NSDictionary *offscreen = Replacing(rex, kCGWindowIsOnscreen, @NO);
    assert(PinnedOwner(@[offscreen], UINT32_MAX, &owner) == PinFound && owner == 100);
    assert(PinnedOwner(@[Metadata(200, UINT32_MAX, frame)], UINT32_MAX, &owner) == PinFound && owner == 200);
}

static void WindowPinning(void) {
    CGRect window = CGRectMake(20, 40, 1000, 800);
    const CGRect right = CGRectMake(0.5, 0, 0.5, 1);
    NSDictionary *rex = Metadata(100, 1, window);
    NSDictionary *otherRex = Metadata(100, 2, CGRectMake(0, 0, 10, 10));
    NSDictionary *wrongPID = Metadata(200, 1, window);

    assert(WindowID(@[rex], 0, NULL) == 1);
    assert(!WindowID(@[rex, otherRex], 0, NULL)); // Even a disjoint second Rex window is ambiguous.
    assert(!WindowID(@[rex, rex], 0, NULL));
    assert(!WindowID(@[otherRex, rex], 1, &right));
    assert(!WindowID(@[rex, otherRex], 1, &right));
    assert(!WindowID(@[rex, rex], 1, &right));
    assert(!WindowID(@[wrongPID], 0, NULL));
    assert(!WindowID(@[wrongPID, otherRex], 1, &right));
    assert(!WindowID(@[otherRex], 1, &right)); // Never use a replacement window.
    NSDictionary *floatingRex = Replacing(rex, kCGWindowLayer, @25);
    NSDictionary *transparentRex = Replacing(rex, kCGWindowAlpha, @0);
    assert(!WindowID(@[floatingRex], 0, NULL));
    assert(!WindowID(@[floatingRex], 1, &right));
    assert(!WindowID(@[transparentRex], 0, NULL));
    assert(!WindowID(@[transparentRex], 1, &right));
    NSDictionary *transparentOther = Replacing(otherRex, kCGWindowAlpha, @0);
    assert(WindowID(@[transparentOther, rex], 0, NULL) == 1);
    assert(WindowID(@[transparentOther, rex], 1, &right) == 1);
    assert(!WindowID(@[Replacing(rex, kCGWindowBounds, nil)], 0, NULL));
    assert(!WindowID(@[Replacing(rex, kCGWindowAlpha, nil)], 0, NULL));
    NSDictionary *foreign = Metadata(200, 3, window);
    assert(!WindowID(@[rex, Replacing(foreign, kCGWindowLayer, nil)], 1, &right));
    assert(!WindowID(@[rex, Replacing(foreign, kCGWindowOwnerPID, @100.5)], 1, &right));
}

static void PaneOcclusion(void) {
    CGRect window = CGRectMake(20, 40, 1000, 800);
    const CGRect right = CGRectMake(0.5, 0, 0.5, 1);
    NSDictionary *rex = Metadata(100, 1, window);
    Geometry clear = SampleGeometry(@[rex], 100, 1, 300, NULL, &right);
    assert(clear.windowID == 1);

    NSDictionary *sibling = Metadata(200, 3, CGRectMake(22, 180, 100, 100));
    CGRect titlebarRect = CGRectMake(700, 40, 100, 24);
    NSDictionary *titlebar = Metadata(200, 4, titlebarRect);
    NSDictionary *titlebarChrome = Replacing(Metadata(100, 5, titlebarRect), kCGWindowLayer, @25);
    CGFloat l = CGRectGetMinX(clear.pane), r = CGRectGetMaxX(clear.pane);
    CGFloat t = CGRectGetMinY(clear.pane), b = CGRectGetMaxY(clear.pane);
    CGRect edges[] = {
        CGRectMake(l + 30, t - 20, 10, 20), CGRectMake(r, t + 30, 20, 10),
        CGRectMake(l + 30, b, 10, 20), CGRectMake(l - 20, t + 30, 20, 10),
    };
    const CGPoint inward[] = {{0, 0.5}, {-0.5, 0}, {0, -0.5}, {0.5, 0}};
    for (size_t i = 0; i < sizeof(edges) / sizeof(*edges); ++i) {
        assert(WindowID(@[Metadata(200, 5, edges[i]), rex], 1, &right) == 1);
        CGRect across = CGRectOffset(edges[i], inward[i].x, inward[i].y);
        Geometry blocked = SampleGeometry(@[Metadata(200, 5, across), rex], 100, 1, 300, NULL, &right);
        assert(!blocked.windowID && blocked.rejection &&
            !strcmp(blocked.rejection, "another window overlaps the tile; waiting for clear placement"));
    }
    for (NSDictionary *surface in @[sibling, titlebar, titlebarChrome]) {
        assert(WindowID(@[surface, rex], 0, NULL) == 1);
        assert(WindowID(@[surface, rex], 1, &right) == 1);
    }

    NSDictionary *onePixel = Metadata(200, 6, CGRectOffset(edges[3], 0.5, 0));
    NSDictionary *covered = Metadata(200, 7, window);
    NSDictionary *menu = Replacing(covered, kCGWindowLayer, @(CGWindowLevelForKey(kCGMainMenuWindowLevelKey)));
    NSDictionary *chrome = Replacing(Metadata(100, 8, window), kCGWindowLayer, @25);
    NSDictionary *helper = Metadata(300, 9, window);
    for (NSDictionary *surface in @[onePixel, covered, menu, chrome, helper]) {
        assert(WindowID(@[surface, rex], 0, NULL) == 1); // Pin identity even while covered.
        assert(!WindowID(@[surface, rex], 1, &right));
        assert(WindowID(@[rex, surface], 1, &right) == 1);
    }
    const CGRect workspace = CGRectMake(0, 0, 1, 1);
    assert(WindowID(@[covered, rex], 0, &workspace) == 1); // Foreground preflight projects the workspace.
    assert(WindowID(@[Replacing(covered, kCGWindowAlpha, @0), rex], 1, &right) == 1);

    NSDictionary *unknownOpacity = Replacing(covered, kCGWindowAlpha, nil);
    NSDictionary *unknownBounds = Replacing(sibling, kCGWindowBounds, nil);
    for (NSDictionary *surface in @[unknownOpacity, unknownBounds]) {
        assert(WindowID(@[surface, rex], 0, NULL) == 1);
        assert(!WindowID(@[surface, rex], 1, &right)); // Unknown coverage above the pane is unsafe.
        assert(WindowID(@[rex, surface], 1, &right) == 1);
    }
}

static void PanelOrdering(void) {
    CGRect window = CGRectMake(20, 40, 1000, 800);
    CGRect right = CGRectMake(0.5, 0, 0.5, 1);
    NSDictionary *rex = Metadata(100, 1, window);
    Geometry geometry = SampleGeometry(@[rex], 100, 1, 300, NULL, &right);
    assert(geometry.windowID == 1);
    CGRect expected = CGRectInset(geometry.pane, 0.5, 0.5);
    NSDictionary *panel = Metadata(300, 44, expected);
    NSDictionary *rounded = Metadata(300, 91, CGRectIntegral(expected));
    NSDictionary *harmless = Metadata(200, 5, CGRectMake(22, 180, 100, 100));
    NSDictionary *popup = Metadata(300, 6, CGRectMake(700, 180, 100, 100));
    NSDictionary *spoof = Metadata(200, 44, expected);
    NSDictionary *occluder = Metadata(200, 7, expected);
    NSDictionary *floating = Replacing(panel, kCGWindowLayer, @(CGWindowLevelForKey(kCGNormalWindowLevelKey) + 25));

    assert(!SampleGeometry(@[panel, rex], 100, 1, 300, NULL, &right).windowID); // Not yet ordered.
    assert(SampleGeometry(@[panel, harmless, rex, occluder], 100, 1, 300, &expected, &right).windowID == 1);
    assert(SampleGeometry(@[rounded, rex], 100, 1, 300, &expected, &right).windowID == 1);
    assert(!SampleGeometry(@[rex, panel], 100, 1, 300, &expected, &right).windowID);
    Geometry absent = SampleGeometry(@[rex], 100, 1, 300, &expected, &right);
    assert(!absent.windowID && absent.rejection == PanelNotListed && CGRectEqualToRect(absent.pane, geometry.pane));
    Geometry blocked = SampleGeometry(@[occluder, rex], 100, 1, 300, &expected, &right);
    assert(!blocked.windowID && blocked.rejection != PanelNotListed); // Missing panel must not mask an obstruction.
    assert(!SampleGeometry(@[spoof, rex], 100, 1, 300, &expected, &right).windowID);
    assert(!SampleGeometry(@[popup, rex], 100, 1, 300, &expected, &right).windowID);
    assert(!SampleGeometry(@[panel, rex, popup], 100, 1, 300, &expected, &right).windowID);
    assert(!SampleGeometry(@[panel, occluder, rex], 100, 1, 300, &expected, &right).windowID);
    assert(!SampleGeometry(@[floating, rex], 100, 1, 300, &expected, &right).windowID);
    assert(!SampleGeometry(@[Replacing(panel, kCGWindowAlpha, @0), rex], 100, 1, 300, &expected, &right).windowID);
}

static void AssertOwnedPixels(NSView *view, CGFloat expectedAlpha) {
    // AppKit draws only this fixture's own views into memory. It never reads
    // the WindowServer image, another app's window, or the desktop.
    NSRect rect = view.bounds;
    assert(rect.origin.x == 0 && rect.origin.y == 0);
    NSInteger width = (NSInteger)NSWidth(rect), height = (NSInteger)NSHeight(rect);
    assert(width > 2 && height > 2);
    NSBitmapImageRep *bitmap = [[NSBitmapImageRep alloc] initWithBitmapDataPlanes:NULL
        pixelsWide:width pixelsHigh:height bitsPerSample:8 samplesPerPixel:4 hasAlpha:YES
        isPlanar:NO colorSpaceName:NSDeviceRGBColorSpace bytesPerRow:0 bitsPerPixel:0];
    assert(bitmap);
    NSGraphicsContext *context = [NSGraphicsContext graphicsContextWithBitmapImageRep:bitmap];
    assert(context);
    CGContextClearRect(context.CGContext, rect);
    [view displayRectIgnoringOpacity:rect inContext:context];
    const NSPoint points[] = {
        {width / 2, 0}, {width - 1, height / 2}, {width / 2, height - 1}, {0, height / 2},
        {width / 2, height / 2},
    };
    for (size_t i = 0; i < sizeof(points) / sizeof(*points); ++i) {
        NSColor *pixel = [bitmap colorAtX:(NSInteger)points[i].x y:(NSInteger)points[i].y];
        assert(pixel && pixel.alphaComponent == expectedAlpha);
    }
}

static void PanelStaging(void) {
    assert([NSApplication.sharedApplication setActivationPolicy:NSApplicationActivationPolicyAccessory]);
    NSRect screen = NSScreen.screens.firstObject.visibleFrame;
    assert(NSWidth(screen) >= 200 && NSHeight(screen) >= 120);
    NSRect frame = NSIntegralRect(NSMakeRect(NSMidX(screen) - 100, NSMidY(screen) - 60, 200, 120));
    MiloPreviewPanel *panel = [[MiloPreviewPanel alloc] initWithContentRect:frame
        styleMask:NSWindowStyleMaskBorderless | NSWindowStyleMaskNonactivatingPanel backing:NSBackingStoreBuffered defer:NO];
    panel.floatingPanel = NO;
    panel.level = NSNormalWindowLevel;
    panel.collectionBehavior = NSWindowCollectionBehaviorTransient | NSWindowCollectionBehaviorIgnoresCycle;
    panel.hasShadow = NO;
    panel.animationBehavior = NSWindowAnimationBehaviorNone;
    // A fully transparent, non-drawing child models the unpainted portions
    // of a WebKit page without depending on the web process's bitmap path.
    NSView *transparentPage = [[NSView alloc] initWithFrame:NSZeroRect];
    [panel installContentForStaging:transparentPage];
    assert(!panel.opaque && panel.backgroundColor.alphaComponent == 0 && panel.alphaValue == 1);
    assert(panel.contentView.hidden && panel.ignoresMouseEvents && !panel.canBecomeKeyWindow);
    [panel orderWindow:NSWindowAbove relativeTo:0];
    assert(panel.visible && !panel.keyWindow && panel.contentView.hidden);
    AssertOwnedPixels(panel.contentView.superview, 0);

    // Observe only this fixture's actual window metadata. Model the foreign
    // target and both ordering outcomes without involving a real target.
    CGRect expected = CGRectMake(NSMinX(frame), CGDisplayBounds(CGMainDisplayID()).size.height - NSMaxY(frame),
        NSWidth(frame), NSHeight(frame));
    NSArray *owned = nil;
    pid_t fixturePID = getpid();
    NSDate *deadline = [NSDate dateWithTimeIntervalSinceNow:1];
    do {
        NSMutableArray *matching = [NSMutableArray array];
        for (NSDictionary *window in VisibleWindows()) {
            int64_t pid;
            if (WindowInteger(window, kCGWindowOwnerPID, 1, INT_MAX, &pid) && pid == fixturePID)
                [matching addObject:window];
        }
        owned = matching;
        if (owned.count) break;
        [NSRunLoop.currentRunLoop runUntilDate:[NSDate dateWithTimeIntervalSinceNow:0.02]];
    } while (deadline.timeIntervalSinceNow > 0);
    pid_t targetPID = fixturePID == 100 ? 101 : 100;
    NSDictionary *target = Metadata(targetPID, UINT32_MAX, expected);
    assert(!SampleGeometry([@[target] arrayByAddingObjectsFromArray:owned], targetPID, UINT32_MAX,
        fixturePID, &expected, NULL).windowID);
    assert(panel.contentView.hidden && panel.ignoresMouseEvents && !panel.canBecomeKeyWindow);
    assert(SampleGeometry([owned arrayByAddingObject:target], targetPID, UINT32_MAX, fixturePID, &expected, NULL).windowID);
    [panel orderOut:nil];

    // Draw the proven body itself (not the window background) so the native
    // underlay is exercised beneath the transparent child at all four edges.
    panel.placementProven = YES;
    assert(panel.opaque && panel.backgroundColor.alphaComponent == 1 && panel.alphaValue == 1);
    assert(!panel.contentView.hidden && !panel.ignoresMouseEvents && panel.canBecomeKeyWindow);
    assert(transparentPage.superview == panel.contentView && NSEqualRects(transparentPage.frame, panel.contentView.bounds));
    AssertOwnedPixels(panel.contentView, 1);

    panel.placementProven = NO;
    assert(!panel.opaque && panel.backgroundColor.alphaComponent == 0 && panel.alphaValue == 1);
    assert(panel.contentView.hidden && panel.ignoresMouseEvents && !panel.canBecomeKeyWindow);
    // Draw the enclosing frame view so AppKit skips the hidden body.
    AssertOwnedPixels(panel.contentView.superview, 0);
    assert(transparentPage.superview == panel.contentView);
}

static void RemapPinnedWindow(void) {
    CGRect right = CGRectMake(0.5, 0, 0.5, 1);
    CGRect window = CGRectMake(20, 40, 1000, 800);
    Geometry initial = SampleGeometry(@[Metadata(100, 1, window)], 100, 1, 300, NULL, &right);
    assert(initial.windowID == 1 && CGRectEqualToRect(initial.window, window));
    assert(CGRectEqualToRect(initial.pane, CGRectMake(537, 146, 471, 682)));

    CGRect moved = CGRectMake(-1200, 220, 800, 600);
    Geometry current = SampleGeometry(@[Metadata(100, 1, moved)], 100, 1, 300, NULL, &right);
    assert(current.windowID == initial.windowID && CGRectEqualToRect(current.window, moved));
    assert(!SameGeometry(initial, current) && CGRectEqualToRect(current.pane, CGRectMake(-783, 326, 371, 482)));
    PreviewScreen screens[] = {
        {CGRectMake(0, 0, 1440, 900), CGRectMake(0, 0, 1440, 875), 1, 1, NO},
        {CGRectMake(-1280, -200, 1280, 1024), CGRectMake(-1280, -180, 1280, 1004), 2, 2, NO},
    };
    NSRect frame;
    CGDirectDisplayID display;
    assert(!ScreenPlacementFailure(current, screens[0].frame, screens, 2, &frame, &display));
    assert(display == 2 && NSEqualRects(frame, NSMakeRect(-783, 92, 371, 482)));
    assert(!WindowID(@[Metadata(100, 2, moved)], 1, &right));

    CGRect tiny = CGRectMake(0, 0, 0.001, 0.001);
    assert(!WindowID(@[Metadata(100, 1, window)], 1, &tiny));
}

static void WatcherProtocol(void) {
    pid_t pid;
    CGRect rect;
    const char tile[] = "show 123 tile 500000 250000 500000 750000";
    assert(ReadTile(tile, sizeof(tile) - 1, &pid, &rect));
    assert(pid == 123 && CGRectEqualToRect(rect, CGRectMake(0.5, 0.25, 0.5, 0.75)));
    const char *invalid[] = {
        "show 123 tile", "show 123 float 0 0 1000000 1000000",
        "show 1 tile 0 0 10 10", "show +123 tile 0 0 10 10", "show 2147483648 tile 0 0 10 10",
        "show 123 tile 0 0 0 1", "show 123 tile 0 0 1 0", "show 123 tile 0 0 1000001 1",
        "show 123 tile 900000 0 100001 1", "show 123 tile 0 900000 1 100001",
        "show 123 tile 0.5 0 10 10", "show 123 tile -1 0 10 10", "show 123 tile 0  0 10 10",
        "show 123 tile 0 0 10 10 ", "show 123 tile 0 0 10 10\r", "show 123 tile 0 0 10 10 10",
        "guard", "guard 123 tile 0 0 10 10", "guard-hide",
    };
    for (size_t i = 0; i < sizeof(invalid) / sizeof(invalid[0]); ++i) {
        assert(!ReadTile(invalid[i], strlen(invalid[i]), &pid, &rect));
    }
    const char embedded[] = "show 123 tile 0 0 10 10\0hide";
    assert(!ReadTile(embedded, sizeof(embedded) - 1, &pid, &rect));
    assert(SampleFresh(1.5, 1) && !SampleFresh(1.75, 1) && !SampleFresh(1, 1.01));
}

static VisibilityUpdate PipeRecord(const int pipeFD[2], VisibilityLease *lease, NSTimeInterval now, const char *record) {
    size_t length = strlen(record);
    char line[96];
    assert(length && length <= sizeof(line) && record[length - 1] == '\n');
    assert(WriteAll(pipeFD[1], record, length));
    assert(read(pipeFD[0], line, sizeof(line)) == (ssize_t)length);
    return UpdateVisibility(lease, now, 1700000000 + now, 0, 123, line, length - 1);
}

static void WatcherLease(void) {
    int pipeFD[2];
    assert(pipe(pipeFD) == 0 && fcntl(pipeFD[0], F_SETFL, O_NONBLOCK) == 0);
    VisibilityLease lease = {0};
    const char show[] = "show 123 tile 500000 250000 500000 750000 @1700000010\n";
    assert(PipeRecord(pipeFD, &lease, 9, "renew @1700000009\n") == VisibilityUnchanged && !LeaseFresh(&lease, 9));
    assert(PipeRecord(pipeFD, &lease, 10.75, show) == VisibilityChanged && LeaseFresh(&lease, 10.75));
    assert(lease.checked == 10); // Pay for the fractional observation and pipe delay.
    assert(PipeRecord(pipeFD, &lease, 11.5, "renew @1700000011\n") == VisibilityUnchanged && lease.checked == 11);
    assert(LeaseFresh(&lease, 13.99) && !LeaseFresh(&lease, 10));

    // No EOF or hide arrives, and a successful current Quartz sample cannot extend the lease.
    char byte;
    assert(read(pipeFD[0], &byte, 1) < 0 && errno == EAGAIN);
    assert(SampleFresh(14, 13.875) && !LeaseFresh(&lease, 14));
    assert(PipeRecord(pipeFD, &lease, 15, "renew @1700000014\n") == VisibilityChanged && LeaseFresh(&lease, 15));
    assert(CGRectEqualToRect(lease.tile, CGRectMake(0.5, 0.25, 0.5, 0.75)));
    assert(PipeRecord(pipeFD, &lease, 15.1, "show 123 tile 500000 250000 500000 750000 @1700000015\n") == VisibilityUnchanged);
    const char resized[] = "show 123 tile 400000 0 600000 1000000 @1700000015\n";
    assert(PipeRecord(pipeFD, &lease, 15.2, resized) == VisibilityChanged && LeaseFresh(&lease, 15.2));
    assert(CGRectEqualToRect(lease.tile, CGRectMake(0.4, 0, 0.6, 1)));
    assert(PipeRecord(pipeFD, &lease, 15.5, "renew @1700000015\n") == VisibilityUnchanged);
    assert(CGRectEqualToRect(lease.tile, CGRectMake(0.4, 0, 0.6, 1)));
    assert(PipeRecord(pipeFD, &lease, 16, "hide\n") == VisibilityChanged);
    assert(PipeRecord(pipeFD, &lease, 17, "renew @1700000017\n") == VisibilityUnchanged && !LeaseFresh(&lease, 17));
    assert(PipeRecord(pipeFD, &lease, 18, "show 124 tile 0 0 1 1 @1700000018\n") == VisibilityWrongOwner && !LeaseFresh(&lease, 18));
    assert(PipeRecord(pipeFD, &lease, 19, "show 123 tile 0 0 1000000 1000000 @1700000019\n") == VisibilityChanged);
    const char *invalid[] = {
        "renew\n", "show 123 tile 0 0 1 1\n", "renew 123\n", "renew@1700000020\n",
        "renew @+1700000020\n", "renew @01700000020\n", "renew @1700000020.2\n", "renew @\n",
        "renew  @1700000020\n", "renew @1700000020 \n", "renew @18446744073709551616\n",
        "guard-hide\n", "resize-gap\n", "shared-input\n",
    };
    for (size_t i = 0; i < sizeof(invalid) / sizeof(invalid[0]); ++i)
        assert(PipeRecord(pipeFD, &lease, 20, invalid[i]) == VisibilityInvalid);
    assert(PipeRecord(pipeFD, &lease, 20.25, "renew @1700000021\n") == VisibilityChanged && !LeaseFresh(&lease, 20.25));
    assert(PipeRecord(pipeFD, &lease, 20.5, "show 123 tile 0 0 1 1 @18446744073709551615\n") == VisibilityChanged);
    assert(!lease.announcedAt && !LeaseFresh(&lease, 20.5));
    assert(PipeRecord(pipeFD, &lease, 21, "show 123 tile 0 0 1 1 @1700000021\n") == VisibilityChanged);
    assert(LeaseFresh(&lease, 21)); // An unusable future stamp cannot poison a later correct show.
    assert(PipeRecord(pipeFD, &lease, 21, "unclaimed\n") == VisibilityUnclaimed);
    close(pipeFD[1]);
    close(pipeFD[0]);
}

static void FocusAcknowledgment(void) {
    int pipeFD[2];
    assert(pipe(pipeFD) == 0 && fcntl(pipeFD[0], F_SETFL, O_NONBLOCK) == 0);
    VisibilityLease lease = {0};
    assert(PipeRecord(pipeFD, &lease, 10, "show 123 tile 0 0 1000000 1000000 @1700000010\n") == VisibilityChanged);

    NSTimeInterval deadline = 10 + FocusReplyLifetime;
    assert(ReadFocusReply(pipeFD[0], 11, &deadline) == FocusWaiting);
    assert(PipeRecord(pipeFD, &lease, 23, "renew @1700000023\n") == VisibilityChanged);
    assert(LeaseFresh(&lease, 23) && deadline > 23);
    assert(ReadFocusReply(pipeFD[0], 23, &deadline) == FocusWaiting);

    assert(WriteAll(pipeFD[1], "1", 1));
    assert(ReadFocusReply(pipeFD[0], 24, &deadline) == FocusApproved && deadline == 0);
    assert(LeaseFresh(&lease, 24));
    assert(!LeaseFresh(&lease, 26)); // Approval doesn't extend the visibility lease.

    deadline = 27 + FocusReplyLifetime;
    assert(WriteAll(pipeFD[1], "0", 1));
    assert(ReadFocusReply(pipeFD[0], 28, &deadline) == FocusRefused && deadline == 0);
    assert(!LeaseFresh(&lease, 28));

    deadline = 30 + FocusReplyLifetime;
    assert(WriteAll(pipeFD[1], "!", 1));
    assert(ReadFocusReply(pipeFD[0], 31, &deadline) == FocusFailed && deadline > 0);
    assert(WriteAll(pipeFD[1], "1", 1));
    assert(ReadFocusReply(pipeFD[0], deadline, &deadline) == FocusFailed && deadline > 0);
    char byte;
    assert(read(pipeFD[0], &byte, 1) == 1 && byte == '1'); // Late approval isn't consumed.
    close(pipeFD[1]);
    assert(ReadFocusReply(pipeFD[0], 32, &deadline) == FocusEnded && deadline > 0);
    close(pipeFD[0]);
}

@interface WatcherFixtureOwner : NSObject
- (pid_t)processIdentifier;
@end

@implementation WatcherFixtureOwner
- (pid_t)processIdentifier { return 123; }
- (BOOL)isTerminated { return NO; }
@end

// Only the panel's state is needed to test synchronous input revocation;
// the default geometry test does not create any actual windows.
@interface WatcherFixturePanel : NSObject
@property (nonatomic) BOOL placementProven;
@property (nonatomic, getter=isVisible) BOOL visible;
- (void)orderOut:(id)sender;
@end

@implementation WatcherFixturePanel
- (void)orderOut:(id)sender { (void)sender; self.visible = NO; }
@end

@interface WatcherFixtureAnchor : MiloPreviewAnchor
- (void)exerciseDrain:(int)writer;
- (void)exerciseBurst:(int)writer;
- (void)exerciseClockJump:(int)writer;
@end

@implementation WatcherFixtureAnchor
- (BOOL)canSample {
    return [self observerEligible];
}

static void WriteStamped(int writer, const char *record, time_t stamp) {
    char line[96];
    int length = snprintf(line, sizeof(line), "%s @%lld\n", record, (long long)stamp);
    assert(length > 0 && (size_t)length < sizeof(line) && WriteAll(writer, line, (size_t)length));
}

- (void)exerciseDrain:(int)writer {
    static const char show[] = "show 123 tile 500000 0 500000 1000000";
    time_t stamp = time(NULL);
    WriteStamped(writer, show, stamp - 30);
    WriteStamped(writer, "renew", stamp - 29);
    NSTimeInterval started = NSProcessInfo.processInfo.systemUptime;
    [self readState];
    assert(_lease.announcedAt == (uint64_t)(stamp - 30) && !LeaseFresh(&_lease, LeaseNow()));
    assert(![self proofCurrent:_generation started:started]);

    // A later genuine observation can revalidate the known tile, but the
    // generation of a proof taken before draining cannot be reused.
    NSUInteger generation = _generation;
    WriteStamped(writer, "renew", time(NULL));
    assert(![self proofCurrent:generation started:started]);
    assert(LeaseFresh(&_lease, LeaseNow()) && !_candidate.windowID);
    assert(![self proofCurrent:_generation started:started - SampleLifetime]);
    assert([self proofCurrent:_generation started:NSProcessInfo.processInfo.systemUptime]);

    generation = _generation;
    Geometry stale = {.windowID = _pinned};
    // Even an incomplete record can become a revocation. It must invalidate
    // proof immediately, before the rest of the pipe record arrives.
    assert(WriteAll(writer, "hi", 2));
    [self readState];
    assert(_lineLength == 2 && _generation != generation);
    assert(WriteAll(writer, "de\n", 3));
    WriteStamped(writer, "renew", time(NULL));
    [self applySample:stale checkedPanel:NO lostPin:NO generation:generation
        started:NSProcessInfo.processInfo.systemUptime];
    assert(!_lease.announcedAt);
    assert(!_lineLength && !LeaseFresh(&_lease, LeaseNow()) && !_candidate.windowID);

    // A full show can re-establish the lease, but its new geometry cannot
    // reuse an in-flight result from the previous generation.
    WriteStamped(writer, "show 123 tile 600000 0 400000 1000000", time(NULL));
    WriteStamped(writer, "renew", time(NULL));
    [self applySample:stale checkedPanel:NO lostPin:NO generation:generation
        started:NSProcessInfo.processInfo.systemUptime];
    assert(LeaseFresh(&_lease, LeaseNow()) && !_candidate.windowID);
    assert(CGRectEqualToRect(_lease.tile, CGRectMake(0.6, 0, 0.4, 1)));
}

- (void)exerciseBurst:(int)writer {
    static const char show[] = "show 123 tile 500000 0 500000 1000000";
    time_t stamp = time(NULL);
    WriteStamped(writer, show, stamp);
    [self readState];
    assert([self observerEligible]);
    WatcherFixturePanel *panel = [WatcherFixturePanel new];
    panel.placementProven = panel.visible = YES;
    _panel = (MiloPreviewPanel *)panel;
    NSUInteger generation = _generation;

    // At least 2160 bytes of valid queued records exceed one 1536-byte
    // read budget, so the first pass cannot reach the trailing hide.
    for (size_t i = 0; i < 120; ++i) WriteStamped(writer, "renew", stamp);
    assert(WriteAll(writer, "hide\n", 5));
    assert(![self acceptsPanelInput]);
    assert(!_finished && _drainingState && _lease.announcedAt && ![self observerEligible]);
    assert(!panel.visible && !panel.placementProven && _panel == (MiloPreviewPanel *)panel);
    assert(_generation != generation);

    [self readState];
    assert(!_finished && !_drainingState && !_lineLength && !_lease.announcedAt);
    WriteStamped(writer, "renew", time(NULL));
    [self readState];
    assert(![self observerEligible]);
    WriteStamped(writer, show, time(NULL));
    [self readState];
    assert([self observerEligible] && !panel.visible && _panel == (MiloPreviewPanel *)panel);
    assert(![self proofCurrent:generation started:NSProcessInfo.processInfo.systemUptime]);
    _panel = nil;
}

- (void)exerciseClockJump:(int)writer {
    static const char show[] = "show 123 tile 500000 0 500000 1000000";
    WriteStamped(writer, show, 1000);
    [self readState];
    assert([self observerEligible] && _lease.checked == 100);
    WatcherFixturePanel *panel = [WatcherFixturePanel new];
    panel.placementProven = panel.visible = YES;
    _panel = (MiloPreviewPanel *)panel;
    NSUInteger generation = _generation;

    // Forty continuous seconds elapsed while the watcher accumulated old
    // evidence. The wall clock stepped back to 1020; 1019 looks fresh if
    // we compute wall age alone. Reject it before handling input or proof.
    WriteStamped(writer, show, 1019);
    WriteStamped(writer, "renew", 1019);
    fixtureClock.continuous = 140 * NSEC_PER_SEC;
    fixtureClock.realtime = 1020 * NSEC_PER_SEC;
    assert(![self acceptsPanelInput]);
    assert(!_finished && _clock.rejectThrough >= 1040 && !_lease.announcedAt && ![self observerEligible]);
    assert(!panel.visible && !panel.placementProven && _panel == (MiloPreviewPanel *)panel);
    assert(![self proofCurrent:generation started:NSProcessInfo.processInfo.systemUptime]);

    fixtureClock.continuous = 160 * NSEC_PER_SEC;
    fixtureClock.realtime = 1040 * NSEC_PER_SEC;
    WriteStamped(writer, show, 1040);
    [self readState];
    assert(![self observerEligible] && !_lease.announcedAt);
    fixtureClock.continuous = 161 * NSEC_PER_SEC;
    fixtureClock.realtime = 1041 * NSEC_PER_SEC;
    WriteStamped(writer, "renew", 1041);
    [self readState];
    assert(![self observerEligible]);
    generation = _generation;
    WriteStamped(writer, show, 1041);
    assert(![self proofCurrent:generation started:NSProcessInfo.processInfo.systemUptime]);
    assert([self observerEligible] && !panel.visible && _panel == (MiloPreviewPanel *)panel);
    assert([self proofCurrent:_generation started:NSProcessInfo.processInfo.systemUptime]);
    _panel = nil;
}
@end

static void WatcherDrain(void (^exercise)(WatcherFixtureAnchor *, int)) {
    int pipeFD[2];
    assert(pipe(pipeFD) == 0 && fcntl(pipeFD[0], F_SETFL, O_NONBLOCK) == 0);
    WatcherFixtureAnchor *anchor = [[WatcherFixtureAnchor alloc] initWithURL:nil
        rex:(NSRunningApplication *)[WatcherFixtureOwner new] pinned:1 stateFD:pipeFD[0] focusFD:-1 parent:getppid()];
    exercise(anchor, pipeFD[1]);
    close(pipeFD[1]);
    close(pipeFD[0]);
}

int main(int argc, const char *argv[]) {
    @autoreleasepool {
        if (argc == 2 && strcmp(argv[1], "--panel-staging") == 0) {
            PanelStaging();
            puts("preview panel staging passed");
            return 0;
        }
        assert(argc == 1);
        ScreenPlacement();
        PinLookupResults();
        WindowPinning();
        PaneOcclusion();
        PanelOrdering();
        RemapPinnedWindow();
        WatcherProtocol();
        WatcherLease();
        WatcherDrain(^(WatcherFixtureAnchor *anchor, int writer) { [anchor exerciseDrain:writer]; });
        WatcherDrain(^(WatcherFixtureAnchor *anchor, int writer) { [anchor exerciseBurst:writer]; });
        fixtureClock.continuous = 100 * NSEC_PER_SEC;
        fixtureClock.realtime = 1000 * NSEC_PER_SEC;
        fixtureClock.enabled = YES;
        WatcherDrain(^(WatcherFixtureAnchor *anchor, int writer) { [anchor exerciseClockJump:writer]; });
        fixtureClock.enabled = NO;
        FocusAcknowledgment();
        puts("preview geometry tests passed");
    }
    return 0;
}
