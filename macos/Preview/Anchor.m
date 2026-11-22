#import "Anchor.h"
#import "Browser.h"
#import "Chrome.h"

#import <AppKit/AppKit.h>
#import <dispatch/dispatch.h>
#import <errno.h>
#import <fcntl.h>
#import <limits.h>
#import <signal.h>
#import <stdio.h>
#import <string.h>
#import <math.h>
#import <time.h>
#import <unistd.h>

static const NSTimeInterval SampleInterval = 0.25;
static const NSTimeInterval SampleLifetime = 0.60;
static const NSTimeInterval LeaseLifetime = 3.0;
enum { FractionScale = 1000000 };

typedef struct {
    CGRect window;
    CGRect pane;
    CGWindowID windowID;
} Geometry;

static BOOL SameGeometry(Geometry a, Geometry b) {
    return a.windowID == b.windowID && CGRectEqualToRect(a.window, b.window) && CGRectEqualToRect(a.pane, b.pane);
}

static BOOL SampleFresh(NSTimeInterval now, NSTimeInterval started) {
    return started > 0 && now >= started && now - started < SampleLifetime;
}

static BOOL ValidRect(CGRect r) {
    return isfinite(r.origin.x) && isfinite(r.origin.y) && isfinite(r.size.width) && isfinite(r.size.height) &&
        r.size.width > 0 && r.size.height > 0;
}

static BOOL HasType(id value, CFTypeID type) {
    return value != nil && CFGetTypeID((__bridge CFTypeRef)value) == type;
}

static BOOL MetadataNumber(NSDictionary *window, CFStringRef key, double *out) {
    id value = window[(__bridge NSString *)key];
    if (!HasType(value, CFNumberGetTypeID())) return NO;
    double number = [value doubleValue];
    if (!isfinite(number)) return NO;
    *out = number;
    return YES;
}

static BOOL WindowInteger(NSDictionary *window, CFStringRef key, int64_t lower, int64_t upper, int64_t *out) {
    double number;
    if (!MetadataNumber(window, key, &number) || number < lower || number > upper || trunc(number) != number) return NO;
    *out = (int64_t)number;
    return YES;
}

static BOOL WindowAlpha(NSDictionary *window, double *alpha) {
    return MetadataNumber(window, kCGWindowAlpha, alpha) && *alpha >= 0 && *alpha <= 1;
}

typedef NS_ENUM(NSUInteger, PinLookup) { PinUnavailable, PinMissing, PinFound };

static PinLookup PinnedOwner(NSArray *windows, CGWindowID pinned, pid_t *pid) {
    if (!windows) return PinUnavailable;
    if (!windows.count) return PinMissing;
    id window = windows.firstObject;
    int64_t number, owner;
    if (windows.count != 1 || ![window isKindOfClass:NSDictionary.class] ||
        !WindowInteger(window, kCGWindowNumber, 1, UINT32_MAX, &number) || number != pinned ||
        !WindowInteger(window, kCGWindowOwnerPID, 2, INT_MAX, &owner)) return PinUnavailable;
    *pid = (pid_t)owner;
    return PinFound;
}

static PinLookup LookupPin(CGWindowID pinned, pid_t *pid) {
    // Quartz accepts raw CGWindowID values in this array, not CFNumber objects.
    const void *identifier = (const void *)(uintptr_t)pinned;
    CFArrayRef ids = CFArrayCreate(NULL, &identifier, 1, NULL);
    if (!ids) return PinUnavailable;
    NSArray *windows = CFBridgingRelease(CGWindowListCreateDescriptionFromArray(ids));
    CFRelease(ids);
    return PinnedOwner(windows, pinned, pid);
}

static CGRect WindowBounds(NSDictionary *window) {
    id bounds = window[(__bridge NSString *)kCGWindowBounds];
    CGRect r;
    return HasType(bounds, CFDictionaryGetTypeID()) && CGRectMakeWithDictionaryRepresentation((__bridge CFDictionaryRef)bounds, &r) &&
        ValidRect(r) ? r : CGRectNull;
}

