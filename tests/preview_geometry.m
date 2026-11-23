// macOS, no GUI interaction or permission needed:
// xcrun clang -fobjc-arc -mmacosx-version-min=13.0 tests/preview_geometry.m macos/Preview/Browser.m \
//   -framework AppKit -framework CoreGraphics -framework WebKit -o /tmp/preview-geometry
// /tmp/preview-geometry
// /tmp/preview-geometry --panel-staging additionally checks Quartz in a logged-in GUI session.
#import <assert.h>
#import <sys/socket.h>
#import <sys/wait.h>
#import <util.h>
#import "../macos/Preview/Chrome.h"
#import "../macos/Preview/ResizeInput.h"

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
    Geometry left = {CGRectMake(-1200, 220, 700, 500), CGRectMake(-1199.75, 230.25, 500.5, 400.25), 1, NULL};
    assert(PlaceOnScreen(left, primary, screens, 3, &overlay, &display));
    assert(display == 2 && NSEqualRects(overlay, NSMakeRect(-1199.5, 269.5, 500, 400)));

    Geometry above = {CGRectMake(1500, -920, 800, 600), CGRectMake(1510, -880, 790, 560), 1, NULL};
    assert(PlaceOnScreen(above, primary, screens, 3, &overlay, &display));
    assert(display == 3 && NSEqualRects(overlay, NSMakeRect(1510, 1220, 790, 560)));

    Geometry crosses = {CGRectMake(-200, 220, 700, 500), CGRectMake(0, 220, 400, 500), 1, NULL};
    assert(!PlaceOnScreen(crosses, primary, screens, 3, &overlay, &display));
    Geometry menuBar = {CGRectMake(100, 0, 700, 500), CGRectMake(100, 12, 400, 400), 1, NULL};
    assert(!PlaceOnScreen(menuBar, primary, screens, 3, &overlay, &display));
    Geometry fullScreen = {primary, CGRectMake(100, 50, 400, 400), 1, NULL};
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
    CGRect right = CGRectMake(0.5, 0, 0.5, 1);
    CGRect overlap = CGRectMake(700, 180, 100, 100);
    NSDictionary *rex = Metadata(100, 1, window);
    NSDictionary *otherRex = Metadata(100, 2, CGRectMake(0, 0, 10, 10));
    NSDictionary *occluding = Metadata(200, 3, overlap);
    NSDictionary *harmless = Metadata(200, 5, CGRectMake(22, 180, 100, 100));
    NSDictionary *wrongPID = Metadata(200, 1, window);
    NSDictionary *floating = Replacing(occluding, kCGWindowLayer, @(CGWindowLevelForKey(kCGNormalWindowLevelKey) + 25));

    assert(WindowID(@[rex], 0, NULL) == 1);
    assert(!WindowID(@[harmless, rex], 0, NULL)); // Preflight has no pane, so checks the window.
    assert(!WindowID(@[rex, otherRex], 0, NULL)); // Even a disjoint second Rex window is ambiguous.
    assert(!WindowID(@[otherRex, rex], 1, &right));
    NSDictionary *chrome = Replacing(otherRex, kCGWindowLayer, @25);
    assert(WindowID(@[chrome, rex], 1, &right) == 1); // Non-overlapping Rex chrome, not another normal window.
    assert(!WindowID(@[Replacing(chrome, kCGWindowBounds, occluding[(__bridge NSString *)kCGWindowBounds]), rex], 1, &right));
    assert(!WindowID(@[wrongPID, otherRex], 1, &right));
    assert(!WindowID(@[otherRex], 1, &right)); // Never use a replacement window.
    assert(!WindowID(@[Replacing(rex, kCGWindowLayer, @25)], 1, &right));
    assert(!WindowID(@[Replacing(rex, kCGWindowAlpha, @0)], 1, &right));
    assert(WindowID(@[Replacing(otherRex, kCGWindowAlpha, @0), rex], 1, &right) == 1);
    assert(!WindowID(@[occluding, rex], 1, &right));
    assert(!WindowID(@[floating, rex], 1, &right));
    assert(WindowID(@[Replacing(occluding, kCGWindowAlpha, @0), rex], 1, &right) == 1);
    assert(!WindowID(@[Replacing(occluding, kCGWindowAlpha, nil), rex], 1, &right));
    assert(!WindowID(@[Replacing(harmless, kCGWindowBounds, nil), rex], 1, &right));
    assert(!WindowID(@[rex, Replacing(harmless, kCGWindowLayer, nil)], 1, &right));
    assert(!WindowID(@[rex, Replacing(harmless, kCGWindowOwnerPID, @100.5)], 1, &right));
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
    [panel prepareStaging];
    assert(!panel.opaque && panel.backgroundColor.alphaComponent == 0 && panel.alphaValue == 1);
    assert(panel.contentView.hidden && panel.ignoresMouseEvents && !panel.canBecomeKeyWindow);
    [panel orderWindow:NSWindowAbove relativeTo:0];
    assert(panel.visible && !panel.keyWindow && panel.contentView.hidden);

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

    // Exercise reveal/revoke with the real panel ordered out, leaving the
    // fixture invisible and nonactivating throughout its run.
    panel.placementProven = YES;
    assert(!panel.contentView.hidden && !panel.ignoresMouseEvents && panel.canBecomeKeyWindow);
    panel.placementProven = NO;
    assert(panel.contentView.hidden && panel.ignoresMouseEvents && !panel.canBecomeKeyWindow);
}

