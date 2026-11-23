// Separate accessory processes own the Rex stand-in, foreign surface and preview.
// Rex identity, foreground notification and chrome preferences are modeled;
// frames, z-order, visibility, Space membership and WKWebView are real.
#import <assert.h>
#import <fcntl.h>
#import <inttypes.h>
#import <objc/runtime.h>
#import <mach/mach.h>
#import <stdarg.h>
#import <stdatomic.h>
#import <sys/resource.h>
#import <termios.h>
#import <unistd.h>
#import <WebKit/WebKit.h>
#import "../macos/Preview/Chrome.h"

@interface FixtureWorkspace : NSObject
+ (instancetype)sharedWorkspace;
@property (nonatomic, readonly) NSRunningApplication *frontmostApplication;
@property (nonatomic, readonly) NSNotificationCenter *notificationCenter;
@end

@interface NSWindow (RexTickFixture)
+ (NSInteger)fixtureWindowNumberAtPoint:(NSPoint)point belowWindowWithWindowNumber:(NSInteger)below;
@end

@interface NSEvent (RexTickFixture)
+ (NSPoint)fixtureMouseLocation;
@end

static CFArrayRef FixtureWindows(CGWindowListOption, CGWindowID);
static CFArrayRef FixturePin(CFArrayRef);
static BOOL FixturePane(CGRect, CGRect, CGRect *);

#define CGWindowListCopyWindowInfo FixtureWindows
#define CGWindowListCreateDescriptionFromArray FixturePin
#define MiloApproximatePane FixturePane
#define NSWorkspace FixtureWorkspace
#define windowNumberAtPoint fixtureWindowNumberAtPoint
#define mouseLocation fixtureMouseLocation
#import "../macos/Preview/Anchor.m"
#undef mouseLocation
#undef windowNumberAtPoint
#undef NSWorkspace
#undef MiloApproximatePane
#undef CGWindowListCreateDescriptionFromArray
#undef CGWindowListCopyWindowInfo

static pid_t ownerPID;
static CGWindowID targetID, coverID, replacementID;
static _Atomic(CGWindowID) panelID;
static atomic_bool heldSample;
static NSArray *preflightWindows;
static NSString *root;
static int trace;
static CGFloat quartzHeight;
static NSPoint parkedPointer;
static BOOL foreground = YES;
static const char TileCovered[] = "another window overlaps the tile; waiting for clear placement";

@interface FixtureOwner : NSObject
@property (nonatomic) pid_t processIdentifier;
@end
@implementation FixtureOwner
- (BOOL)isActive { return foreground; }
- (BOOL)isTerminated { return NO; }
@end
static FixtureOwner *owner;

@implementation FixtureWorkspace
+ (instancetype)sharedWorkspace {
    static FixtureWorkspace *workspace;
    static dispatch_once_t once;
    dispatch_once(&once, ^{ workspace = [FixtureWorkspace new]; });
    return workspace;
}
- (NSRunningApplication *)frontmostApplication { return foreground ? (id)owner : nil; }
- (NSNotificationCenter *)notificationCenter {
    static NSNotificationCenter *notifications;
    static dispatch_once_t once;
    dispatch_once(&once, ^{ notifications = [NSNotificationCenter new]; });
    return notifications;
}
@end

@implementation NSEvent (RexTickFixture)
+ (NSPoint)fixtureMouseLocation { return parkedPointer; }
@end

static NSString *Path(NSString *name) { return [root stringByAppendingPathComponent:name]; }
static BOOL Flag(NSString *name) { return [NSFileManager.defaultManager fileExistsAtPath:Path(name)]; }
static NSString *Read(NSString *name) {
    return [NSString stringWithContentsOfFile:Path(name) encoding:NSUTF8StringEncoding error:NULL];
}
static void Publish(NSString *name, NSString *text) {
    assert([text writeToFile:Path(name) atomically:YES encoding:NSUTF8StringEncoding error:NULL]);
}
static void Trace(const char *format, ...) {
    char buffer[256];
    va_list args;
    va_start(args, format);
    int length = vsnprintf(buffer, sizeof(buffer), format, args);
    va_end(args);
    assert(length > 0 && (size_t)length < sizeof(buffer));
    ssize_t written;
    do { written = write(trace, buffer, (size_t)length); } while (written < 0 && errno == EINTR);
    assert(written == length);
}

// Benchmark mode adds no polling source. Both builds write to a disposable
// control directory, use the same monotonic host clock and run the real UI.
static int perfFD = -1;
static unsigned perfRight;
static uint64_t perfTicks;
static uint64_t perfExposureLosses;
static _Atomic(uint64_t) perfMainWindows, perfBackgroundWindows, perfPins;

static uint64_t PerfNow(void) { return clock_gettime_nsec_np(CLOCK_UPTIME_RAW); }

static void PerfRecord(const char *event, uint64_t when, const char *fields, ...) {
    char buffer[768];
    int head = snprintf(buffer, sizeof(buffer), "{\"event\":\"%s\",\"t_ns\":%" PRIu64, event, when);
    assert(head > 0 && (size_t)head < sizeof(buffer) - 2);
    va_list args;
    va_start(args, fields);
    int tail = vsnprintf(buffer + head, sizeof(buffer) - (size_t)head - 2, fields, args);
    va_end(args);
    assert(tail >= 0 && (size_t)tail < sizeof(buffer) - (size_t)head - 2);
    buffer[head + tail] = '}';
    buffer[head + tail + 1] = '\n';
    assert(WriteAll(perfFD, buffer, (size_t)(head + tail + 2)));
}

static uint64_t CPUTimeNS(struct timeval time) {
    return (uint64_t)time.tv_sec * NSEC_PER_SEC + (uint64_t)time.tv_usec * NSEC_PER_USEC;
}