// Quartz supplies onscreen metadata in front-to-back order. A pinned window is
// never replaced by another window, even if that window is the only Rex window.
static Geometry SampleGeometry(NSArray *windows, pid_t rexPID, CGWindowID pinned, pid_t helperPID,
    const CGRect *orderedPanel, const CGRect *normalized) {
    Geometry none = {0}, geometry = {0};
    NSUInteger match = NSNotFound, panel = NSNotFound;
    int normal = CGWindowLevelForKey(kCGNormalWindowLevelKey);
    for (NSUInteger i = 0; i < windows.count; ++i) {
        NSDictionary *window = windows[i];
        int64_t owner, layer, number;
        if (![window isKindOfClass:NSDictionary.class] ||
            !WindowInteger(window, kCGWindowOwnerPID, 1, INT_MAX, &owner) ||
            !WindowInteger(window, kCGWindowLayer, INT_MIN, INT_MAX, &layer) ||
            !WindowInteger(window, kCGWindowNumber, 1, UINT32_MAX, &number)) return none;
        if (pinned && number == pinned && (owner != rexPID || layer != normal)) return none;
        if (orderedPanel && owner == helperPID && layer == normal) {
            double alpha;
            if (!WindowAlpha(window, &alpha)) return none;
            if (alpha != 0) {
                CGRect bounds = WindowBounds(window);
                // Some Quartz metadata rounds bounds outward to whole points.
                if (panel != NSNotFound || (alpha != 1) ||
                    (!CGRectEqualToRect(bounds, *orderedPanel) &&
                     !CGRectEqualToRect(bounds, CGRectIntegral(*orderedPanel)))) return none;
                panel = i;
            }
        }
        if (owner != rexPID || layer != normal) continue;
        double alpha;
        if (!WindowAlpha(window, &alpha)) return none;
        if (alpha == 0 && number != pinned) continue;
        if (alpha == 0 || (pinned && number != pinned) || match != NSNotFound) return none;
        CGRect bounds = WindowBounds(window);
        if (CGRectIsNull(bounds)) return none;
        geometry = (Geometry){bounds, bounds, (CGWindowID)number};
        match = i;
    }
    if (match == NSNotFound || (orderedPanel && (panel == NSNotFound || panel >= match)) ||
        (normalized && !MiloApproximatePane(geometry.window, *normalized, &geometry.pane))) return none;

    for (NSUInteger i = 0; i < match; ++i) {
        if (i == panel) continue;
        NSDictionary *window = windows[i];
        double alpha;
        if (!WindowAlpha(window, &alpha)) return none;
        if (alpha == 0) continue;
        CGRect bounds = WindowBounds(window);
        if (CGRectIsNull(bounds) || !CGRectIsEmpty(CGRectIntersection(bounds, geometry.pane))) return none;
    }
    return geometry;
}

static NSArray *VisibleWindows(void) {
    return CFBridgingRelease(CGWindowListCopyWindowInfo(
        kCGWindowListOptionOnScreenOnly | kCGWindowListExcludeDesktopElements, kCGNullWindowID));
}

static NSRunningApplication *RexApplication(pid_t pid) {
    if (pid < 2) return nil;
    NSRunningApplication *app = [NSRunningApplication runningApplicationWithProcessIdentifier:pid];
    return app && !app.terminated && [app.bundleIdentifier isEqualToString:@"com.superlogical.rex"] ? app : nil;
}

static BOOL FrontmostRex(NSRunningApplication *rex) {
    return rex && !rex.terminated && rex.active &&
        NSWorkspace.sharedWorkspace.frontmostApplication.processIdentifier == rex.processIdentifier;
}

static BOOL ReadDecimal(const char **cursor, const char *end, uint32_t limit, uint32_t *out) {
    const char *p = *cursor;
    if (p == end || *p < '0' || *p > '9') return NO;
    uint32_t value = 0;
    while (p != end && *p >= '0' && *p <= '9') {
        uint32_t digit = (uint32_t)(*p++ - '0');
        if (value > (limit - digit) / 10) return NO;
        value = value * 10 + digit;
    }
    *cursor = p;
    *out = value;
    return YES;
}

static BOOL ReadShow(const char *line, size_t length, pid_t *pid, CGRect *normalized) {
    const char *end = line + length;
    if (length < 5 || memcmp(line, "show ", 5)) return NO;
    const char *p = line + 5;
    uint32_t process, parts[4];
    if (!ReadDecimal(&p, end, INT_MAX, &process) || process < 2 ||
        end - p < 6 || memcmp(p, " tile ", 6)) return NO;
    p += 6;
    for (size_t i = 0; i < 4; ++i) {
        if (i && (p == end || *p++ != ' ')) return NO;
        if (!ReadDecimal(&p, end, FractionScale, &parts[i])) return NO;
    }
    if (p != end || !parts[2] || !parts[3] ||
        parts[0] + parts[2] > FractionScale || parts[1] + parts[3] > FractionScale) return NO;
    *pid = (pid_t)process;
    *normalized = CGRectMake((CGFloat)parts[0] / FractionScale, (CGFloat)parts[1] / FractionScale,
        (CGFloat)parts[2] / FractionScale, (CGFloat)parts[3] / FractionScale);
    return YES;
}