static void RemapPinnedWindow(void) {
    CGRect right = CGRectMake(0.5, 0, 0.5, 1);
    CGRect window = CGRectMake(20, 40, 1000, 800);
    Geometry initial = SampleGeometry(@[Metadata(100, 1, window)], 100, 1, 300, NULL, &right);
    assert(initial.windowID == 1 && CGRectEqualToRect(initial.window, window));
    assert(CGRectEqualToRect(initial.pane, CGRectMake(547, 152, 451, 670)));

    CGRect moved = CGRectMake(-1200, 220, 800, 600);
    Geometry current = SampleGeometry(@[Metadata(100, 1, moved)], 100, 1, 300, NULL, &right);
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
    BOOL guarded;
    const char tile[] = "show 123 tile 500000 250000 500000 750000";
    assert(ReadTile(tile, sizeof(tile) - 1, &pid, &rect, &guarded) && !guarded);
    assert(pid == 123 && CGRectEqualToRect(rect, CGRectMake(0.5, 0.25, 0.5, 0.75)));
    const char guard[] = "guard 123 tile 500000 250000 500000 750000";
    assert(ReadTile(guard, sizeof(guard) - 1, &pid, &rect, &guarded) && guarded);
    assert(pid == 123 && CGRectEqualToRect(rect, CGRectMake(0.5, 0.25, 0.5, 0.75)));
    const char *invalid[] = {
        "show 123 tile", "show 123 float 0 0 1000000 1000000",
        "show 1 tile 0 0 10 10", "show +123 tile 0 0 10 10", "show 2147483648 tile 0 0 10 10",
        "show 123 tile 0 0 0 1", "show 123 tile 0 0 1 0", "show 123 tile 0 0 1000001 1",
        "show 123 tile 900000 0 100001 1", "show 123 tile 0 900000 1 100001",
        "show 123 tile 0.5 0 10 10", "show 123 tile -1 0 10 10", "show 123 tile 0  0 10 10",
        "show 123 tile 0 0 10 10 ", "show 123 tile 0 0 10 10\r", "show 123 tile 0 0 10 10 10",
        "guard", "guard 1 tile 0 0 10 10", "guard 123 tile 0 0 0 10", "guarded 123 tile 0 0 10 10",
    };
    for (size_t i = 0; i < sizeof(invalid) / sizeof(invalid[0]); ++i) {
        assert(!ReadTile(invalid[i], strlen(invalid[i]), &pid, &rect, &guarded));
    }
    const char embedded[] = "show 123 tile 0 0 10 10\0hide";
    assert(!ReadTile(embedded, sizeof(embedded) - 1, &pid, &rect, &guarded));
    assert(SampleFresh(1.5, 1) && !SampleFresh(1.75, 1) && !SampleFresh(1, 1.01));
}