static void PerfCounters(unsigned char marker) {
    uint64_t when = PerfNow();
    struct rusage usage;
    assert(getrusage(RUSAGE_SELF, &usage) == 0);

    // These are kernel-attributed wakeup classes for this helper task, not
    // run-loop iteration counts or wakeups of WebContent, GPU or WindowServer.
    task_power_info_data_t power;
    mach_msg_type_number_t count = TASK_POWER_INFO_COUNT;
    char interrupt[32] = "null", platformIdle[32] = "null", resident[32] = "null";
    if (task_info(mach_task_self(), TASK_POWER_INFO, (task_info_t)&power, &count) == KERN_SUCCESS &&
        count == TASK_POWER_INFO_COUNT) {
        snprintf(interrupt, sizeof(interrupt), "%" PRIu64, power.task_interrupt_wakeups);
        snprintf(platformIdle, sizeof(platformIdle), "%" PRIu64, power.task_platform_idle_wakeups);
    }
    mach_task_basic_info_data_t memory;
    count = MACH_TASK_BASIC_INFO_COUNT;
    if (task_info(mach_task_self(), MACH_TASK_BASIC_INFO, (task_info_t)&memory, &count) == KERN_SUCCESS &&
        count == MACH_TASK_BASIC_INFO_COUNT)
        snprintf(resident, sizeof(resident), "%" PRIu64, (uint64_t)memory.resident_size);

    uint64_t mainWindows = atomic_load_explicit(&perfMainWindows, memory_order_relaxed);
    uint64_t backgroundWindows = atomic_load_explicit(&perfBackgroundWindows, memory_order_relaxed);
    uint64_t pins = atomic_load_explicit(&perfPins, memory_order_relaxed);
    PerfRecord("counters", when,
        ",\"marker\":%u,\"snapshot_end_ns\":%" PRIu64 ",\"cpu_user_ns\":%" PRIu64 ",\"cpu_system_ns\":%" PRIu64
        ",\"interrupt_wakeups\":%s,\"platform_idle_wakeups\":%s,\"resident_bytes\":%s,\"tick_calls\":%" PRIu64
        ",\"quartz_window_samples\":%" PRIu64 ",\"quartz_background_samples\":%" PRIu64 ",\"quartz_pin_samples\":%" PRIu64,
        (unsigned)marker, PerfNow(), CPUTimeNS(usage.ru_utime), CPUTimeNS(usage.ru_stime),
        interrupt, platformIdle, resident, perfTicks, mainWindows + backgroundWindows, backgroundWindows, pins);
}

static void PerfVerifyFinalViewport(void);

static dispatch_source_t PerfStart(void) {
    const char *file = getenv("MILO_REX_TICK_PERF_FILE");
    if (!file) return nil;
    const char *page = getenv("MILO_REX_TICK_PERF_PAGE");
    assert(*file == '/' && page && (!strcmp(page, "light") || !strcmp(page, "busy")));
    perfFD = open(file, O_WRONLY | O_CREAT | O_APPEND | O_CLOEXEC, 0600);
    assert(perfFD >= 0);
    PerfRecord("start", PerfNow(), ",\"pid\":%d,\"page\":\"%s\",\"clock\":\"CLOCK_UPTIME_RAW\","
        "\"resource_scope\":\"preview_helper\"", getpid(), page);

    int control = open(Path(@"perf-control").fileSystemRepresentation, O_RDWR | O_NONBLOCK | O_CLOEXEC);
    struct stat metadata;
    assert(control >= 0 && fstat(control, &metadata) == 0 && S_ISFIFO(metadata.st_mode));
    dispatch_source_t source = dispatch_source_create(DISPATCH_SOURCE_TYPE_READ, (uintptr_t)control, 0,
        dispatch_get_main_queue());
    assert(source);
    dispatch_source_set_event_handler(source, ^{
        unsigned char markers[16];
        ssize_t n;
        do { n = read(control, markers, sizeof(markers)); } while (n < 0 && errno == EINTR);
        if (n < 0 && errno == EAGAIN) return;
        assert(n > 0);
        for (ssize_t i = 0; i < n; ++i) {
            if (markers[i] == 'V') PerfVerifyFinalViewport();
            else PerfCounters(markers[i]);
        }
    });
    dispatch_source_set_cancel_handler(source, ^{ close(control); });
    dispatch_resume(source);
    return source;
}

static CGRect QuartzFrame(NSRect frame) {
    return CGRectMake(NSMinX(frame), quartzHeight - NSMaxY(frame), NSWidth(frame), NSHeight(frame));
}

static NSArray *ActualWindows(void) {
    return CFBridgingRelease(CGWindowListCopyWindowInfo(
        kCGWindowListOptionOnScreenOnly | kCGWindowListExcludeDesktopElements, kCGNullWindowID));
}

static NSUInteger WindowIndex(NSArray *windows, CGWindowID id) {
    for (NSUInteger i = 0; i < windows.count; ++i)
        if ([windows[i][(__bridge NSString *)kCGWindowNumber] unsignedIntValue] == id) return i;
    return NSNotFound;
}

static void ReadWindowIDs(void) {
    assert(sscanf(Read(@"window-ids").UTF8String, "%u %u", &targetID, &replacementID) == 2);
    assert(targetID && replacementID && targetID != replacementID);
}

static NSDictionary *WithOwner(NSDictionary *window, pid_t pid) {
    NSMutableDictionary *row = [window mutableCopy];
    row[(__bridge NSString *)kCGWindowOwnerPID] = @(pid);
    return row;
}

