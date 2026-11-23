// Built only by the disposable Rex lab. Run the real Anchor coordinator and
// Browser with the Rex pane as our controlling tty and the Lua watcher on stdin.
#import <assert.h>
#import <objc/runtime.h>
#import <os/lock.h>
#import <WebKit/WebKit.h>
#import "../macos/Preview/Chrome.h"
#import "../macos/Preview/Browser.h"

@interface FixtureWorkspace : NSObject
+ (instancetype)sharedWorkspace;
@property (nonatomic, readonly) NSRunningApplication *frontmostApplication;
@property (nonatomic, readonly) NSNotificationCenter *notificationCenter;
@end

@interface NSEvent (RexTickFixture)
+ (NSPoint)fixtureMouseLocation;
@end

@interface NSWindow (RexTickFixture)
+ (NSInteger)fixtureWindowNumberAtPoint:(NSPoint)point belowWindowWithWindowNumber:(NSInteger)below;
@end

static CFArrayRef FixtureWindows(CGWindowListOption, CGWindowID);
static CFArrayRef FixturePin(CFArrayRef);
static BOOL FixturePane(CGRect, CGRect, CGRect *);
static NSView *SlowStartContent(NSURL *url, BOOL (^input)(void), void (^focus)(void));
#define MiloPreviewContent SlowStartContent
#define CGWindowListCopyWindowInfo FixtureWindows
#define CGWindowListCreateDescriptionFromArray FixturePin
#define MiloApproximatePane FixturePane
#define NSWorkspace FixtureWorkspace
#define mouseLocation fixtureMouseLocation
#define windowNumberAtPoint fixtureWindowNumberAtPoint
#import "../macos/Preview/Anchor.m"
#undef MiloPreviewContent
#undef windowNumberAtPoint
#undef mouseLocation
#undef NSWorkspace
#undef MiloApproximatePane
#undef CGWindowListCreateDescriptionFromArray
#undef CGWindowListCopyWindowInfo

static const CGWindowID Target = UINT32_MAX;
static const NSInteger RelativeWindow = 42; // Never passed to any actual order API.
static const CGWindowID Panel = 43;
static pid_t ownerPID;
static CGRect targetFrame;
static CGFloat quartzHeight;
static NSPoint pointer;
static NSString *denyPlacement;
static NSString *delaySamples;
static int trace;
static os_unfair_lock sampleLock = OS_UNFAIR_LOCK_INIT;
static BOOL staged;
static CGRect stagedFrame;

// Only the sample consumed by FrontmostRex is replaced, not an app's identity.
// The PID is the independent, non-GUI peer that owns the disposable Rex blocks.
@interface FixtureOwner : NSObject
@property (nonatomic) pid_t processIdentifier;
@end
@implementation FixtureOwner
- (BOOL)isActive { return YES; }
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
- (NSRunningApplication *)frontmostApplication { return (NSRunningApplication *)owner; }
- (NSNotificationCenter *)notificationCenter {
    static NSNotificationCenter *notifications;
    static dispatch_once_t once;
    dispatch_once(&once, ^{ notifications = [NSNotificationCenter new]; });
    return notifications;
}
@end

static void Trace(const char *event) {
    assert(WriteAll(trace, event, strlen(event)));
}

// A slow first construction outlives the frame used to accept the command.
// The real browser must earn fresh placement proof, not a second PTY command.
static NSView *SlowStartContent(NSURL *url, BOOL (^input)(void), void (^focus)(void)) {
    usleep(700000);
    Trace("cold-start-delay\n");
    return MiloPreviewContent(url, input, focus);
}

static NSDictionary *Metadata(pid_t pid, CGWindowID window, CGRect frame) {
    return @{
        (__bridge NSString *)kCGWindowOwnerPID: @(pid),
        (__bridge NSString *)kCGWindowNumber: @(window),
        (__bridge NSString *)kCGWindowLayer: @(CGWindowLevelForKey(kCGNormalWindowLevelKey)),
        (__bridge NSString *)kCGWindowAlpha: @1,
        (__bridge NSString *)kCGWindowBounds: CFBridgingRelease(CGRectCreateDictionaryRepresentation(frame)),
    };
}

static CFArrayRef FixtureWindows(CGWindowListOption options, CGWindowID relative) {
    assert(options == (kCGWindowListOptionOnScreenOnly | kCGWindowListExcludeDesktopElements));
    assert(relative == kCGNullWindowID);
    os_unfair_lock_lock(&sampleLock);
    BOOL hasPanel = staged;
    CGRect frame = stagedFrame;
    os_unfair_lock_unlock(&sampleLock);
    if ([NSFileManager.defaultManager fileExistsAtPath:delaySamples]) {
        usleep(700000); // Deliberately outlive the production sample, without changing its clock.
        Trace("late-sample\n");
    }
    NSDictionary *target = Metadata(ownerPID, Target, targetFrame);
    NSArray *windows = @[target];
    if (hasPanel) {
        NSDictionary *panel = Metadata(getpid(), Panel, frame);
        BOOL deny = [NSFileManager.defaultManager fileExistsAtPath:denyPlacement];
        windows = deny ? @[target, panel] : @[panel, target];
    }
    Trace("sample\n");
    return CFBridgingRetain(windows);
}