static VisibilityUpdate PipeRecord(const int pipeFD[2], VisibilityLease *lease, NSTimeInterval now, const char *record) {
    size_t length = strlen(record);
    char line[64];
    assert(length && length <= sizeof(line) && record[length - 1] == '\n');
    assert(WriteAll(pipeFD[1], record, length));
    assert(read(pipeFD[0], line, sizeof(line)) == (ssize_t)length);
    return UpdateVisibility(lease, now, 123, line, length - 1);
}

static void WatcherLease(void) {
    int pipeFD[2];
    assert(pipe(pipeFD) == 0 && fcntl(pipeFD[0], F_SETFL, O_NONBLOCK) == 0);
    VisibilityLease lease = {0};
    const char show[] = "show 123 tile 500000 250000 500000 750000\n";
    assert(PipeRecord(pipeFD, &lease, 9, "renew\n") == VisibilityUnchanged && !LeaseFresh(&lease, 9));
    assert(PipeRecord(pipeFD, &lease, 10, show) == VisibilityChanged && LeaseFresh(&lease, 10));
    assert(PipeRecord(pipeFD, &lease, 11, "renew\n") == VisibilityUnchanged);
    assert(LeaseFresh(&lease, 13.99) && !LeaseFresh(&lease, 10));

    // No EOF or hide arrives, and a successful current Quartz sample cannot extend the lease.
    char byte;
    assert(read(pipeFD[0], &byte, 1) < 0 && errno == EAGAIN);
    assert(SampleFresh(14, 13.875) && !LeaseFresh(&lease, 14));
    assert(PipeRecord(pipeFD, &lease, 15, "renew\n") == VisibilityChanged && LeaseFresh(&lease, 15));
    assert(CGRectEqualToRect(lease.tile, CGRectMake(0.5, 0.25, 0.5, 0.75)));
    assert(PipeRecord(pipeFD, &lease, 16, "hide\n") == VisibilityChanged);
    assert(PipeRecord(pipeFD, &lease, 17, "renew\n") == VisibilityUnchanged && !LeaseFresh(&lease, 17));
    assert(PipeRecord(pipeFD, &lease, 18, "show 124 tile 0 0 1 1\n") == VisibilityWrongOwner && !LeaseFresh(&lease, 18));
    assert(PipeRecord(pipeFD, &lease, 19, show) == VisibilityChanged && LeaseFresh(&lease, 19));
    assert(PipeRecord(pipeFD, &lease, 20, "renew 123\n") == VisibilityInvalid);
    assert(PipeRecord(pipeFD, &lease, 21, "unclaimed\n") == VisibilityUnclaimed);
    close(pipeFD[1]);
    close(pipeFD[0]);
}