// Filter out unrelated applications so the lab never depends on the user's
// windows. Do not fabricate z-order, frames, IDs or panel visibility.
static CFArrayRef FixtureWindows(CGWindowListOption options, CGWindowID relative) {
    assert(options == (kCGWindowListOptionOnScreenOnly | kCGWindowListExcludeDesktopElements));
    assert(relative == kCGNullWindowID);
    if (preflightWindows) return CFBridgingRetain(preflightWindows);
    if (perfFD >= 0) {
        _Atomic(uint64_t) *counter = NSThread.isMainThread ? &perfMainWindows : &perfBackgroundWindows;
        atomic_fetch_add_explicit(counter, 1, memory_order_relaxed);
    }
    NSArray *actual = CFBridgingRelease(CGWindowListCopyWindowInfo(options, relative));
    if (!actual) return NULL;
    CGWindowID preview = atomic_load(&panelID);
    NSMutableArray *windows = [NSMutableArray new];
    for (NSDictionary *row in actual) {
        CGWindowID id = [row[(__bridge NSString *)kCGWindowNumber] unsignedIntValue];
        if (id == targetID || id == replacementID) [windows addObject:WithOwner(row, ownerPID)];
        else if (id == coverID || id == preview) [windows addObject:row];
    }
    if (perfFD < 0 && !NSThread.isMainThread && Flag(@"hold-sample")) {
        // Preserve the actual captured stack, but deliver it after a real
        // observer revocation. No fake geometry or altered sample clock.
        Trace("sample-held\n");
        NSTimeInterval deadline = NSProcessInfo.processInfo.systemUptime + 15;
        while (Flag(@"hold-sample")) {
            assert(NSProcessInfo.processInfo.systemUptime < deadline);
            usleep(2000);
        }
        atomic_store(&heldSample, true);
    }
    return CFBridgingRetain(windows);
}

static CFArrayRef FixturePin(CFArrayRef ids) {
    assert(CFArrayGetCount(ids) == 1 && (uintptr_t)CFArrayGetValueAtIndex(ids, 0) == targetID);
    if (perfFD >= 0) atomic_fetch_add_explicit(&perfPins, 1, memory_order_relaxed);
    NSArray *rows = CFBridgingRelease(CGWindowListCreateDescriptionFromArray(ids));
    if (!rows.count) return CFBridgingRetain(rows);
    assert(rows.count == 1);
    return CFBridgingRetain(@[WithOwner(rows.firstObject, ownerPID)]);
}

static BOOL FixturePane(CGRect window, CGRect normalized, CGRect *pane) {
    MiloRexChrome chrome;
    return MiloDecodeRexChrome(@{}, &chrome) && MiloPaneInRexChrome(window, normalized, &chrome, pane);
}

static CGRect CheckCover(NSArray *windows, NSString *position, CGRect tile) {
    NSUInteger target = WindowIndex(windows, targetID), cover = WindowIndex(windows, coverID);
    assert(target != NSNotFound && cover < target);
    pid_t foreign = [windows[cover][(__bridge NSString *)kCGWindowOwnerPID] intValue];
    assert(foreign > 1 && foreign != ownerPID && foreign != getpid());
    CGRect host = WindowBounds(windows[target]), surface = WindowBounds(windows[cover]), pane;
    assert(CGRectContainsRect(host, surface) && FixturePane(host, tile, &pane));
    if ([position isEqual:@"tile"]) {
        assert(CGRectContainsRect(pane, surface));
    } else {
        assert(CGRectIsEmpty(CGRectIntersection(pane, surface)));
        if ([position isEqual:@"sibling"]) {
            CGRect sibling;
            assert(FixturePane(host, CGRectMake(0, 0, tile.origin.x, 1), &sibling));
            assert(CGRectContainsRect(sibling, surface));
        } else {
            CGRect workspace;
            assert([position isEqual:@"titlebar"] && FixturePane(host, CGRectMake(0, 0, 1, 1), &workspace));
            assert(CGRectGetMaxY(surface) <= CGRectGetMinY(workspace));
        }
    }
    return surface;
}

@implementation NSWindow (RexTickFixture)
+ (NSInteger)fixtureWindowNumberAtPoint:(NSPoint)point belowWindowWithWindowNumber:(NSInteger)below {
    assert(below == 0);
    NSInteger actual = [NSWindow windowNumberAtPoint:point belowWindowWithWindowNumber:below];
    if (actual == targetID || actual == coverID || actual == replacementID) {
        Trace("appkit-hit\n");
        return actual;
    }
    // If an unrelated window wins the global hit, use the front fixture
    // window at that point. Relative order still goes to AppKit with its real ID.
    Trace("filtered-hit\n");
    CGPoint quartz = CGPointMake(point.x, quartzHeight - point.y);
    for (NSDictionary *row in VisibleWindows()) {
        if (CGRectContainsPoint(WindowBounds(row), quartz))
            return [row[(__bridge NSString *)kCGWindowNumber] integerValue];
    }
    return 0;
}
@end

static void SetAccessory(void) {
    assert([NSApplication.sharedApplication setActivationPolicy:NSApplicationActivationPolicyAccessory]);
}

@interface LabSurface : NSPanel
@end
@implementation LabSurface
- (BOOL)canBecomeKeyWindow { return NO; }
- (BOOL)canBecomeMainWindow { return NO; }
@end

static NSPanel *LabWindow(NSRect frame, NSString *title, NSColor *color) {
    NSPanel *panel = [[LabSurface alloc] initWithContentRect:frame
        styleMask:NSWindowStyleMaskBorderless | NSWindowStyleMaskNonactivatingPanel
        backing:NSBackingStoreBuffered defer:NO];
    panel.title = title;
    panel.level = NSNormalWindowLevel;
    panel.backgroundColor = color;
    panel.hidesOnDeactivate = NO;
    panel.releasedWhenClosed = NO;
    panel.hasShadow = NO;
    panel.animationBehavior = NSWindowAnimationBehaviorNone;
    panel.ignoresMouseEvents = YES;
    return panel;
}

