// macOS, no GUI interaction or Accessibility permission needed:
// xcrun clang -fobjc-arc -mmacosx-version-min=13.0 tests/preview_geometry.m macos/Preview/Browser.m \
//   -framework AppKit -framework ApplicationServices -framework CoreGraphics -framework WebKit -o /tmp/preview-geometry
// /tmp/preview-geometry
#import <ApplicationServices/ApplicationServices.h>

// Interpose only the two synchronous reads under test. The production loops and budget are unchanged.
static AXError DelayedAttributes(AXUIElementRef, CFArrayRef, AXCopyMultipleAttributeOptions, CFArrayRef *);
static AXError DelayedHit(AXUIElementRef, float, float, AXUIElementRef *);
#define AXUIElementCopyMultipleAttributeValues DelayedAttributes
#define AXUIElementCopyElementAtPosition DelayedHit
#import "../macos/Preview/Anchor.m"
#undef AXUIElementCopyMultipleAttributeValues
#undef AXUIElementCopyElementAtPosition
#import <assert.h>
#import <util.h>

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

static NSDictionary *WithLayer(NSDictionary *metadata, NSNumber *layer) {
    NSMutableDictionary *copy = [metadata mutableCopy];
    copy[(__bridge NSString *)kCGWindowLayer] = layer;
    return copy;
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
    assert(MatchingVisibleWindow(@[ownPanel, harmless, rex, otherRexWindow], 100, 300, window, pane) == 1);
    assert(MatchingVisibleWindow(@[frontApp, rex], 100, 300, window, pane) == kCGNullWindowID);
    assert(MatchingVisibleWindow(@[otherRexWindow, rex], 100, 300, window, pane) == kCGNullWindowID);
    assert(MatchingVisibleWindow(@[rex, ambiguous], 100, 300, window, pane) == kCGNullWindowID);
    assert(MatchingVisibleWindow(@[rex, frontApp], 100, 300, window, pane) == 1);
    assert(MatchingVisibleWindow(@[Metadata(200, 1, window)], 100, 300, window, pane) == kCGNullWindowID);

    int normal = CGWindowLevelForKey(kCGNormalWindowLevelKey);
    NSDictionary *system = WithLayer(Metadata(200, 7, CGRectMake(0, 0, 1440, 900)), @(normal + 20));
    NSDictionary *rexPopup = WithLayer(Metadata(100, 8, pane), @(normal + 25));
    NSDictionary *ownPopup = Metadata(300, 9, pane);
    assert(MatchingVisibleWindow(@[system, rexPopup, ownPanel, ownPopup, rex], 100, 300, window, pane) == 1);
    assert(MatchingVisibleWindow(@[WithLayer(frontApp, @(normal - 1)), rex], 100, 300, window, pane) == kCGNullWindowID);
    assert(MatchingVisibleWindow(@[WithLayer(harmless, nil), rex], 100, 300, window, pane) == kCGNullWindowID);
    assert(MatchingVisibleWindow(@[WithLayer(rex, @(normal + 25))], 100, 300, window, pane) == kCGNullWindowID);
}

static void TerminalEcho(void) {
    int master, slave;
    char path[PATH_MAX];
    assert(openpty(&master, &slave, path, NULL, NULL) == 0);
    int writer = open(path, O_WRONLY | O_NOCTTY);
    assert(writer >= 0);
    struct termios original, saved, current;
    assert(tcgetattr(slave, &original) == 0);
    original.c_lflag |= ECHO | ECHONL;
    assert(tcsetattr(slave, TCSANOW, &original) == 0);
    assert(QuietTTY(writer, &saved));
    assert(tcgetattr(slave, &current) == 0);
    assert(current.c_lflag == (original.c_lflag & ~(ECHO | ECHONL)));
    assert(RestoreTTY(writer, &saved));
    assert(tcgetattr(slave, &current) == 0 && current.c_lflag == original.c_lflag);
    close(writer);
    close(slave);
    close(master);
}

static void StartupDeadline(void) {
    NSTimeInterval since = 0;
    for (int seconds = 10; seconds < 16; ++seconds) {
        assert(!LayoutDeadlineElapsed(seconds, &since, YES));
        assert(since == 10);
    }
    assert(LayoutDeadlineElapsed(16, &since, YES));
    assert(!LayoutDeadlineElapsed(17, &since, NO) && since == 0);
    assert(!LayoutDeadlineElapsed(30, &since, YES) && since == 30);
    assert(LayoutDeadlineElapsed(36, &since, YES));
}

static struct {
    NSTimeInterval now, delay;
    size_t calls;
} axFixture;

static NSTimeInterval TestClock(void *context) {
    return *(NSTimeInterval *)context;
}

static AXError DelayedAttributes(AXUIElementRef element, CFArrayRef keys, AXCopyMultipleAttributeOptions options, CFArrayRef *result) {
    (void)keys;
    (void)options;
    axFixture.now += axFixture.delay;
    NSString *role = (__bridge NSString *)(axFixture.calls++ ? kAXGroupRole : kAXWindowRole);
    NSArray *row = @[role, NSNull.null, NSNull.null, NSNull.null, @[(__bridge id)element]];
    *result = (CFArrayRef)CFBridgingRetain(row);
    return kAXErrorSuccess;
}

static AXError DelayedHit(AXUIElementRef element, float x, float y, AXUIElementRef *result) {
    (void)x;
    (void)y;
    ++axFixture.calls;
    axFixture.now += axFixture.delay;
    *result = element;
    CFRetain(element);
    return kAXErrorSuccess;
}

static void AcquisitionDeadline(void) {
    AXUIElementRef element = AXUIElementCreateApplication(getpid());
    AXBudget budget = {ProofLifetime, TestClock, &axFixture.now};
    CGRect pane = CGRectMake(0, 0, 20, 20);
    AXTree tree = {.markerNode = NoNode};

    // Each operation finishes within the per-message cap, but a long tree or hit test must stop.
    axFixture.delay = 0.045;
    assert(!WalkAX(&budget, &tree, element, NoNode, 0, @"unused"));
    assert(axFixture.calls == 7 && tree.count == 7 && BeforeAXDeadline(&budget) && !AXCanIssue(&budget));
    axFixture.now = 0;
    axFixture.calls = 0;
    assert(!HitTestsMatch(&budget, element, element, pane));
    assert(axFixture.calls == 7 && BeforeAXDeadline(&budget) && !AXCanIssue(&budget));

    // Each call gets the same deadline, including consecutive float background grids.
    axFixture.now = 0;
    axFixture.calls = 0;
    axFixture.delay = 0.020;
    assert(HitTestsMatch(&budget, element, element, pane));
    assert(!HitTestsMatch(&budget, element, element, pane));
    assert(axFixture.calls == 15 && !AXCanIssue(&budget));

    // An in-flight call returning late is never a successful sample, even if AX returns success.
    axFixture.now = 0;
    axFixture.calls = 0;
    axFixture.delay = ProofLifetime + 0.01;
    assert(!HitTestsMatch(&budget, element, element, pane));
    assert(axFixture.calls == 1 && !BeforeAXDeadline(&budget));
    CFRelease(element);
}

int main(void) {
    @autoreleasepool {
        ScreenPlacement();
        AXPaneOverlap();
        WindowOrdering();
        TerminalEcho();
        StartupDeadline();
        AcquisitionDeadline();
        puts("preview geometry tests passed");
    }
    return 0;
}