static void FocusAcknowledgment(void) {
    int pipeFD[2];
    assert(pipe(pipeFD) == 0 && fcntl(pipeFD[0], F_SETFL, O_NONBLOCK) == 0);
    VisibilityLease lease = {0};
    assert(PipeRecord(pipeFD, &lease, 10, "show 123 tile 0 0 1000000 1000000\n") == VisibilityChanged);
    assert(LeaseAllowsSampling(&lease, 10, 0));

    NSTimeInterval deadline = 10 + FocusReplyLifetime;
    assert(ReadFocusReply(pipeFD[0], 11, &deadline) == FocusWaiting);
    assert(PipeRecord(pipeFD, &lease, 23, "renew\n") == VisibilityChanged);
    assert(LeaseFresh(&lease, 23) && !LeaseAllowsSampling(&lease, 23, deadline));
    assert(ReadFocusReply(pipeFD[0], 23, &deadline) == FocusWaiting);

    assert(WriteAll(pipeFD[1], "1", 1));
    assert(ReadFocusReply(pipeFD[0], 24, &deadline) == FocusApproved && deadline == 0);
    assert(LeaseAllowsSampling(&lease, 24, deadline));
    assert(!LeaseAllowsSampling(&lease, 26, deadline)); // Approval doesn't extend the visibility lease.

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

static void GuardedLease(void) {
    int pipeFD[2];
    assert(pipe(pipeFD) == 0);
    const char show[] = "show 123 tile 0 0 500000 500000\n";
    const char guard[] = "guard 123 tile 0 0 500000 500000\n";
    VisibilityLease lease = {0};
    GuardedPeek peek = {0};
    assert(PipeRecord(pipeFD, &lease, 10, show) == VisibilityChanged);
    assert(ConsentAllowsReveal(&lease, &peek, 10, NO));
    assert(PipeRecord(pipeFD, &lease, 11, guard) == VisibilityChanged); // Same geometry, different mode.
    assert(LeaseAllowsSampling(&lease, 11, 0) && !ConsentAllowsReveal(&lease, &peek, 11, YES));
    assert(PipeRecord(pipeFD, &lease, 12, "renew\n") == VisibilityUnchanged);
    assert(PipeRecord(pipeFD, &lease, 13, guard) == VisibilityUnchanged);
    assert(!ConsentAllowsReveal(&lease, &peek, 13, YES)); // Neither record can reveal.
    assert(PipeRecord(pipeFD, &lease, 14, "guard 124 tile 0 0 500000 500000\n") == VisibilityWrongOwner);
    assert(PipeRecord(pipeFD, &lease, 15, "hide\n") == VisibilityChanged && lease.guarded);
    assert(PipeRecord(pipeFD, &lease, 16, "renew\n") == VisibilityUnchanged && !LeaseFresh(&lease, 16));
    assert(PipeRecord(pipeFD, &lease, 17, show) == VisibilityChanged && lease.guarded && !LeaseFresh(&lease, 17));
    assert(PipeRecord(pipeFD, &lease, 18, guard) == VisibilityChanged && LeaseFresh(&lease, 18));

    VisibilityLease hidden = {0}, initialConfirm = {.guarded = YES};
    assert(PipeRecord(pipeFD, &hidden, 20, "guard-hide\n") == VisibilityChanged && hidden.guarded);
    assert(!LeaseFresh(&hidden, 20));
    assert(PipeRecord(pipeFD, &hidden, 21, show) == VisibilityChanged && !LeaseFresh(&hidden, 21));
    assert(PipeRecord(pipeFD, &initialConfirm, 20, show) == VisibilityChanged);
    assert(!LeaseFresh(&initialConfirm, 20) && !ConsentAllowsReveal(&initialConfirm, &peek, 20, YES));
    close(pipeFD[1]);
    close(pipeFD[0]);
}

static void GuardedPresentation(void) {
    VisibilityLease lease = {.guarded = YES};
    GuardedPeek peek = {.ttyReady = YES, .announced = YES};
    StartPeek(&peek, 10);
    assert(peek.phase == PeekStarting && peek.deadline == 18);
    assert(ConsentAllowsReveal(&lease, &peek, 17, YES) && !ConsentAllowsReveal(&lease, &peek, 17, NO));
    assert(ShowPeek(&peek, 17.5));
    assert(peek.phase == PeekVisible && peek.deadline == 22);
    assert(PeekActive(&peek, 21.99) && !PeekActive(&peek, 22));
    assert(!ShowPeek(&peek, 21)); // A later verification cannot extend a visible peek.
    EndPeek(&peek, NO);
    assert(!peek.ttyReady && peek.announced && !ConsentAllowsReveal(&lease, &peek, 21, YES));
    StartPeek(&peek, 30);
    assert(!ShowPeek(&peek, 38) && !ConsentAllowsReveal(&lease, &peek, 38, YES));
    assert(!ShowPeek(&peek, 42));
}

static void TTYStep(int wire) {
    char ready;
    assert(WriteAll(wire, "?", 1));
    assert(read(wire, &ready, 1) == 1 && ready == '!');
}

static void GuardedTTYChild(int wire) {
    int tty = OpenTTY("/dev/tty");
    assert(tty >= 0);
    struct termios mode, before, after;
    assert(tcgetattr(tty, &mode) == 0);
    mode.c_lflag = (mode.c_lflag | ICANON) & ~(ECHO | ECHONL);
    mode.c_iflag = (mode.c_iflag | ICRNL) & ~(IGNCR | INLCR);
    assert(tcsetattr(tty, TCSANOW, &mode) == 0 && tcgetattr(tty, &before) == 0);
    GuardedPeek peek = {0};
    TTYStep(wire); // A complete and partial command typed before readiness.
    assert(ArmPeekTTY(&peek, tty) && ReadPeekInput(&peek, tty) == PeekNoInput);
    TTYStep(wire); // Return cannot complete the pretyped partial line.
    assert(ReadPeekInput(&peek, tty) == PeekIgnored);
    TTYStep(wire);
    assert(ReadPeekInput(&peek, tty) == PeekNoInput); // "sh" without Return.
    EndPeek(&peek, NO); // Losing only a geometry sample while idle doesn't rearm the TTY.
    assert(peek.ttyReady && peek.announced);
    TTYStep(wire);
    assert(ReadPeekInput(&peek, tty) == PeekShow); // "ow" and Return complete that line.
    StartPeek(&peek, 10);
    TTYStep(wire);
    assert(ReadPeekInput(&peek, tty) == PeekShow && peek.phase == PeekStarting && peek.deadline == 18);
    assert(ShowPeek(&peek, 13) && peek.deadline == 22);

    TTYStep(wire); // Hard revocation must discard both queued and partial stale commands.
    EndPeek(&peek, YES);
    assert(!peek.ttyReady && !PeekActive(&peek, 22));
    assert(ArmPeekTTY(&peek, tty) && ReadPeekInput(&peek, tty) == PeekNoInput);
    TTYStep(wire);
    assert(ReadPeekInput(&peek, tty) == PeekIgnored);
    TTYStep(wire); // The tail of a long canonical line is not a command.
    assert(ReadPeekInput(&peek, tty) == PeekIgnored && peek.discarding);
    assert(ReadPeekInput(&peek, tty) == PeekIgnored && !peek.discarding);
    TTYStep(wire);
    assert(ReadPeekInput(&peek, tty) == PeekShow);
    StartPeek(&peek, 40);
    TTYStep(wire);
    assert(ReadPeekInput(&peek, tty) == PeekHide);
    EndPeek(&peek, YES);
    assert(ArmPeekTTY(&peek, tty) && ReadPeekInput(&peek, tty) == PeekNoInput);
    TTYStep(wire);
    assert(ReadPeekInput(&peek, tty) == PeekIgnored);

    // Do not consume from a noncanonical or wrongly configured terminal.
    mode.c_lflag &= ~ICANON;
    assert(tcsetattr(tty, TCSANOW, &mode) == 0);
    assert(!ArmPeekTTY(&peek, tty) && ReadPeekInput(&peek, tty) == PeekBadTTY);
    mode = before;
    mode.c_iflag &= ~ICRNL;
    assert(tcsetattr(tty, TCSANOW, &mode) == 0);
    assert(!ArmPeekTTY(&peek, tty) && ReadPeekInput(&peek, tty) == PeekBadTTY);
    assert(tcsetattr(tty, TCSANOW, &before) == 0);
    int wrong[2];
    assert(pipe(wrong) == 0 && WriteAll(wrong[1], "show\n", 5));
    assert(!ArmPeekTTY(&peek, wrong[0]) && ReadPeekInput(&peek, wrong[0]) == PeekBadTTY);
    char unread[5];
    assert(read(wrong[0], unread, sizeof(unread)) == (ssize_t)sizeof(unread) && !memcmp(unread, "show\n", sizeof(unread)));
    close(wrong[0]);
    close(wrong[1]);
    TTYStep(wire);
    assert(ReadPeekInput(&peek, tty) == PeekShow);

    assert(tcgetattr(tty, &after) == 0);
    assert(before.c_iflag == after.c_iflag && before.c_oflag == after.c_oflag &&
        before.c_cflag == after.c_cflag && before.c_lflag == after.c_lflag &&
        !memcmp(before.c_cc, after.c_cc, sizeof(before.c_cc)) &&
        cfgetispeed(&before) == cfgetispeed(&after) && cfgetospeed(&before) == cfgetospeed(&after));
    close(tty);
}

static void GuardedTTY(void) {
    int wire[2], master;
    assert(socketpair(AF_UNIX, SOCK_STREAM, 0, wire) == 0);
    pid_t child = forkpty(&master, NULL, NULL, NULL);
    assert(child >= 0);
    if (!child) {
        close(wire[0]);
        GuardedTTYChild(wire[1]);
        _exit(0);
    }
    close(wire[1]);
    const char *input[] = {
        "show\rshow", "\r", "sh", "ow\r", "show\r", "show\rshow", "\r",
        "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx" "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx" "show\r", "show\r",
        "hide\rshow\rshow", "\r", "show\r",
    };
    for (size_t i = 0; i < sizeof(input) / sizeof(input[0]); ++i) {
        char step;
        assert(read(wire[0], &step, 1) == 1 && step == '?');
        assert(WriteAll(master, input[i], strlen(input[i])) && WriteAll(wire[0], "!", 1));
    }
    // Drain the one-shot readiness notice like a terminal viewer. Darwin waits for a
    // session leader's pending PTY output when it exits.
    char output[512] = {0};
    size_t used = 0;
    ssize_t count;
    while ((count = read(master, output + used, sizeof(output) - 1 - used)) > 0) {
        used += (size_t)count;
        assert(used < sizeof(output) - 1);
    }
    const char *ready = strstr(output, "Milo Preview: ready to confirm.");
    assert(ready && !strstr(ready + 1, "Milo Preview: ready to confirm."));
    int status;
    assert(waitpid(child, &status, 0) == child && WIFEXITED(status) && WEXITSTATUS(status) == 0);
    close(master);
    close(wire[0]);
}

static void ResizeExpectAt(int wire, ResizeInput *input, int tty, ResizeInputResult expected, int line) {
    TTYStep(wire);
    ResizeInputResult got = ReadResizeInput(input, tty);
    if (got != expected) fprintf(stderr, "resize fixture line %d: expected %d, got %d\n", line, expected, got);
    assert(got == expected);
}
#define ResizeExpect(wire, input, tty, expected) ResizeExpectAt(wire, input, tty, expected, __LINE__)

static void ResizeTTYRestored(int tty, const struct termios *before, int flags) {
    struct termios after;
    assert(tcgetattr(tty, &after) == 0);
    int restoredFlags = fcntl(tty, F_GETFL);
    // Check the public open modes, not private status bits Darwin sets
    // after the first write to a freshly opened /dev/tty descriptor.
    const int openModes = O_ACCMODE | O_NONBLOCK | O_APPEND | O_ASYNC;
    assert(restoredFlags >= 0 && (restoredFlags & openModes) == (flags & openModes));
    assert(before->c_iflag == after.c_iflag && before->c_oflag == after.c_oflag &&
        before->c_cflag == after.c_cflag && ((before->c_lflag ^ after.c_lflag) & ~PENDIN) == 0 &&
        !memcmp(before->c_cc, after.c_cc, sizeof(after.c_cc)) &&
        cfgetispeed(before) == cfgetispeed(&after) && cfgetospeed(before) == cfgetospeed(&after));
}

static void ResizeTTYChild(int wire) {
    int tty = OpenTTY("/dev/tty");
    assert(tty >= 0);
    int flags = fcntl(tty, F_GETFL);
    struct termios mode, before;
    assert(tcgetattr(tty, &mode) == 0);
    mode.c_lflag = (mode.c_lflag | ICANON) & ~(ECHO | ECHONL);
    mode.c_iflag = (mode.c_iflag | ICRNL) & ~(IGNCR | INLCR);
    assert(tcsetattr(tty, TCSANOW, &mode) == 0 && tcgetattr(tty, &before) == 0);
    ResizeInput input = {0};
    TTYStep(wire); // Drop complete lines and partial packets from before the new scope.
    assert(BeginResizeInput(&input, tty) && !ResizeInputReady(&input));
    assert(ReadResizeInput(&input, tty) == ResizeInputWaiting);
    ResizeExpect(wire, &input, tty, ResizeInputWaiting); // Replies plus a partial paste.
    assert(!ResizeInputReady(&input));
    ResizeExpect(wire, &input, tty, ResizeInputWaiting); // Finish the paste before arming.
    assert(ResizeInputReady(&input) && input.mode == ResizeMouse);
    ResizeExpect(wire, &input, tty, ResizeInputWaiting); // Split a valid motion packet.
    ResizeExpect(wire, &input, tty, ResizeInputMotion);
    ResizeExpect(wire, &input, tty, ResizeInputWaiting); // Release, drag, wheel and off-grid press.
    ResizeExpect(wire, &input, tty, ResizeInputExplicit); // Fresh left press.
    ResizeExpect(wire, &input, tty, ResizeInputWaiting); // Typed show.
    ResizeExpect(wire, &input, tty, ResizeInputWaiting); // DCS containing fake input and BEL.
    ResizeExpect(wire, &input, tty, ResizeInputWaiting); // Malformed CSI followed by Return.
    ResizeExpect(wire, &input, tty, ResizeInputWaiting); // Multiline paste with fake input.
    ResizeExpect(wire, &input, tty, ResizeInputDismiss); // A later hide wins over a press.
    ResizeExpect(wire, &input, tty, ResizeInputWaiting); // Leave an unfinished packet in the parser.
    assert(EndResizeInput(&input, tty) && EndResizeInput(&input, tty));
    ResizeTTYRestored(tty, &before, flags);

    TTYStep(wire); // Discard old packet tail and queued Return.
    assert(BeginResizeInput(&input, tty) && ReadResizeInput(&input, tty) == ResizeInputWaiting);
    ResizeExpect(wire, &input, tty, ResizeInputWaiting); // 2004 supported, 1003 unsupported.
    assert(ResizeInputReady(&input) && input.mode == ResizeCanonical);
    assert(tcgetattr(tty, &mode) == 0 && (mode.c_lflag & ICANON));
    ResizeExpect(wire, &input, tty, ResizeInputWaiting); // Paste's newline is not Return.
    ResizeExpect(wire, &input, tty, ResizeInputWaiting); // Typed show after the paste trailer.
    ResizeExpect(wire, &input, tty, ResizeInputExplicit); // Separate, empty canonical Return.
    ResizeExpect(wire, &input, tty, ResizeInputWaiting); // Eight reads cannot exhaust this burst.
    assert(ReadResizeInput(&input, tty) == ResizeInputDismiss);

    mode.c_lflag &= ~ICANON;
    assert(tcsetattr(tty, TCSANOW, &mode) == 0 && ReadResizeInput(&input, tty) == ResizeInputFailed);
    ResizeTTYRestored(tty, &before, flags);
    assert(BeginResizeInput(&input, tty));
    ResizeExpect(wire, &input, tty, ResizeInputFailed); // No verified bracketed paste.
    assert(!ResizeInputReady(&input) && EndResizeInput(&input, tty));
    ResizeTTYRestored(tty, &before, flags);
    close(tty);
}

static void GuardedResizeTTY(void) {
    int wire[2], master;
    assert(socketpair(AF_UNIX, SOCK_STREAM, 0, wire) == 0);
    struct winsize size = {.ws_row = 24, .ws_col = 80};
    int diagnostics = dup(STDERR_FILENO);
    assert(diagnostics >= 0);
    pid_t child = forkpty(&master, NULL, NULL, &size);
    assert(child >= 0);
    if (!child) {
        assert(dup2(diagnostics, STDERR_FILENO) >= 0);
        close(diagnostics);
        close(wire[0]);
        ResizeTTYChild(wire[1]);
        _exit(0);
    }
    close(diagnostics);
    close(wire[1]);
    const char *steps[] = {
        "show\r\033[<0;2;",
        "\033[?2004;1$y\033[?1006;1$y\033[?1003;1$y\033[200~\r",
        "\033[<0;2;3M\r\033[201~",
        "\033[<35;3;", "4M",
        "\033[<0;3;4m\033[<32;3;4M\033[<64;3;4M\033[<0;81;4M\033[<0;3;25M",
        "\033[<0;3;4M",
        "show\r",
        "\033PReply\a\033[<0;3;4M\r\r\033\\\r",
        "\033[<0;;4M\r",
        "\033[200~\r\033[<0;3;4M\rhide\r\033[201~",
        "\033[<0;3;4Mhide\r",
        "\033[<0;3;",
        "4M\r",
        "\033[?2004;1$y\033[?1006;1$y\033[?1003;0$y",
        "\033[200~\r\033[201~",
        "show\r",
        "\r",
        "\rx\rx\rx\rx\rx\rx\rx\rhide\r",
        "\033[?2004;0$y\033[?1006;1$y\033[?1003;1$y",
    };
    for (size_t i = 0; i < sizeof(steps) / sizeof(steps[0]); ++i) {
        char step;
        assert(read(wire[0], &step, 1) == 1 && step == '?');
        assert(WriteAll(master, steps[i], strlen(steps[i])) && WriteAll(wire[0], "!", 1));
    }
    char output[512] = {0};
    size_t used = 0;
    ssize_t count;
    while ((count = read(master, output + used, sizeof(output) - 1 - used)) > 0) {
        used += (size_t)count;
        assert(used < sizeof(output) - 1);
    }
    const char *save = "\033[?1003;1006;2004s";
    const char *restore = "\033[?1003;1006;2004r";
    const char *cursor = output;
    for (unsigned i = 0; i < 3; ++i) {
        cursor = strstr(cursor, save);
        assert(cursor && strstr(cursor, "\033[?2004h\033[?1006h\033[?1003h"));
        cursor = strstr(cursor, restore);
        assert(cursor);
        cursor += strlen(restore);
    }
    assert(!strstr(cursor, save) && !strstr(cursor, restore));
    assert(strstr(output, "\033[?1003;1006r"));
    int status;
    assert(waitpid(child, &status, 0) == child && WIFEXITED(status) && WEXITSTATUS(status) == 0);
    close(master);
    close(wire[0]);
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
        PanelOrdering();
        RemapPinnedWindow();
        WatcherProtocol();
        WatcherLease();
        FocusAcknowledgment();
        GuardedLease();
        GuardedPresentation();
        GuardedTTY();
        GuardedResizeTTY();
        puts("preview geometry tests passed");
    }
    return 0;
}