static int RunWindowHost(NSScreen *screen) {
    SetAccessory();
    const pid_t parent = getppid();
    NSRect usable = screen.visibleFrame;
    CGFloat w = fmin(NSWidth(usable) - 60, 720), h = fmin(NSHeight(usable) - 60, 430);
    assert(w > 380 && h > 240);
    NSRect frame = NSIntegralRect(NSMakeRect(NSMidX(usable) - w / 2, NSMidY(usable) - h / 2, w, h));
    NSPanel *target = LabWindow(frame, @"Milo Rex Lab • Rex stand-in", NSColor.darkGrayColor);
    NSPanel *replacement = LabWindow(frame, @"Milo Rex Lab • replacement", NSColor.darkGrayColor);
    [target orderBack:nil];
    __block BOOL replaced;
    __block BOOL announced;
    [NSTimer scheduledTimerWithTimeInterval:0.05 repeats:YES block:^(NSTimer *timer) {
        (void)timer;
        if (getppid() != parent) _exit(0);
        assert(!NSApp.active && !NSApp.keyWindow);
        if (!announced) {
            NSArray *rows = ActualWindows();
            NSUInteger index = WindowIndex(rows, (CGWindowID)target.windowNumber);
            if (index != NSNotFound) {
                assert(target.visible && target.onActiveSpace &&
                    CGRectEqualToRect(WindowBounds(rows[index]), QuartzFrame(target.frame)));
                Publish(@"window-ids", [NSString stringWithFormat:@"%ld %ld %.0f\n",
                    (long)target.windowNumber, (long)replacement.windowNumber, NSWidth(target.frame)]);
                announced = YES;
            }
        }
        NSString *command = Read(@"window-command");
        if (replaced || ![command isEqual:@"replace"]) return;
        // Keep the pinned ID alive but offscreen. An eligible-looking
        // same-owner window must not be substituted for it.
        [target orderOut:nil];
        [replacement orderBack:nil];
        assert(!target.visible && replacement.visible && replacement.onActiveSpace && !replacement.keyWindow);
        replaced = YES;
        Publish(@"window-applied", command);
    }];
    [NSApp run];
    return 0;
}

static NSRect CoverFrame(NSRect target, NSString *position) {
    BOOL titlebar = [position isEqual:@"titlebar"], sibling = [position isEqual:@"sibling"];
    assert(titlebar || sibling || [position isEqual:@"tile"]);
    CGFloat x = NSMinX(target) + NSWidth(target) * (sibling ? 0.20 : 0.87) - 24;
    CGFloat y = titlebar ? NSMaxY(target) - 32 : NSMinY(target) + NSHeight(target) * 0.38 - 10;
    return NSIntegralRect(NSMakeRect(x, y, 48, 20));
}

static int RunCoverHost(void) {
    SetAccessory();
    const pid_t parent = getppid();
    NSArray *windows = ActualWindows();
    NSUInteger index = WindowIndex(windows, targetID);
    assert(index != NSNotFound && [windows[index][(__bridge NSString *)kCGWindowOwnerPID] intValue] != getpid());
    CGRect bounds = WindowBounds(windows[index]);
    NSRect target = NSMakeRect(CGRectGetMinX(bounds), quartzHeight - CGRectGetMaxY(bounds),
        CGRectGetWidth(bounds), CGRectGetHeight(bounds));
    NSPanel *cover = LabWindow(CoverFrame(target, @"tile"), @"Milo Rex Lab • occluder", NSColor.grayColor);
    CGWindowID foreign = (CGWindowID)cover.windowNumber;
    assert(foreign && foreign != targetID && foreign != replacementID);
    Publish(@"cover-id", [NSString stringWithFormat:@"%u\n", foreign]);

    __block NSString *requested, *applied;
    __block CGWindowID above;
    [NSTimer scheduledTimerWithTimeInterval:0.05 repeats:YES block:^(NSTimer *timer) {
        (void)timer;
        if (getppid() != parent) _exit(0);
        assert(!NSApp.active && !NSApp.keyWindow);
        NSString *command = Read(@"window-command");
        if (!command || [command isEqual:@"replace"] || [command isEqual:applied]) return;
        if (![command isEqual:requested]) {
            requested = command;
            [cover setFrame:CoverFrame(target, command) display:YES];
            // Never order in front of unrelated windows. Once clear over the
            // titlebar, put the cover above the visible preview; the next
            // move onto the tile retains that order without bringing it front.
            above = targetID;
            if (!cover.visible || [command isEqual:@"titlebar"]) {
                CGWindowID preview = (CGWindowID)Read(@"panel-window").longLongValue;
                if (preview && WindowIndex(ActualWindows(), preview) != NSNotFound) above = preview;
                [cover orderWindow:NSWindowAbove relativeTo:above];
            }
        }
        assert(cover.visible && cover.onActiveSpace && !cover.keyWindow);
        NSArray *rows = ActualWindows();
        NSUInteger coverIndex = WindowIndex(rows, foreign), aboveIndex = WindowIndex(rows, above);
        if (aboveIndex == NSNotFound || coverIndex >= aboveIndex ||
            !CGRectEqualToRect(WindowBounds(rows[coverIndex]), QuartzFrame(cover.frame))) return;
        applied = requested;
        Publish(@"window-applied", applied);
    }];
    [NSApp run];
    return 0;
}

static WKWebView *FindWebView(NSView *view) {
    if ([view isKindOfClass:WKWebView.class]) return (WKWebView *)view;
    for (NSView *child in view.subviews) {
        WKWebView *web = FindWebView(child);
        if (web) return web;
    }
    return nil;
}

static __weak WKWebView *firstWeb;
static NSString *firstURL;
static NSString *const FormValue = @"Rex divider form state";

static void AssertNonactivating(MiloPreviewPanel *panel) {
    assert(NSThread.isMainThread && !NSApp.active && !NSApp.keyWindow && !panel.keyWindow);
    assert(!panel.canBecomeMainWindow && (panel.styleMask & NSWindowStyleMaskNonactivatingPanel));
}

static void CheckShown(MiloPreviewPanel *panel, CGRect tile) {
    AssertNonactivating(panel);
    assert(panel.visible && panel.onActiveSpace && panel.placementProven &&
        !panel.contentView.hidden && !panel.ignoresMouseEvents && panel.canBecomeKeyWindow);
    assert(parkedPointer.x + 120 < NSMinX(panel.frame) && !NSPointInRect(parkedPointer, panel.frame));
    CGRect bounds = QuartzFrame(panel.frame);
    NSArray *windows = VisibleWindows();
    NSString *position = Read(@"window-applied");
    CGRect foreign = CheckCover(windows, position, tile);
    assert(CGRectIsEmpty(CGRectIntersection(bounds, foreign)));
    NSUInteger target = WindowIndex(windows, targetID);
    NSUInteger helper = WindowIndex(windows, (CGWindowID)panel.windowNumber);
    assert(target != NSNotFound && helper < target);
    if ([position isEqual:@"titlebar"]) assert(WindowIndex(windows, coverID) < helper);
    Geometry stack = SampleGeometry(windows, ownerPID, targetID, getpid(), &bounds, &tile);
    NSRect frame;
    CGDirectDisplayID display;
    assert(stack.windowID == targetID && !ScreenFrameFailure(stack, &frame, &display));
    assert(NSEqualRects(frame, panel.frame));
}