typedef struct {
    CGRect tile;
    NSTimeInterval checked;
} VisibilityLease;

static NSTimeInterval LeaseNow(void) {
    // Unlike uptime, this clock also counts time spent asleep.
    return (NSTimeInterval)clock_gettime_nsec_np(CLOCK_MONOTONIC_RAW) / NSEC_PER_SEC;
}

static BOOL LeaseFresh(const VisibilityLease *lease, NSTimeInterval now) {
    return lease->checked > 0 && now >= lease->checked && now - lease->checked < LeaseLifetime;
}

typedef enum {
    VisibilityUnchanged, VisibilityChanged, VisibilityInvalid, VisibilityWrongOwner, VisibilityUnclaimed,
} VisibilityUpdate;

static VisibilityUpdate UpdateVisibility(VisibilityLease *lease, NSTimeInterval now, pid_t expected,
    const char *line, size_t length) {
    if (length == 4 && !memcmp(line, "hide", 4)) {
        lease->checked = 0;
        return VisibilityChanged;
    }
    if (length == 9 && !memcmp(line, "unclaimed", 9)) return VisibilityUnclaimed;
    CGRect tile;
    if (length == 5 && !memcmp(line, "renew", 5)) {
        // A renewal may recover an expired show, but only another show can undo a hide.
        if (!lease->checked) return VisibilityUnchanged;
        tile = lease->tile;
    } else {
        pid_t pid;
        if (!ReadShow(line, length, &pid, &tile)) return VisibilityInvalid;
        if (pid != expected) return VisibilityWrongOwner;
    }
    BOOL unchanged = LeaseFresh(lease, now) && CGRectEqualToRect(tile, lease->tile);
    *lease = (VisibilityLease){tile, now};
    return unchanged ? VisibilityUnchanged : VisibilityChanged;
}

typedef struct {
    NSRect frame, visibleFrame;
    CGFloat scale;
    CGDirectDisplayID id;
    BOOL mirrored;
} PreviewScreen;

// Quartz uses the primary display's top-left as origin; AppKit uses its bottom-left.
// Do not clip or extrapolate when switching screens or when a window spans displays.
static BOOL PlaceOnScreen(Geometry geometry, CGRect primary, const PreviewScreen *screens, size_t count,
    NSRect *pane, CGDirectDisplayID *display) {
    if (!ValidRect(primary) || primary.origin.x != 0 || primary.origin.y != 0) return NO;
    NSRect window = NSMakeRect(geometry.window.origin.x, primary.size.height - CGRectGetMaxY(geometry.window),
        geometry.window.size.width, geometry.window.size.height);
    NSRect body = NSMakeRect(geometry.pane.origin.x, primary.size.height - CGRectGetMaxY(geometry.pane),
        geometry.pane.size.width, geometry.pane.size.height);
    const PreviewScreen *target = NULL;
    for (size_t i = 0; i < count; ++i) {
        if (!NSContainsRect(screens[i].frame, window)) continue;
        if (target) return NO;
        target = &screens[i];
    }
    if (!target || !target->id || target->mirrored || !NSContainsRect(window, body) ||
        !NSContainsRect(target->visibleFrame, body)) return NO;
    // Native full-screen Spaces are not a supported overlay placement target.
    if (NSEqualRects(target->frame, window)) return NO;
    *display = target->id;

    // Round inward at the selected screen's pixel scale; never cover a neighboring control.
    CGFloat scale = target->scale;
    if (!isfinite(scale) || scale <= 0) return NO;
    CGFloat left = ceil(NSMinX(body) * scale) / scale, bottom = ceil(NSMinY(body) * scale) / scale;
    CGFloat right = floor(NSMaxX(body) * scale) / scale, top = floor(NSMaxY(body) * scale) / scale;
    *pane = NSMakeRect(left, bottom, right - left, top - bottom);
    return ValidRect(*pane);
}

static BOOL ScreenFrame(Geometry geometry, NSRect *pane, CGDirectDisplayID *display) {
    PreviewScreen screens[32];
    size_t count = 0;
    for (NSScreen *screen in NSScreen.screens) {
        if (count == sizeof(screens) / sizeof(screens[0])) return NO;
        CGDirectDisplayID id = [screen.deviceDescription[@"NSScreenNumber"] unsignedIntValue];
        screens[count++] = (PreviewScreen){screen.frame, screen.visibleFrame, screen.backingScaleFactor,
            id, id && CGDisplayIsInMirrorSet(id)};
    }
    return PlaceOnScreen(geometry, CGDisplayBounds(CGMainDisplayID()), screens, count, pane, display);
}

