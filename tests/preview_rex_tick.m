// Two accessory processes own the disposable AppKit surfaces. The Rex app
// identity, foreground notification, and Chrome preferences are modeled; the
// windows' frames, ordering, visibility, Space membership and WKWebView are real.
#import <assert.h>
#import <fcntl.h>
#import <objc/runtime.h>
#import <mach/mach.h>
#import <stdarg.h>
#import <stdatomic.h>
#import <sys/resource.h>
#import <termios.h>
#import <unistd.h>
#import <WebKit/WebKit.h>
#import "../macos/Preview/Chrome.h"
#import "../macos/Preview/MenuStrip.h"

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
static BOOL FixtureSystemMenuOwner(pid_t);
static BOOL FixturePreviewProcess(pid_t);

#define CGWindowListCopyWindowInfo FixtureWindows
#define CGWindowListCreateDescriptionFromArray FixturePin
#define MiloApproximatePane FixturePane
#define MiloSystemMenuOwner FixtureSystemMenuOwner
#define MiloPreviewProcess FixturePreviewProcess
#define NSWorkspace FixtureWorkspace
#define windowNumberAtPoint fixtureWindowNumberAtPoint
#define mouseLocation fixtureMouseLocation
#import "../macos/Preview/Anchor.m"
#undef mouseLocation
#undef windowNumberAtPoint
#undef NSWorkspace
#undef MiloPreviewProcess
#undef MiloSystemMenuOwner
#undef MiloApproximatePane
#undef CGWindowListCreateDescriptionFromArray
#undef CGWindowListCopyWindowInfo

static pid_t ownerPID, menuPID, competingPID;
static CGWindowID targetID, coverID, replacementID;
static _Atomic(CGWindowID) panelID;
static atomic_bool heldSample;
static NSArray *preflightWindows;
static NSString *root;
static int trace;
static CGFloat quartzHeight;
static NSPoint parkedPointer;
static BOOL foreground = YES;

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