static void PerfFrame(const char *event, uint64_t started, uint64_t when, unsigned right, NSRect frame) {
    PerfRecord(event, when, ",\"start_ns\":%" PRIu64 ",\"right\":%u,\"width_pt\":%.6f,\"height_pt\":%.6f",
        started, right, NSWidth(frame), NSHeight(frame));
}

static void (*ActualPanelFrame)(MiloPreviewPanel *, SEL, NSRect, BOOL);
static void ObservePanelFrame(MiloPreviewPanel *panel, SEL sel, NSRect frame, BOOL display) {
    NSRect before = panel.frame;
    BOOL changing = !NSEqualRects(before, frame);
    BOOL exposed = changing && panel.placementProven && panel.visible;
    uint64_t exposureLosses = perfExposureLosses;
    unsigned right = perfRight;
    uint64_t started = changing ? PerfNow() : 0;
    ActualPanelFrame(panel, sel, frame, display);
    if (changing) {
        uint64_t when = PerfNow();
        NSRect after = panel.frame;
        // Continued exposure is not a new proof. Never infer it from the
        // frame alone, including across reentrant conceal/order callbacks.
        BOOL continuous = exposed && perfExposureLosses == exposureLosses &&
            panel.placementProven && panel.visible && !NSEqualRects(before, after);
        PerfRecord("frame", when, ",\"start_ns\":%" PRIu64 ",\"right\":%u,\"width_pt\":%.6f,"
            "\"height_pt\":%.6f,\"visible_continuously\":%s",
            started, right, NSWidth(after), NSHeight(after), continuous ? "true" : "false");
    }
}

static void (*ActualWebFrame)(WKWebView *, SEL, NSRect);
static void ObserveWebFrame(WKWebView *web, SEL sel, NSRect frame) {
    BOOL resizing = !NSEqualSizes(web.frame.size, frame.size);
    unsigned right = perfRight;
    uint64_t started = resizing ? PerfNow() : 0;
    ActualWebFrame(web, sel, frame);
    if (resizing) PerfFrame("web_frame", started, PerfNow(), right, web.frame);
}

static void (*ActualOrder)(MiloPreviewPanel *, SEL, NSWindowOrderingMode, NSInteger);
static void ObserveOrder(MiloPreviewPanel *panel, SEL sel, NSWindowOrderingMode order, NSInteger relative) {
    AssertNonactivating(panel);
    // AppKit implements orderOut: through this same method. A conceal is
    // not an attempt to order the preview relative to a different window.
    if (order == NSWindowOut) {
        assert(!panel.placementProven && panel.ignoresMouseEvents);
        if (perfFD >= 0) ++perfExposureLosses;
        ActualOrder(panel, sel, order, relative);
        return;
    }
    assert(order == NSWindowAbove && relative == targetID);
    assert(!panel.placementProven && panel.contentView.hidden && panel.ignoresMouseEvents && !panel.canBecomeKeyWindow);
    CGWindowID window = (CGWindowID)panel.windowNumber;
    if (atomic_exchange(&panelID, window) != window) {
        Publish(@"panel-window", [NSString stringWithFormat:@"%u", window]);
    }
    unsigned right = perfRight;
    uint64_t started = perfFD >= 0 ? PerfNow() : 0;
    ActualOrder(panel, sel, order, relative);
    uint64_t when = perfFD >= 0 ? PerfNow() : 0;
    assert(panel.visible);
    AssertNonactivating(panel);
    if (when) PerfFrame("ordered", started, when, right, panel.frame);
    Trace("ordered\n");
}

static void (*ActualOut)(MiloPreviewPanel *, SEL, id);
static void ObserveOut(MiloPreviewPanel *panel, SEL sel, id sender) {
    BOOL wasVisible = panel.visible;
    unsigned right = perfRight;
    if (wasVisible && perfFD >= 0) ++perfExposureLosses;
    ActualOut(panel, sel, sender);
    if (wasVisible) {
        if (perfFD >= 0) PerfRecord("out", PerfNow(), ",\"right\":%u", right);
        AssertNonactivating(panel);
        assert(!panel.visible && !panel.placementProven && panel.contentView.hidden && panel.ignoresMouseEvents);
        Trace("out\n");
    }
}

static void (*ActualProven)(MiloPreviewPanel *, SEL, BOOL);
static void ObserveProven(MiloPreviewPanel *panel, SEL sel, BOOL proven) {
    BOOL changed = panel.placementProven != proven;
    unsigned right = perfRight;
    uint64_t started = changed && perfFD >= 0 ? PerfNow() : 0;
    if (started && !proven) ++perfExposureLosses;
    ActualProven(panel, sel, proven);
    if (!changed) return;
    uint64_t when = perfFD >= 0 ? PerfNow() : 0;
    if (!proven) {
        if (when) PerfRecord("concealed", when, ",\"right\":%u", right);
        Trace("concealed\n");
        return;
    }
    AssertNonactivating(panel);
    assert(panel.visible && panel.onActiveSpace && !panel.contentView.hidden &&
        !panel.ignoresMouseEvents && panel.canBecomeKeyWindow);
    WKWebView *web = FindWebView(panel.contentView);
    assert(web);
    static BOOL seen;
    if (!seen) { seen = YES; firstWeb = web; }
    else assert(web == firstWeb); // Also fails if the original WK instance deallocated.
    NSRect frame = panel.frame;
    // Placement is now proven and native content is exposed. This is not
    // a signal from the compositor or a measurement of physical pixels.
    if (when) PerfFrame("visible", started, when, right, frame);
    Trace("shown %p %.2f %.2f %.2f %.2f\n", (__bridge void *)web, NSMinX(frame), NSMinY(frame),
        NSWidth(frame), NSHeight(frame));
}