CGWindowID milo_preview_pin_window(pid_t pid) {
    NSRunningApplication *rex = RexApplication(pid);
    if (!FrontmostRex(rex)) return kCGNullWindowID;
    Geometry sampled = SampleGeometry(VisibleWindows(), pid, kCGNullWindowID, getpid(), NULL, NULL);
    // There is no tile yet. Check just that this window and its current chrome can support placement.
    NSRect pane;
    CGDirectDisplayID display;
    if (!sampled.windowID || !MiloApproximatePane(sampled.window, CGRectMake(0, 0, 1, 1), &sampled.pane) ||
        !ScreenFrame(sampled, &pane, &display) || !FrontmostRex(rex)) return kCGNullWindowID;
    return sampled.windowID;
}

@interface MiloPreviewPanel : NSPanel
@end

@implementation MiloPreviewPanel
- (BOOL)canBecomeKeyWindow { return YES; }
- (BOOL)canBecomeMainWindow { return NO; }
@end

// Never let a stopped terminal or a stalled watcher hold the AppKit loop.
static BOOL WriteAll(int fd, const char *bytes, size_t length) {
    while (length) {
        ssize_t count = write(fd, bytes, length);
        if (count < 0 && errno == EINTR) continue;
        if (count <= 0) return NO;
        bytes += count;
        length -= (size_t)count;
    }
    return YES;
}

static int OpenTTYOutput(const char *path) {
    return open(path, O_WRONLY | O_CLOEXEC | O_NOCTTY | O_NONBLOCK);
}

static void WriteDiagnostic(int fd, const char *reason) {
    char line[256];
    int length = snprintf(line, sizeof(line), "milo preview: %s\n", reason);
    if (length > 0 && (size_t)length < sizeof(line)) WriteAll(fd, line, (size_t)length);
}

@interface MiloPreviewAnchor : NSObject <NSWindowDelegate> {
    NSURL *_url;
    NSRunningApplication *_rex;
    CGWindowID _pinned;
    int _stateFD, _tty;
    pid_t _parent;
    MiloPreviewPanel *_panel;
    NSTimer *_timer;
    NSMutableArray *_notificationTokens;
    id _keyMonitor;
    dispatch_queue_t _sampleQueue;
    dispatch_source_t _stateSource, _parentSource;
    NSMutableArray<dispatch_source_t> *_signalSources;
    VisibilityLease _lease;
    Geometry _candidate;
    NSRect _candidateFrame;
    CGRect _orderedPanelFrame;
    CGDirectDisplayID _candidateDisplay;
    NSTimeInterval _lastSample;
    NSTimeInterval _nextPinCheck;
    NSUInteger _generation;
    BOOL _samplePending, _finished, _sessionInactive, _screensAsleep;
    BOOL _panelProven;
    char _line[64];
    size_t _lineLength;
    int _status;
}
- (instancetype)initWithURL:(NSURL *)url rex:(NSRunningApplication *)rex pinned:(CGWindowID)pinned
    stateFD:(int)stateFD parent:(pid_t)parent tty:(int)tty;
- (int)run;
- (void)tick;
@end

@implementation MiloPreviewAnchor
- (instancetype)initWithURL:(NSURL *)url rex:(NSRunningApplication *)rex pinned:(CGWindowID)pinned
    stateFD:(int)stateFD parent:(pid_t)parent tty:(int)tty {
    if ((self = [super init])) {
        _url = url;
        _rex = rex;
        _pinned = pinned;
        _stateFD = stateFD;
        _parent = parent;
        _tty = tty;
        _sampleQueue = dispatch_queue_create("milo.preview.window", DISPATCH_QUEUE_SERIAL);
        _notificationTokens = [NSMutableArray array];
        _signalSources = [NSMutableArray array];
    }
    return self;
}

- (void)conceal {
    _candidate = (Geometry){0};
    _lastSample = 0;
    _panelProven = NO;
    _panel.ignoresMouseEvents = YES;
    if (_panel.visible) [_panel orderOut:nil];
}

- (void)invalidate {
    ++_generation;
    [self conceal];
}