static CFArrayRef FixturePin(CFArrayRef ids) {
    assert(CFArrayGetCount(ids) == 1 && (uintptr_t)CFArrayGetValueAtIndex(ids, 0) == Target);
    return CFBridgingRetain(@[Metadata(ownerPID, Target, targetFrame)]);
}

static BOOL FixturePane(CGRect window, CGRect normalized, CGRect *pane) {
    MiloRexChrome chrome;
    return MiloDecodeRexChrome(@{}, &chrome) && MiloPaneInRexChrome(window, normalized, &chrome, pane);
}

@implementation NSEvent (RexTickFixture)
+ (NSPoint)fixtureMouseLocation { return pointer; }
@end

@implementation NSWindow (RexTickFixture)
+ (NSInteger)fixtureWindowNumberAtPoint:(NSPoint)point belowWindowWithWindowNumber:(NSInteger)below {
    CGPoint quartz = CGPointMake(point.x, quartzHeight - point.y);
    return !below && CGRectContainsPoint(targetFrame, quartz) ? RelativeWindow : 0;
}
@end

static BOOL (*ActualVisible)(id, SEL);

static BOOL FixtureVisible(id object, SEL selector) {
    (void)object; (void)selector;
    os_unfair_lock_lock(&sampleLock);
    BOOL visible = staged;
    os_unfair_lock_unlock(&sampleLock);
    return visible;
}

static BOOL FixtureActiveSpace(id object, SEL selector) {
    (void)object; (void)selector;
    return YES;
}

static void FixtureOrder(MiloPreviewPanel *panel, SEL selector, NSWindowOrderingMode order, NSInteger relative) {
    (void)selector;
    assert(NSThread.isMainThread && !staged && order == NSWindowAbove && relative == RelativeWindow);
    assert(!panel.placementProven && panel.contentView.hidden && panel.ignoresMouseEvents && !panel.canBecomeKeyWindow);
    assert(!panel.keyWindow && !NSApp.keyWindow && (panel.styleMask & NSWindowStyleMaskNonactivatingPanel));
    assert(!ActualVisible(panel, @selector(isVisible)));
    NSRect frame = panel.frame;
    os_unfair_lock_lock(&sampleLock);
    stagedFrame = CGRectMake(NSMinX(frame), quartzHeight - NSMaxY(frame), NSWidth(frame), NSHeight(frame));
    staged = YES;
    os_unfair_lock_unlock(&sampleLock);
    // No order message reaches AppKit: the actual WKWebView and panel are always offscreen.
    char event[128];
    int length = snprintf(event, sizeof(event), "stage %.1f %.1f %.1f %.1f\n", NSMinX(frame), NSMinY(frame),
        NSWidth(frame), NSHeight(frame));
    assert(length > 0 && (size_t)length < sizeof(event));
    Trace(event);
}

static void FixtureOrderOut(MiloPreviewPanel *panel, SEL selector, id sender) {
    (void)selector; (void)sender;
    assert(NSThread.isMainThread);
    if (!staged) return;
    assert(!panel.placementProven && panel.contentView.hidden && panel.ignoresMouseEvents && !panel.keyWindow);
    os_unfair_lock_lock(&sampleLock);
    staged = NO;
    os_unfair_lock_unlock(&sampleLock);
    Trace("hidden\n");
}

static WKWebView *FindWebView(NSView *view) {
    if ([view isKindOfClass:WKWebView.class]) return (WKWebView *)view;
    for (NSView *child in view.subviews) {
        WKWebView *web = FindWebView(child);
        if (web) return web;
    }
    return nil;
}

static void VerifyPage(WKWebView *web, unsigned tries) {
    if (!tries) return;
    [web evaluateJavaScript:@"document.readyState === 'complete' && !!document.getElementById('rex-lab-page')"
        completionHandler:^(id result, NSError *error) {
        if (!error && [result isEqual:@YES]) {
            Trace("webkit-loaded\n");
            // A snapshot can be unavailable while ordered out. Record paint only
            // if pixels really contain the fixture page's green background.
            [web takeSnapshotWithConfiguration:nil completionHandler:^(NSImage *image, NSError *snapshotError) {
                NSBitmapImageRep *bitmap = image && !snapshotError ?
                    [NSBitmapImageRep imageRepWithData:image.TIFFRepresentation] : nil;
                NSColor *pixel = [[bitmap colorAtX:bitmap.pixelsWide / 2 y:bitmap.pixelsHigh / 2]
                    colorUsingColorSpace:NSColorSpace.sRGBColorSpace];
                BOOL green = pixel && pixel.greenComponent > 0.55 && pixel.redComponent < 0.2 && pixel.blueComponent < 0.5;
                Trace(green ? "webkit-offscreen-paint\n" : "webkit-snapshot-unavailable\n");
            }];
        } else {
            dispatch_after(dispatch_time(DISPATCH_TIME_NOW, 100 * NSEC_PER_MSEC), dispatch_get_main_queue(), ^{
                VerifyPage(web, tries - 1);
            });
        }
    }];
}