static IMP Hook(Class cls, SEL selector, IMP replacement) {
    Method method = class_getInstanceMethod(cls, selector);
    assert(method);
    IMP original = method_getImplementation(method);
    class_replaceMethod(cls, selector, replacement, method_getTypeEncoding(method));
    return original;
}

// One passive notification after WebKit computed the root layout box.
// Do not poll, force layout, or assume this is when the display presented it.
static NSString *const PerfViewportScript =
    @"(() => {"
     "const form = document.getElementById('divider-form');"
     "if (!form) return;"
     "const observer = new ResizeObserver(([entry]) => {"
     "  const {width, height} = entry.contentRect;"
     "  window.webkit.messageHandlers.miloRexViewport.postMessage(["
     "    innerWidth, innerHeight, width, height, performance.now(), performance.timeOrigin, form.isConnected"
     "  ]);"
     "});"
     "observer.observe(document.documentElement);"
     "})();";
static NSString *const PerfViewportMessage = @"miloRexViewport";
static __weak WKWebView *perfWeb;

// ResizeObserver can coalesce notifications in a non-key window. Verify
// the final DOM independently, after the passive timing window has ended.
static void PerfVerifyFinalViewport(void) {
    WKWebView *web = perfWeb;
    MiloPreviewPanel *panel = (MiloPreviewPanel *)web.window;
    assert(web && panel.placementProven && panel.visible && !panel.ignoresMouseEvents);
    NSSize expected = web.bounds.size;
    assert(fabs(NSWidth(panel.frame) - expected.width) <= 1);
    [web evaluateJavaScript:@"[innerWidth, innerHeight, document.documentElement.getBoundingClientRect().width,"
        " !!document.querySelector('#divider-form')]" completionHandler:^(id result, NSError *error) {
        assert(!error && [result count] == 4 && [result[3] isEqual:@YES]);
        assert(fabs([result[0] doubleValue] - expected.width) <= 1 &&
            fabs([result[1] doubleValue] - expected.height) <= 1 && [result[2] doubleValue] > 0);
        PerfRecord("final_viewport", PerfNow(), ",\"right\":%u", perfRight);
    }];
}

@interface PerfViewport : NSObject <WKScriptMessageHandler>
@end
@implementation PerfViewport {
    BOOL _ready;
    double _documentOrigin;
}
- (void)userContentController:(WKUserContentController *)controller didReceiveScriptMessage:(WKScriptMessage *)message {
    (void)controller;
    uint64_t when = PerfNow();
    assert(message.frameInfo.mainFrame && message.webView == perfWeb);
    assert([message.frameInfo.request.URL.path isEqualToString:@"/rex-lab-page"]);
    assert([message.body isKindOfClass:NSArray.class] && [message.body count] == 7);
    NSArray *values = message.body;
    assert([values[6] isEqual:@YES]);
    int width = [values[0] intValue], height = [values[1] intValue];
    double layoutWidth = [values[2] doubleValue], layoutHeight = [values[3] doubleValue];
    double webTime = [values[4] doubleValue], documentOrigin = [values[5] doubleValue];
    assert(isfinite(layoutWidth) && isfinite(layoutHeight) && isfinite(webTime) && isfinite(documentOrigin));
    if (width <= 0 || height <= 0 || layoutWidth <= 0) return;
    if (!_ready) _documentOrigin = documentOrigin;
    else assert(documentOrigin == _documentOrigin);
    PerfRecord(_ready ? "reflow" : "page_ready", when,
        ",\"width_css\":%d,\"height_css\":%d,\"layout_width_css\":%.6f,\"layout_height_css\":%.6f,\"web_time_ms\":%.6f",
        width, height, layoutWidth, layoutHeight, webTime);
    _ready = YES;
}
@end

static WKNavigation *(*ActualLoad)(WKWebView *, SEL, NSURLRequest *);
static WKNavigation *ObserveLoad(WKWebView *web, SEL sel, NSURLRequest *request) {
    // This is the actual preview's first navigation, not a second load or an
    // evaluateJavaScript round-trip after content should already be laid out.
    assert(!perfWeb && [request.URL.path isEqualToString:@"/rex-lab-page"]);
    perfWeb = web;
    WKUserContentController *controller = web.configuration.userContentController;
    WKContentWorld *world = [WKContentWorld worldWithName:@"Milo Rex Layout Fixture"];
    [controller addScriptMessageHandler:[PerfViewport new] contentWorld:world name:PerfViewportMessage];
    [controller addUserScript:[[WKUserScript alloc] initWithSource:PerfViewportScript
        injectionTime:WKUserScriptInjectionTimeAtDocumentEnd forMainFrameOnly:YES inContentWorld:world]];
    return ActualLoad(web, sel, request);
}

static void SeedForm(MiloPreviewPanel *panel, unsigned tries, BOOL navigating) {
    assert(tries && panel.placementProven);
    WKWebView *web = FindWebView(panel.contentView);
    assert(web && web == firstWeb);
    if (!navigating && [web.URL.path isEqualToString:@"/rex-lab-page"] && !web.loading) {
        [web evaluateJavaScript:@"document.getElementById('divider-link').click(); true"
            completionHandler:^(id result, NSError *error) {
                assert(!error && [result isEqual:@YES]);
                SeedForm(panel, tries - 1, YES);
            }];
        return;
    }
    if (navigating && [web.URL.path isEqualToString:@"/rex-lab-form"] && web.canGoBack && !web.loading) {
        firstURL = web.URL.absoluteString;
        assert([web.backForwardList.backItem.URL.path isEqualToString:@"/rex-lab-page"]);
        [web evaluateJavaScript:@"document.getElementById('divider-form').value = 'Rex divider form state'"
            completionHandler:^(id result, NSError *error) {
                assert(!error && [result isEqual:FormValue]);
                Trace("form-seeded\n");
            }];
        return;
    }
    dispatch_after(dispatch_time(DISPATCH_TIME_NOW, 100 * NSEC_PER_MSEC), dispatch_get_main_queue(), ^{
        SeedForm(panel, tries - 1, navigating);
    });
}