- (void)finish:(const char *)reason {
    if (_finished) return;
    _finished = YES;
    [self invalidate];
    if (reason) {
        _status = 1;
        WriteDiagnostic(_tty, reason);
    }
    [NSApp stop:nil];
    // Wake the run loop so a command-line AppKit process observes -stop:.
    NSEvent *wake = [NSEvent otherEventWithType:NSEventTypeApplicationDefined location:NSZeroPoint modifierFlags:0
        timestamp:0 windowNumber:0 context:nil subtype:0 data1:0 data2:0];
    [NSApp postEvent:wake atStart:YES];
}

- (BOOL)canSample {
    return _lease.checked > 0 && !_sessionInactive && !_screensAsleep && FrontmostRex(_rex) &&
        LeaseFresh(&_lease, LeaseNow());
}

- (BOOL)acceptsPanelInput {
    return _panel.visible && _panelProven && _panel.onActiveSpace && [self canSample] &&
        SampleFresh(NSProcessInfo.processInfo.systemUptime, _lastSample);
}

- (void)handleLine {
    switch (UpdateVisibility(&_lease, LeaseNow(), _rex.processIdentifier, _line, _lineLength)) {
        case VisibilityUnchanged: break;
        case VisibilityChanged: [self invalidate]; break;
        case VisibilityInvalid: [self finish:"invalid Rex visibility message"]; break;
        case VisibilityWrongOwner: [self finish:"the Rex client changed; reopen the preview in the current Rex client"]; break;
        case VisibilityUnclaimed:
            [self finish:"the Mac viewer did not claim this preview tile. Close it and retry from a Mac-created working terminal; no permissions are needed."];
            break;
    }
}

- (void)readState {
    char bytes[192];
    ssize_t count = read(_stateFD, bytes, sizeof(bytes));
    if (!count) {
        [self finish:_lineLength ? "incomplete Rex visibility message" : NULL];
        return;
    }
    if (count < 0) {
        if (errno != EINTR && errno != EAGAIN) [self finish:"could not read Rex visibility state"];
        return;
    }
    for (ssize_t i = 0; i < count && !_finished; ++i) {
        if (bytes[i] == '\n') {
            [self handleLine];
            _lineLength = 0;
        } else if (_lineLength == sizeof(_line)) {
            [self finish:"oversized Rex visibility message"];
        } else {
            _line[_lineLength++] = bytes[i];
        }
    }
}

- (void)returnToTerminal {
    if (!_panel.keyWindow || ![self acceptsPanelInput]) return;
    [self invalidate];
    static const char focus[] = "focus\n";
    if (!WriteAll(STDOUT_FILENO, focus, sizeof(focus) - 1)) [self finish:"could not return focus to the Rex terminal"];
}