static CGRect QuartzFrame(NSRect frame) {
    return CGRectMake(NSMinX(frame), quartzHeight - NSMaxY(frame), NSWidth(frame), NSHeight(frame));
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
    NSArray *actual = CFBridgingRelease(CGWindowListCopyWindowInfo(options, relative));
    if (!actual) return NULL;
    CGWindowID preview = atomic_load(&panelID);
    NSMutableArray *windows = [NSMutableArray new];
    for (NSDictionary *row in actual) {
        CGWindowID id = [row[(__bridge NSString *)kCGWindowNumber] unsignedIntValue];
        if (id == targetID || id == replacementID) [windows addObject:WithOwner(row, ownerPID)];
        else if (id == coverID || id == preview) [windows addObject:row];
    }
    if (!NSThread.isMainThread && Flag(@"hold-sample")) {
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
    NSArray *rows = CFBridgingRelease(CGWindowListCreateDescriptionFromArray(ids));
    if (!rows.count) return CFBridgingRetain(rows);
    assert(rows.count == 1);
    return CFBridgingRetain(@[WithOwner(rows.firstObject, ownerPID)]);
}

static BOOL FixturePane(CGRect window, CGRect normalized, CGRect *pane) {
    MiloRexChrome chrome;
    return MiloDecodeRexChrome(@{}, &chrome) && MiloPaneInRexChrome(window, normalized, &chrome, pane);
}
static BOOL FixtureSystemMenuOwner(pid_t pid) { return pid == menuPID; }
static BOOL FixturePreviewProcess(pid_t pid) { return pid == competingPID; }

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
    NSRect coverFrame = NSInsetRect(frame, 30, 30);
    coverFrame.origin.x = NSMidX(frame);
    coverFrame.size.width = NSMaxX(frame) - NSMinX(coverFrame) - 20;
    NSPanel *target = LabWindow(frame, @"Milo Rex Lab • Rex stand-in", NSColor.darkGrayColor);
    NSPanel *cover = LabWindow(coverFrame, @"Milo Rex Lab • occluder", NSColor.grayColor);
    NSPanel *replacement = LabWindow(frame, @"Milo Rex Lab • replacement", NSColor.darkGrayColor);
    [target orderBack:nil];
    __block NSString *applied;
    __block BOOL announced;
    [NSTimer scheduledTimerWithTimeInterval:0.05 repeats:YES block:^(NSTimer *timer) {
        (void)timer;
        if (getppid() != parent) _exit(0);
        assert(!NSApp.active && !NSApp.keyWindow);
        if (!announced) {
            CFArrayRef list = CGWindowListCopyWindowInfo(kCGWindowListOptionOnScreenOnly |
                kCGWindowListExcludeDesktopElements, kCGNullWindowID);
            NSArray *rows = CFBridgingRelease(list);
            for (NSDictionary *row in rows) {
                if ([row[(__bridge NSString *)kCGWindowNumber] integerValue] != target.windowNumber) continue;
                assert(target.visible && target.onActiveSpace &&
                    CGRectEqualToRect(WindowBounds(row), QuartzFrame(target.frame)));
                Publish(@"window-ids", [NSString stringWithFormat:@"%ld %ld %ld %.0f\n",
                    (long)target.windowNumber, (long)cover.windowNumber, (long)replacement.windowNumber, NSWidth(target.frame)]);
                announced = YES;
                break;
            }
        }
        NSString *command = Read(@"window-command");
        if (!command || [command isEqual:applied]) return;
        if ([command isEqual:@"cover"]) {
            NSInteger preview = Read(@"panel-window").integerValue;
            assert(preview > 0);
            [cover orderWindow:NSWindowAbove relativeTo:preview];
            assert(cover.visible && cover.onActiveSpace && !cover.keyWindow);
        } else if ([command isEqual:@"uncover"]) {
            [cover orderOut:nil];
            assert(!cover.visible);
        } else {
            assert([command isEqual:@"replace"]);
            // Keep the pinned ID alive but offscreen. An eligible-looking
            // same-owner window must not be substituted for it.
            [target orderOut:nil];
            [replacement orderBack:nil];
            assert(!target.visible && replacement.visible && replacement.onActiveSpace && !replacement.keyWindow);
        }
        applied = command;
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

static void CheckShown(MiloPreviewPanel *panel) {
    AssertNonactivating(panel);
    assert(panel.visible && panel.onActiveSpace && panel.placementProven &&
        !panel.contentView.hidden && !panel.ignoresMouseEvents && panel.canBecomeKeyWindow);
    assert(parkedPointer.x + 120 < NSMinX(panel.frame) && !NSPointInRect(parkedPointer, panel.frame));
    CGRect bounds = QuartzFrame(panel.frame);
    Geometry stack = SampleGeometry(VisibleWindows(), ownerPID, targetID, getpid(), &bounds, NULL);
    assert(stack.windowID == targetID); // Ordered above that exact real window.
}

static void (*ActualOrder)(MiloPreviewPanel *, SEL, NSWindowOrderingMode, NSInteger);
static void ObserveOrder(MiloPreviewPanel *panel, SEL sel, NSWindowOrderingMode order, NSInteger relative) {
    AssertNonactivating(panel);
    // AppKit implements orderOut: through this same method. A conceal is
    // not an attempt to order the preview relative to a different window.
    if (order == NSWindowOut) {
        assert(!panel.placementProven && panel.ignoresMouseEvents);
        ActualOrder(panel, sel, order, relative);
        return;
    }
    assert(order == NSWindowAbove && relative == targetID);
    assert(!panel.placementProven && panel.contentView.hidden && panel.ignoresMouseEvents && !panel.canBecomeKeyWindow);
    CGWindowID window = (CGWindowID)panel.windowNumber;
    if (atomic_exchange(&panelID, window) != window) {
        Publish(@"panel-window", [NSString stringWithFormat:@"%u", window]);
    }
    ActualOrder(panel, sel, order, relative);
    assert(panel.visible);
    AssertNonactivating(panel);
    Trace("ordered\n");
}

static void (*ActualOut)(MiloPreviewPanel *, SEL, id);
static void ObserveOut(MiloPreviewPanel *panel, SEL sel, id sender) {
    BOOL wasVisible = panel.visible;
    ActualOut(panel, sel, sender);
    if (wasVisible) {
        AssertNonactivating(panel);
        assert(!panel.visible && !panel.placementProven && panel.contentView.hidden && panel.ignoresMouseEvents);
        Trace("out\n");
    }
}

static void (*ActualProven)(MiloPreviewPanel *, SEL, BOOL);
static void ObserveProven(MiloPreviewPanel *panel, SEL sel, BOOL proven) {
    BOOL changed = panel.placementProven != proven;
    ActualProven(panel, sel, proven);
    if (!changed) return;
    if (!proven) {
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

static void Probe(MiloPreviewPanel *panel, NSInteger sequence) {
    CheckShown(panel);
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
- (void)applySample:(Geometry)geometry checkedPanel:(BOOL)checkedPanel lostPin:(BOOL)lostPin
    generation:(NSUInteger)generation started:(NSTimeInterval)started {
    [super applySample:geometry checkedPanel:checkedPanel lostPin:lostPin generation:generation started:started];
    if (atomic_exchange(&heldSample, false)) {
        assert(!_panel.placementProven && !_panel.visible);
        Trace("sample-rejected\n");
    }
}

- (void)tick {
    BOOL active = !Flag(@"foreground-away");
    if (active != foreground) {
        foreground = active;
        [FixtureWorkspace.sharedWorkspace.notificationCenter postNotificationName:active ?
            NSWorkspaceDidActivateApplicationNotification : NSWorkspaceDidDeactivateApplicationNotification object:nil];
    }
    [super tick];
    if (!_panel.placementProven) return;
    static BOOL seeding;
    if (!seeding && Flag(@"seed-form")) {
        seeding = YES;
        SeedForm(_panel, 80, NO);
    }
    static NSInteger lastProbe;
    NSInteger sequence = Read(@"probe").integerValue;
    if (lastProbe < sequence) {
        lastProbe = sequence;
        Probe(_panel, sequence);
    }
}
@end

static NSDictionary *Metadata(pid_t pid, CGWindowID id, CGRect frame, int level) {
    return @{
        (__bridge NSString *)kCGWindowOwnerPID: @(pid),
        (__bridge NSString *)kCGWindowNumber: @(id),
        (__bridge NSString *)kCGWindowLayer: @(level),
        (__bridge NSString *)kCGWindowAlpha: @1,
        (__bridge NSString *)kCGWindowBounds: CFBridgingRelease(CGRectCreateDictionaryRepresentation(frame)),
    };
}

static int Preflight(const char *scenario, NSScreen *screen) {
    if (strcmp(scenario, "live")) {
        // Reproduce menu and unsupported layouts with metadata, never order
        // windows over the user's menu or change display state.
        CGRect display = QuartzFrame(screen.frame);
        NSRect frame = NSIntegralRect(NSInsetRect(screen.visibleFrame, 25, 25));
        frame.size.height = NSMaxY(screen.frame) - NSMinY(frame) - 1;
        CGRect targetFrame = QuartzFrame(frame), workspace;
        assert(FixturePane(targetFrame, CGRectMake(0, 0, 1, 1), &workspace));
        CGRect strip = CGRectMake(CGRectGetMinX(display), CGRectGetMinY(display), CGRectGetWidth(display), 29);
        assert(!CGRectIsEmpty(CGRectIntersection(strip, targetFrame)) && CGRectIsEmpty(CGRectIntersection(strip, workspace)));
        int normal = CGWindowLevelForKey(kCGNormalWindowLevelKey), menu = CGWindowLevelForKey(kCGMainMenuWindowLevelKey);
        NSDictionary *target = Metadata(ownerPID, targetID, targetFrame, normal);
        NSDictionary *stripRow = Metadata(menuPID, coverID, strip, menu);
        if (!strcmp(scenario, "menu")) preflightWindows = @[stripRow, target];
        else if (!strcmp(scenario, "menu-lookalike")) preflightWindows = @[Metadata(1, coverID, strip, menu), target];
        else if (!strcmp(scenario, "helper-status")) preflightWindows = @[
            Metadata(competingPID, coverID, strip, CGWindowLevelForKey(kCGStatusWindowLevelKey)), target];
        else if (!strcmp(scenario, "second-rex")) preflightWindows = @[
            stripRow, target, Metadata(ownerPID, replacementID, targetFrame, normal)];
        else if (!strcmp(scenario, "body-overlay")) {
            CGRect overlay = CGRectOffset(strip, 0, CGRectGetMinY(workspace) - CGRectGetMinY(strip) + 1);
            preflightWindows = @[Metadata(menuPID, coverID, overlay, menu), target];
        } else if (!strcmp(scenario, "competing-preview")) {
            CGRect panel = CGRectInset(workspace, CGRectGetWidth(workspace) / 4, CGRectGetHeight(workspace) / 4);
            preflightWindows = @[stripRow, Metadata(competingPID, replacementID, panel, normal), target];
        } else if (!strcmp(scenario, "background")) {
            foreground = NO;
            preflightWindows = @[target];
        } else {
            assert(!strcmp(scenario, "fullscreen"));
            preflightWindows = @[Metadata(ownerPID, targetID, display, normal)];
        }
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
        if (argc == 3 && !strcmp(argv[1], "--window-host")) {
            root = [NSString stringWithUTF8String:argv[2]];
            return RunWindowHost(screen);
        }
        BOOL preflight = argc == 6 && !strcmp(argv[1], "--preflight");
        assert(preflight || argc == 5);
        root = [NSString stringWithUTF8String:argv[3]];
        assert(sscanf(Read(@"window-ids").UTF8String, "%u %u %u", &targetID, &coverID, &replacementID) == 3);
        assert(targetID && coverID && replacementID && targetID != coverID && targetID != replacementID && coverID != replacementID);
        ownerPID = (pid_t)strtol(argv[preflight ? 2 : 1], NULL, 10);
        assert(ownerPID > 1 && ownerPID != getpid());
        owner = [FixtureOwner new];
        owner.processIdentifier = ownerPID;
        if (preflight) {
            menuPID = getppid();
            competingPID = (pid_t)strtol(argv[4], NULL, 10);
            return Preflight(argv[5], screen);
        }

        assert(strtoul(argv[4], NULL, 10) == targetID);
        SetAccessory();
        // Never warp the user's pointer. The helper sees this parked point
        // far from any fixture tile if it ever queries the global pointer.
        parkedPointer = NSMakePoint(NSMinX(screen.visibleFrame) + 1, NSMinY(screen.visibleFrame) + 1);
        trace = open(Path(@"native.log").fileSystemRepresentation, O_WRONLY | O_CREAT | O_APPEND | O_CLOEXEC, 0600);
        assert(trace >= 0);
        ActualOrder = (void (*)(MiloPreviewPanel *, SEL, NSWindowOrderingMode, NSInteger))
            Hook(MiloPreviewPanel.class, @selector(orderWindow:relativeTo:), (IMP)ObserveOrder);
        ActualOut = (void (*)(MiloPreviewPanel *, SEL, id))
            Hook(MiloPreviewPanel.class, @selector(orderOut:), (IMP)ObserveOut);
        ActualProven = (void (*)(MiloPreviewPanel *, SEL, BOOL))
            Hook(MiloPreviewPanel.class, @selector(setPlacementProven:), (IMP)ObserveProven);

        int tty = open("/dev/tty", O_RDONLY | O_CLOEXEC | O_NOCTTY), focus[2];
        struct termios before, after;
        assert(tty >= 0 && tcgetpgrp(tty) == getpgrp() && tcgetattr(tty, &before) == 0 && pipe(focus) == 0);
        assert(fcntl(STDIN_FILENO, F_SETFL, O_NONBLOCK) == 0 && fcntl(focus[0], F_SETFL, O_NONBLOCK) == 0);
        NSURL *url = [NSURL URLWithString:[NSString stringWithUTF8String:argv[2]]];
        assert(url);
        MiloPreviewAnchor *anchor = [[PersistentFixtureAnchor alloc] initWithURL:url rex:(id)owner
            pinned:targetID stateFD:STDIN_FILENO focusFD:focus[0] parent:getppid()];
        int result = [anchor run];
        assert(tcgetattr(tty, &after) == 0 && after.c_iflag == before.c_iflag && after.c_oflag == before.c_oflag &&
            after.c_cflag == before.c_cflag && ((after.c_lflag ^ before.c_lflag) & ~PENDIN) == 0 &&
            memcmp(after.c_cc, before.c_cc, sizeof(before.c_cc)) == 0);
        Trace("stopped\n");
        return result;
    }
}