static void Probe(MiloPreviewPanel *panel, NSInteger sequence, CGRect tile) {
    CheckShown(panel, tile);
    [panel.contentView layoutSubtreeIfNeeded];
    WKWebView *web = FindWebView(panel.contentView);
    assert(web && web == firstWeb && web.canGoBack && !web.loading);
    assert([web.URL.absoluteString isEqual:firstURL] && [web.backForwardList.backItem.URL.path isEqual:@"/rex-lab-page"]);
    NSSize size = web.bounds.size;
    assert(size.width > 0 && size.height > 0);
    [web evaluateJavaScript:@"[location.href, document.getElementById('divider-form').value, innerWidth, innerHeight]"
        completionHandler:^(id result, NSError *error) {
            assert(!error && [result isKindOfClass:NSArray.class] && [result count] == 4);
            NSArray *state = result;
            assert([state[0] isEqual:firstURL] && [state[1] isEqual:FormValue]);
            int width = [state[2] intValue], height = [state[3] intValue];
            assert(fabs(size.width - width) <= 1 && fabs(size.height - height) <= 1);
            struct rusage usage;
            assert(getrusage(RUSAGE_SELF, &usage) == 0);
            double cpu = usage.ru_utime.tv_sec + usage.ru_stime.tv_sec +
                (usage.ru_utime.tv_usec + usage.ru_stime.tv_usec) / 1e6;
            mach_task_basic_info_data_t memory;
            mach_msg_type_number_t count = MACH_TASK_BASIC_INFO_COUNT;
            assert(task_info(mach_task_self(), MACH_TASK_BASIC_INFO, (task_info_t)&memory, &count) == KERN_SUCCESS);
            Trace("probe %ld %p %d %d %.6f %llu\n", (long)sequence, (__bridge void *)web, width, height, cpu,
                (unsigned long long)memory.resident_size);
        }];
}

@interface PersistentFixtureAnchor : MiloPreviewAnchor
@end
@implementation PersistentFixtureAnchor
- (void)handleLine:(size_t)length {
    if (perfFD < 0) {
        [super handleLine:length];
        return;
    }
    BOOL show = length >= 5 && !memcmp(_line, "show ", 5);
    uint64_t when = show ? PerfNow() : 0;
    CGRect before = _lease.tile;
    [super handleLine:length];
    if (!_lease.announcedAt) perfRight = 0;
    else if (show && _lease.checked) {
        perfRight = (unsigned)llround(_lease.tile.size.width * FractionScale);
        if (!CGRectEqualToRect(before, _lease.tile))
            PerfRecord("received", when, ",\"right\":%u", perfRight);
    }
}

- (void)applySample:(Geometry)geometry checkedPanel:(BOOL)checkedPanel lostPin:(BOOL)lostPin
    generation:(NSUInteger)generation started:(NSTimeInterval)started {
    [super applySample:geometry checkedPanel:checkedPanel lostPin:lostPin generation:generation started:started];
    if (perfFD < 0 && atomic_exchange(&heldSample, false)) {
        assert(!_panel.placementProven && !_panel.visible);
        Trace("sample-rejected\n");
    }
}

- (void)tick {
    if (perfFD >= 0) {
        ++perfTicks;
        [super tick];
        return;
    }
    BOOL active = !Flag(@"foreground-away");
    if (active != foreground) {
        foreground = active;
        [FixtureWorkspace.sharedWorkspace.notificationCenter postNotificationName:active ?
            NSWorkspaceDidActivateApplicationNotification : NSWorkspaceDidDeactivateApplicationNotification object:nil];
    }
    [super tick];
    if (!_panel.placementProven) {
        static NSInteger lastBlocked;
        NSInteger sequence = Read(@"blocked-probe").integerValue;
        if (lastBlocked < sequence && _lastReason && !strcmp(_lastReason, TileCovered)) {
            NSArray *windows = VisibleWindows();
            CheckCover(windows, @"tile", _lease.tile);
            assert(![self acceptsPanelInput]);
            if (_panel) {
                AssertNonactivating(_panel);
                assert(!_panel.visible && !_panel.canBecomeKeyWindow &&
                    _panel.contentView.hidden && _panel.ignoresMouseEvents);
                if (WindowIndex(windows, (CGWindowID)_panel.windowNumber) != NSNotFound) return;
            }
            lastBlocked = sequence;
            Trace("blocked %ld\n", (long)sequence);
        }
        return;
    }
    static BOOL seeding;
    if (!seeding && Flag(@"seed-form")) {
        seeding = YES;
        CheckShown(_panel, _lease.tile);
        SeedForm(_panel, 80, NO);
    }
    static NSInteger requestedProbe, lastProbe;
    static NSTimeInterval probeAfter;
    NSInteger sequence = Read(@"probe").integerValue;
    if (requestedProbe < sequence) {
        requestedProbe = sequence;
        probeAfter = NSProcessInfo.processInfo.systemUptime;
    }
    // The previous placement proof may predate the surface's latest move.
    if (lastProbe < requestedProbe && _lastSample > probeAfter) {
        lastProbe = requestedProbe;
        Probe(_panel, requestedProbe, _lease.tile);
    }
}
@end

static NSDictionary *Metadata(pid_t pid, CGWindowID id, CGRect frame) {
    return @{
        (__bridge NSString *)kCGWindowOwnerPID: @(pid),
        (__bridge NSString *)kCGWindowNumber: @(id),
        (__bridge NSString *)kCGWindowLayer: @(CGWindowLevelForKey(kCGNormalWindowLevelKey)),
        (__bridge NSString *)kCGWindowAlpha: @1,
        (__bridge NSString *)kCGWindowBounds: CFBridgingRelease(CGRectCreateDictionaryRepresentation(frame)),
    };
}