- (void)applySample:(Geometry)geometry checkedPanel:(BOOL)checkedPanel lostPin:(BOOL)lostPin
    generation:(NSUInteger)generation started:(NSTimeInterval)started {
    _samplePending = NO;
    if (_finished) return;
    if (lostPin) { [self finish:NULL]; return; }
    if (generation != _generation) return;
    NSRect frame;
    CGDirectDisplayID display;
    if (![self canSample] || !geometry.windowID ||
        !SampleFresh(NSProcessInfo.processInfo.systemUptime, started) || !ScreenFrame(geometry, &frame, &display) ||
        !SampleFresh(NSProcessInfo.processInfo.systemUptime, started)) {
        [self conceal];
        return;
    }
    if (!SameGeometry(_candidate, geometry) || display != _candidateDisplay ||
        !NSEqualRects(frame, _candidateFrame)) {
        [self conceal];
        _candidate = geometry;
        _candidateFrame = frame;
        _candidateDisplay = display;
        _lastSample = started;
        return;
    }
    _lastSample = started;
    if (!_panel) {
        __weak MiloPreviewAnchor *weakSelf = self;
        NSView *browser = MiloPreviewContent(_url, ^{ [weakSelf returnToTerminal]; });
        if (!browser) {
            [self finish:"expected an explicit loopback HTTP or HTTPS URL"];
            return;
        }
        _panel = [[MiloPreviewPanel alloc] initWithContentRect:frame
            styleMask:NSWindowStyleMaskBorderless | NSWindowStyleMaskNonactivatingPanel
            backing:NSBackingStoreBuffered defer:NO];
        _panel.floatingPanel = NO;
        _panel.level = NSNormalWindowLevel;
        _panel.hidesOnDeactivate = NO;
        _panel.becomesKeyOnlyIfNeeded = NO;
        _panel.collectionBehavior = NSWindowCollectionBehaviorTransient | NSWindowCollectionBehaviorIgnoresCycle;
        _panel.hasShadow = NO;
        _panel.movable = NO;
        _panel.animationBehavior = NSWindowAnimationBehaviorNone;
        _panel.tabbingMode = NSWindowTabbingModeDisallowed;
        _panel.releasedWhenClosed = NO;
        _panel.title = @"Milo Preview (Approximate)";
        _panel.delegate = self;
        _panel.contentView = browser;
        // WebKit startup can outlast a sample. Wait for another one before showing.
        return;
    }
    if (_panel.visible) {
        if (!checkedPanel || !_panel.onActiveSpace || !NSEqualRects(_panel.frame, frame)) {
            [self invalidate];
            return;
        }
        if (!_panelProven) {
            _panelProven = YES;
            _panel.ignoresMouseEvents = NO;
        }
        return;
    }

    [_panel setFrame:frame display:NO];
    CGRect primary = CGDisplayBounds(CGMainDisplayID());
    NSPoint center = NSMakePoint(NSMidX(frame), NSMidY(frame));
    // This point was unobstructed in the Quartz sample; get its window number
    // separately from AppKit rather than reusing the Quartz identifier.
    if (!ValidRect(primary) || primary.origin.x != 0 || primary.origin.y != 0 ||
        !CGRectContainsPoint(geometry.pane, CGPointMake(center.x, primary.size.height - center.y))) {
        [self invalidate];
        return;
    }
    NSInteger relativeWindow = [NSWindow windowNumberAtPoint:center belowWindowWithWindowNumber:0];
    if (relativeWindow <= 0 || generation != _generation || ![self canSample] ||
        !NSEqualRects(_panel.frame, frame) || !SampleFresh(NSProcessInfo.processInfo.systemUptime, started)) {
        [self invalidate];
        return;
    }
    _orderedPanelFrame = CGRectMake(NSMinX(frame), primary.size.height - NSMaxY(frame),
        NSWidth(frame), NSHeight(frame));
    _panelProven = NO;
    _panel.ignoresMouseEvents = YES;
    [_panel orderWindow:NSWindowAbove relativeTo:relativeWindow];
    if (!_panel.visible || generation != _generation || ![self canSample] ||
        !NSEqualRects(_panel.frame, frame) || !SampleFresh(NSProcessInfo.processInfo.systemUptime, started)) {
        [self invalidate];
        return;
    }
    // Verify the panel in a new Quartz sample before accepting any clicks.
    [self tick];
}

- (void)tick {
    if (_finished) return;
    if (getppid() != _parent) { [self finish:NULL]; return; }
    if (_rex.terminated) { [self finish:NULL]; return; }
    BOOL canSample = [self canSample];
    NSTimeInterval now = NSProcessInfo.processInfo.systemUptime;
    if (!canSample && (_candidate.windowID || _panel.visible)) [self invalidate];
    if ((_lastSample && !SampleFresh(now, _lastSample)) || (_panel.visible && !_panel.onActiveSpace)) [self conceal];
    if (_samplePending) return;
    BOOL checkPin = !_sessionInactive && !_screensAsleep && now >= _nextPinCheck;
    if (!canSample && !checkPin) return;
    if (checkPin) _nextPinCheck = now + 1.0;
    _samplePending = YES;
    NSUInteger generation = _generation;
    const CGRect normalized = _lease.tile;
    pid_t pid = _rex.processIdentifier;
    CGWindowID pinned = _pinned;
    BOOL checkedPanel = _panel.visible;
    const CGRect orderedPanel = _orderedPanelFrame;
    __weak MiloPreviewAnchor *weakSelf = self;
    dispatch_async(_sampleQueue, ^{
        @autoreleasepool {
            Geometry geometry = {0};
            if (canSample) {
                geometry = SampleGeometry(VisibleWindows(), pid, pinned, getpid(),
                    checkedPanel ? &orderedPanel : NULL, &normalized);
            }
            BOOL lostPin = NO;
            if (!geometry.windowID && checkPin) {
                pid_t owner;
                PinLookup lookup = LookupPin(pinned, &owner);
                lostPin = lookup == PinMissing || (lookup == PinFound && owner != pid);
            }
            dispatch_async(dispatch_get_main_queue(), ^{
                [weakSelf applySample:geometry checkedPanel:checkedPanel lostPin:lostPin generation:generation started:now];
            });
        }
    });
}

- (void)windowWillClose:(NSNotification *)notification {
    (void)notification;
    [self finish:NULL];
}

