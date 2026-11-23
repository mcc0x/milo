// macOS, no GUI interaction or permission needed:
// xcrun clang -fobjc-arc -mmacosx-version-min=13.0 tests/preview_geometry.m macos/Preview/Browser.m \
//   -framework AppKit -framework CoreGraphics -framework WebKit -framework Security -o /tmp/preview-geometry
// /tmp/preview-geometry
// /tmp/preview-geometry --panel-staging additionally checks Quartz in a logged-in GUI session.
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
    Geometry left = {CGRectMake(-1200, 220, 700, 500), CGRectMake(-1199.75, 230.25, 500.5, 400.25), 1, NULL};
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
    assert(!ScreenPlacementFailure(current, screens[0].frame, screens, 2, &frame, &display));
    assert(display == 2 && NSEqualRects(frame, NSMakeRect(-773, 98, 351, 470)));
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
    return UpdateVisibility(lease, now, 1700000000 + now, 123, line, length - 1);
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

@interface WatcherFixtureAnchor : MiloPreviewAnchor
- (void)exerciseDrain:(int)writer;
@end

@implementation WatcherFixtureAnchor
- (BOOL)canSample {
    return LeaseFresh(&_lease, LeaseNow());
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
@end

static void WatcherDrain(void) {
    int pipeFD[2];
    assert(pipe(pipeFD) == 0 && fcntl(pipeFD[0], F_SETFL, O_NONBLOCK) == 0);
    WatcherFixtureAnchor *anchor = [[WatcherFixtureAnchor alloc] initWithURL:nil
        rex:(NSRunningApplication *)[WatcherFixtureOwner new] pinned:1 stateFD:pipeFD[0] focusFD:-1 parent:getppid()];
    [anchor exerciseDrain:pipeFD[1]];
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
        PanelOrdering();
        RemapPinnedWindow();
        WatcherProtocol();
        WatcherLease();
        WatcherDrain();
        FocusAcknowledgment();
        puts("preview geometry tests passed");
    }
    return 0;
}