static int Preflight(NSString *scenario, NSScreen *screen, CGFloat right) {
    if ([scenario isEqual:@"background"]) {
        foreground = NO;
    } else if ([scenario isEqual:@"second-rex"]) {
        NSArray *windows = VisibleWindows();
        NSUInteger index = WindowIndex(windows, targetID);
        assert(index != NSNotFound);
        NSDictionary *target = windows[index];
        preflightWindows = @[target, Metadata(ownerPID, replacementID, WindowBounds(target))];
    } else if ([scenario isEqual:@"fullscreen"]) {
        // Unsupported display geometry without changing the user's display.
        preflightWindows = @[Metadata(ownerPID, targetID, QuartzFrame(screen.frame))];
    } else {
        // The same disposable foreign process later moves this real surface
        // while the watcher and AppKit coordinator remain running.
        CheckCover(VisibleWindows(), scenario, CGRectMake(1 - right, 0, right, 1));
    }
    const char *reason = NULL;
    CGWindowID pinned = PinForegroundWindow((id)owner, ownerPID, &reason);
    if (pinned) {
        assert(pinned == targetID && !reason);
        printf("%u\n", pinned);
        return 0;
    }
    fprintf(stderr, "%s\n", reason ?: "missing pin refusal");
    return 1;
}

int main(int argc, const char **argv) {
    @autoreleasepool {
        NSScreen *screen = NSScreen.screens.firstObject;
        CGDirectDisplayID primary = CGMainDisplayID();
        assert(screen && [screen.deviceDescription[@"NSScreenNumber"] unsignedIntValue] == primary);
        quartzHeight = CGRectGetHeight(CGDisplayBounds(primary));
        if (argc == 3) {
            root = [NSString stringWithUTF8String:argv[2]];
            if (!strcmp(argv[1], "--window-host")) return RunWindowHost(screen);
            assert(!strcmp(argv[1], "--cover-host"));
            ReadWindowIDs();
            return RunCoverHost();
        }
        BOOL preflight = argc == 6 && !strcmp(argv[1], "--preflight");
        assert(preflight || argc == 5);
        root = [NSString stringWithUTF8String:argv[3]];
        ReadWindowIDs();
        coverID = (CGWindowID)Read(@"cover-id").longLongValue;
        assert(coverID && coverID != targetID && coverID != replacementID);
        ownerPID = (pid_t)strtol(argv[preflight ? 2 : 1], NULL, 10);
        assert(ownerPID > 1 && ownerPID != getpid());
        owner = [FixtureOwner new];
        owner.processIdentifier = ownerPID;
        if (preflight) return Preflight([NSString stringWithUTF8String:argv[4]], screen, strtod(argv[5], NULL));

        assert(strtoul(argv[4], NULL, 10) == targetID);
        dispatch_source_t perfControl = PerfStart();
        SetAccessory();
        // Park the modeled pointer inside the working shell throughout the
        // test. Its left third stays in the shell at every tested split ratio.
        NSArray *windows = VisibleWindows();
        NSUInteger target = WindowIndex(windows, targetID);
        CGRect shell;
        assert(target != NSNotFound && FixturePane(WindowBounds(windows[target]), CGRectMake(0, 0, 1.0/3, 1), &shell));
        parkedPointer = NSMakePoint(CGRectGetMidX(shell), quartzHeight - CGRectGetMidY(shell));
        trace = open(Path(@"native.log").fileSystemRepresentation, O_WRONLY | O_CREAT | O_APPEND | O_CLOEXEC, 0600);
        assert(trace >= 0);
        ActualOrder = (void (*)(MiloPreviewPanel *, SEL, NSWindowOrderingMode, NSInteger))
            Hook(MiloPreviewPanel.class, @selector(orderWindow:relativeTo:), (IMP)ObserveOrder);
        ActualOut = (void (*)(MiloPreviewPanel *, SEL, id))
            Hook(MiloPreviewPanel.class, @selector(orderOut:), (IMP)ObserveOut);
        ActualProven = (void (*)(MiloPreviewPanel *, SEL, BOOL))
            Hook(MiloPreviewPanel.class, @selector(setPlacementProven:), (IMP)ObserveProven);
        if (perfControl) {
            ActualPanelFrame = (void (*)(MiloPreviewPanel *, SEL, NSRect, BOOL))
                Hook(MiloPreviewPanel.class, @selector(setFrame:display:), (IMP)ObservePanelFrame);
            Class webClass = NSClassFromString(@"MiloPreviewWebView");
            assert(webClass);
            ActualWebFrame = (void (*)(WKWebView *, SEL, NSRect))
                Hook(webClass, @selector(setFrame:), (IMP)ObserveWebFrame);
            ActualLoad = (WKNavigation *(*)(WKWebView *, SEL, NSURLRequest *))
                Hook(webClass, @selector(loadRequest:), (IMP)ObserveLoad);
        }

        int tty = open("/dev/tty", O_RDONLY | O_CLOEXEC | O_NOCTTY), focus[2];
        struct termios before, after;
        assert(tty >= 0 && tcgetpgrp(tty) == getpgrp() && tcgetattr(tty, &before) == 0 && pipe(focus) == 0);
        assert(fcntl(STDIN_FILENO, F_SETFL, O_NONBLOCK) == 0 && fcntl(focus[0], F_SETFL, O_NONBLOCK) == 0);
        NSURL *url = [NSURL URLWithString:[NSString stringWithUTF8String:argv[2]]];
        assert(url);
        MiloPreviewAnchor *anchor = [[PersistentFixtureAnchor alloc] initWithURL:url rex:(id)owner
            pinned:targetID stateFD:STDIN_FILENO focusFD:focus[0] parent:getppid()];
        assert(anchor);
        int result = [anchor run];
        if (perfControl) dispatch_source_cancel(perfControl);
        assert(tcgetattr(tty, &after) == 0 && after.c_iflag == before.c_iflag && after.c_oflag == before.c_oflag &&
            after.c_cflag == before.c_cflag && ((after.c_lflag ^ before.c_lflag) & ~PENDIN) == 0 &&
            memcmp(after.c_cc, before.c_cc, sizeof(before.c_cc)) == 0);
        Trace("stopped\n");
        return result;
    }
}