- (void)observe:(NSNotificationName)name center:(NSNotificationCenter *)center {
    __weak MiloPreviewAnchor *weakSelf = self;
    id token = [center addObserverForName:name object:nil queue:NSOperationQueue.mainQueue usingBlock:^(NSNotification *event) {
        MiloPreviewAnchor *anchor = weakSelf;
        if (!anchor) return;
        if ([event.name isEqualToString:NSWorkspaceDidTerminateApplicationNotification]) {
            NSRunningApplication *app = event.userInfo[NSWorkspaceApplicationKey];
            if (app.processIdentifier == anchor->_rex.processIdentifier) [anchor finish:NULL];
            return;
        }
        if ([event.name isEqualToString:NSWorkspaceSessionDidResignActiveNotification]) anchor->_sessionInactive = YES;
        if ([event.name isEqualToString:NSWorkspaceSessionDidBecomeActiveNotification]) anchor->_sessionInactive = NO;
        if ([event.name isEqualToString:NSWorkspaceScreensDidSleepNotification]) anchor->_screensAsleep = YES;
        if ([event.name isEqualToString:NSWorkspaceScreensDidWakeNotification]) anchor->_screensAsleep = NO;
        [anchor invalidate];
    }];
    [_notificationTokens addObject:@[center, token]];
}

- (int)run {
    __weak MiloPreviewAnchor *weakSelf = self;
    _stateSource = dispatch_source_create(DISPATCH_SOURCE_TYPE_READ, (uintptr_t)_stateFD, 0, dispatch_get_main_queue());
    _parentSource = dispatch_source_create(DISPATCH_SOURCE_TYPE_PROC, (uintptr_t)_parent, DISPATCH_PROC_EXIT, dispatch_get_main_queue());
    if (!_stateSource || !_parentSource) {
        if (_stateSource) { dispatch_resume(_stateSource); dispatch_source_cancel(_stateSource); }
        if (_parentSource) { dispatch_resume(_parentSource); dispatch_source_cancel(_parentSource); }
        [self finish:"could not observe the Rex preview lifecycle"];
        return _status;
    }

    NSNotificationCenter *workspace = NSWorkspace.sharedWorkspace.notificationCenter;
    for (NSNotificationName name in @[NSWorkspaceDidActivateApplicationNotification, NSWorkspaceDidDeactivateApplicationNotification,
        NSWorkspaceDidTerminateApplicationNotification, NSWorkspaceActiveSpaceDidChangeNotification,
        NSWorkspaceSessionDidResignActiveNotification, NSWorkspaceSessionDidBecomeActiveNotification,
        NSWorkspaceScreensDidSleepNotification, NSWorkspaceScreensDidWakeNotification]) [self observe:name center:workspace];
    [self observe:NSApplicationDidChangeScreenParametersNotification center:NSNotificationCenter.defaultCenter];
    dispatch_source_set_event_handler(_stateSource, ^{ [weakSelf readState]; });
    dispatch_source_set_event_handler(_parentSource, ^{ [weakSelf finish:NULL]; });
    dispatch_resume(_stateSource);
    dispatch_resume(_parentSource);
    const int signals[] = {SIGHUP, SIGINT, SIGTERM};
    struct sigaction oldActions[sizeof(signals) / sizeof(signals[0])];
    struct sigaction ignore = {.sa_handler = SIG_IGN};
    sigemptyset(&ignore.sa_mask);
    for (size_t i = 0; i < sizeof(signals) / sizeof(signals[0]); ++i) {
        dispatch_source_t source = dispatch_source_create(DISPATCH_SOURCE_TYPE_SIGNAL, (uintptr_t)signals[i], 0, dispatch_get_main_queue());
        if (!source || sigaction(signals[i], &ignore, &oldActions[i]) != 0) {
            if (source) { dispatch_resume(source); dispatch_source_cancel(source); }
            [self finish:"could not observe Rex terminal close signals"];
            break;
        }
        dispatch_source_set_event_handler(source, ^{ [weakSelf finish:NULL]; });
        [_signalSources addObject:source];
        dispatch_resume(source);
    }
    NSEventMask guardedInput = NSEventMaskKeyDown | NSEventMaskKeyUp | NSEventMaskFlagsChanged |
        NSEventMaskLeftMouseDown | NSEventMaskRightMouseDown | NSEventMaskOtherMouseDown |
        NSEventMaskLeftMouseUp | NSEventMaskRightMouseUp | NSEventMaskOtherMouseUp |
        NSEventMaskLeftMouseDragged | NSEventMaskRightMouseDragged | NSEventMaskOtherMouseDragged |
        NSEventMaskScrollWheel | NSEventMaskGesture | NSEventMaskMagnify | NSEventMaskSwipe | NSEventMaskRotate |
        NSEventMaskPressure;
    _keyMonitor = [NSEvent addLocalMonitorForEventsMatchingMask:guardedInput handler:^NSEvent *(NSEvent *event) {
        MiloPreviewAnchor *anchor = weakSelf;
        if (!anchor || !anchor->_panel || event.window != anchor->_panel) return event;
        if (![anchor acceptsPanelInput]) {
            [anchor invalidate];
            return nil;
        }
        if (event.type == NSEventTypeKeyDown && anchor->_panel.keyWindow &&
            (event.modifierFlags & NSEventModifierFlagDeviceIndependentFlagsMask) == NSEventModifierFlagCommand &&
            [event.charactersIgnoringModifiers isEqualToString:@"w"]) {
            [anchor finish:NULL];
            return nil;
        }
        return event;
    }];
    _timer = [NSTimer timerWithTimeInterval:SampleInterval repeats:YES block:^(NSTimer *timer) {
        (void)timer;
        [weakSelf tick];
    }];
    _timer.tolerance = 0.025;
    [NSRunLoop.mainRunLoop addTimer:_timer forMode:NSRunLoopCommonModes];
    [self tick];
    if (!_finished) [NSApp run];
    [self finish:NULL];

    [_timer invalidate];
    if (_keyMonitor) [NSEvent removeMonitor:_keyMonitor];
    for (NSArray *pair in _notificationTokens) [pair[0] removeObserver:pair[1]];
    for (NSUInteger i = 0; i < _signalSources.count; ++i) {
        dispatch_source_cancel(_signalSources[i]);
        sigaction(signals[i], &oldActions[i], NULL);
    }
    dispatch_source_cancel(_parentSource);
    dispatch_source_cancel(_stateSource);
    _panel.delegate = nil;
    [_panel close];
    return _status;
}
@end