static void (*SetPlacementProven)(MiloPreviewPanel *, SEL, BOOL);
static void FixtureProven(MiloPreviewPanel *panel, SEL selector, BOOL proven) {
    BOOL changed = panel.placementProven != proven;
    SetPlacementProven(panel, selector, proven); // Observe the actual hide/reveal, don't replace it.
    assert(panel.contentView.hidden != proven && panel.ignoresMouseEvents != proven);
    if (!changed || !proven) return;
    assert(staged && !panel.keyWindow && !NSApp.keyWindow);
    assert(!ActualVisible(panel, @selector(isVisible)));
    [panel.contentView layoutSubtreeIfNeeded];
    Trace("revealed\n");
    static BOOL checkedWeb;
    WKWebView *web = FindWebView(panel.contentView);
    assert(web);
    if (!checkedWeb) {
        checkedWeb = YES;
        VerifyPage(web, 80);
    }
}

static IMP Replace(Class cls, SEL selector, IMP replacement) {
    Method method = class_getInstanceMethod(cls, selector);
    assert(method);
    IMP original = method_getImplementation(method);
    // Replacing an inherited method here does not modify its superclass.
    class_replaceMethod(cls, selector, replacement, method_getTypeEncoding(method));
    return original;
}

int main(int argc, const char **argv) {
    @autoreleasepool {
        assert(argc == 4);
        ownerPID = (pid_t)strtol(argv[1], NULL, 10);
        assert(ownerPID > 1 && ownerPID != getpid());
        assert([NSApplication.sharedApplication setActivationPolicy:NSApplicationActivationPolicyAccessory]);
        NSScreen *screen = NSScreen.screens.firstObject;
        assert(screen && NSWidth(screen.visibleFrame) > 400 && NSHeight(screen.visibleFrame) > 300);
        NSRect frame = NSIntegralRect(NSInsetRect(screen.visibleFrame, 25, 25));
        quartzHeight = CGRectGetHeight(CGDisplayBounds(CGMainDisplayID()));
        targetFrame = CGRectMake(NSMinX(frame), quartzHeight - NSMaxY(frame), NSWidth(frame), NSHeight(frame));
        pointer = NSMakePoint(NSMinX(frame) + NSWidth(frame) * 0.85, NSMidY(frame));
        NSString *root = [NSString stringWithUTF8String:argv[3]];
        denyPlacement = [root stringByAppendingPathComponent:@"deny-placement"];
        delaySamples = [root stringByAppendingPathComponent:@"delay-samples"];
        trace = open([root stringByAppendingPathComponent:@"native.log"].fileSystemRepresentation,
            O_WRONLY | O_CREAT | O_APPEND | O_CLOEXEC, 0600);
        assert(trace >= 0);
        owner = [FixtureOwner new];
        owner.processIdentifier = ownerPID;
        ActualVisible = (BOOL (*)(id, SEL))Replace(MiloPreviewPanel.class, @selector(isVisible), (IMP)FixtureVisible);
        Replace(MiloPreviewPanel.class, @selector(isOnActiveSpace), (IMP)FixtureActiveSpace);
        Replace(MiloPreviewPanel.class, @selector(orderWindow:relativeTo:), (IMP)FixtureOrder);
        Replace(MiloPreviewPanel.class, @selector(orderOut:), (IMP)FixtureOrderOut);
        SetPlacementProven = (void (*)(MiloPreviewPanel *, SEL, BOOL))Replace(MiloPreviewPanel.class,
            @selector(setPlacementProven:), (IMP)FixtureProven);

        int tty = OpenTTY("/dev/tty"), focus[2];
        assert(tty >= 0 && ForegroundCanonicalTTY(tty) && pipe(focus) == 0);
        assert(fcntl(STDIN_FILENO, F_SETFL, O_NONBLOCK) == 0 && fcntl(focus[0], F_SETFL, O_NONBLOCK) == 0);
        NSURL *url = [NSURL URLWithString:[NSString stringWithUTF8String:argv[2]]];
        assert(url);
        MiloPreviewAnchor *anchor = [[MiloPreviewAnchor alloc] initWithURL:url rex:(NSRunningApplication *)owner
            pinned:Target stateFD:STDIN_FILENO focusFD:focus[0] parent:getppid() tty:tty confirm:YES];
        int result = [anchor run];
        // The sampling queue can still have a completion in flight when run returns.
        return result;
    }
}