int milo_preview_run(const char *url, CGWindowID pinned_window, int state_fd) {
    @autoreleasepool {
        if (!NSThread.isMainThread || !url || !pinned_window || state_fd < 0) {
            fprintf(stderr, "milo preview: invalid native anchor arguments\n");
            return 1;
        }
        NSString *address = [NSString stringWithUTF8String:url];
        NSURL *location = address ? [NSURL URLWithString:address] : nil;
        pid_t parent = getppid();
        int inputFlags = fcntl(state_fd, F_GETFL);
        if (!location || parent < 2 || inputFlags < 0 || fcntl(state_fd, F_SETFL, inputFlags | O_NONBLOCK) < 0) {
            fprintf(stderr, "milo preview: invalid URL or missing Rex watcher\n");
            return 1;
        }
        int tty = OpenTTYOutput("/dev/tty");
        if (tty < 0) {
            fprintf(stderr, "milo preview: no controlling Rex terminal\n");
            return 1;
        }
        pid_t owner;
        PinLookup lookup = LookupPin(pinned_window, &owner);
        NSRunningApplication *rex = lookup == PinFound ? RexApplication(owner) : nil;
        if (!rex) {
            WriteDiagnostic(tty, lookup == PinUnavailable ? "could not verify the pinned Rex window; reopen the preview"
                : "the pinned Rex window is no longer available; reopen the preview");
            close(tty);
            return 1;
        }
        // The Rust launcher may disappear before a focus message. Treat that as an I/O error, not SIGPIPE.
        int outputFlags = fcntl(STDOUT_FILENO, F_GETFL);
        if (outputFlags < 0 || fcntl(STDOUT_FILENO, F_SETFL, outputFlags | O_NONBLOCK) < 0 ||
            fcntl(STDOUT_FILENO, F_SETNOSIGPIPE, 1) < 0 ||
            ![[NSApplication sharedApplication] setActivationPolicy:NSApplicationActivationPolicyAccessory]) {
            WriteDiagnostic(tty, "could not initialize the nonactivating panel");
            close(tty);
            return 1;
        }
        static const char waiting[] = "Milo Preview: approximate placement. Waiting for a supported, visible Rex tile.\n";
        WriteAll(tty, waiting, sizeof(waiting) - 1);
        MiloPreviewAnchor *anchor = [[MiloPreviewAnchor alloc] initWithURL:location rex:rex pinned:pinned_window
            stateFD:state_fd parent:parent tty:tty];
        int status = [anchor run];
        close(tty);
        return status;
    }
}
