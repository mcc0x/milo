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
#import <stdlib.h>
#import <string.h>
#import <math.h>
#import <sys/stat.h>
#import <time.h>
#import <unistd.h>

static const NSTimeInterval SampleInterval = 0.25;
static const NSTimeInterval SampleLifetime = 0.60;
static const NSTimeInterval LeaseLifetime = 3.0;
// Rex's synchronous RPC is bounded at 15 seconds (src/rex.rs).
static const NSTimeInterval FocusReplyLifetime = 16.0;
enum { FractionScale = 1000000 };

typedef struct {
    CGRect window;
    CGRect pane;
    CGWindowID windowID;
    const char *rejection; // Static, non-identifying reason; only set on failure.
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

static const char PanelNotListed[] = "ordered helper panel missing from Quartz";

static Geometry RejectedGeometry(const char *reason) {
    return (Geometry){.rejection = reason};
}

// Quartz supplies onscreen metadata in front-to-back order. A pinned window is
// never replaced by another window, even if that window is the only Rex window.
static Geometry SampleGeometry(NSArray *windows, pid_t rexPID, CGWindowID pinned, pid_t helperPID,
    const CGRect *orderedPanel, const CGRect *normalized) {
    Geometry geometry = {0};
    if (!windows) return RejectedGeometry("Quartz window list unavailable");
    NSUInteger match = NSNotFound, panel = NSNotFound;
    BOOL sawTransparentHelper = NO, sawOtherRex = NO;
    int normal = CGWindowLevelForKey(kCGNormalWindowLevelKey);
    for (NSUInteger i = 0; i < windows.count; ++i) {
        NSDictionary *window = windows[i];
        int64_t owner, layer, number;
        if (![window isKindOfClass:NSDictionary.class] || !WindowInteger(window, kCGWindowOwnerPID, 1, INT_MAX, &owner))
            return RejectedGeometry("incomplete Quartz window identity");
        if (!pinned && owner != rexPID) continue;
        if (!WindowInteger(window, kCGWindowLayer, INT_MIN, INT_MAX, &layer) ||
            !WindowInteger(window, kCGWindowNumber, 1, UINT32_MAX, &number))
            return RejectedGeometry("incomplete Quartz window identity");
        if (pinned && number == pinned && (owner != rexPID || layer != normal))
            return RejectedGeometry("pinned Rex window changed owner or level");
        if (orderedPanel && owner == helperPID && layer == normal) {
            double alpha;
            if (!WindowAlpha(window, &alpha)) return RejectedGeometry("helper window opacity unavailable");
            if (alpha != 0) {
                CGRect bounds = WindowBounds(window);
                // Some Quartz metadata rounds bounds outward to whole points.
                if (panel != NSNotFound) return RejectedGeometry("multiple visible helper panels");
                if (alpha != 1) return RejectedGeometry("helper panel opacity changed");
                if (!CGRectEqualToRect(bounds, *orderedPanel) &&
                    !CGRectEqualToRect(bounds, CGRectIntegral(*orderedPanel)))
                    return RejectedGeometry("Quartz helper frame differs from the ordered frame");
                panel = i;
            } else {
                sawTransparentHelper = YES;
            }
        }
        if (owner != rexPID || layer != normal) continue;
        double alpha;
        if (!WindowAlpha(window, &alpha)) return RejectedGeometry("Rex window opacity unavailable");
        if (alpha == 0 && number != pinned) continue;
        if (alpha == 0) return RejectedGeometry("pinned Rex window transparent");
        if (pinned && number != pinned) {
            sawOtherRex = YES;
            continue;
        }
        if (match != NSNotFound)
            return RejectedGeometry("additional normal Rex window; one visible Rex window is supported");
        CGRect bounds = WindowBounds(window);
        if (CGRectIsNull(bounds)) return RejectedGeometry("Rex window bounds unavailable");
        geometry = (Geometry){.window = bounds, .pane = bounds, .windowID = (CGWindowID)number};
        match = i;
    }
    if (match == NSNotFound) return RejectedGeometry(pinned ? "pinned Rex window not onscreen" : "no visible normal Rex window");
    if (sawOtherRex) return RejectedGeometry("additional normal Rex window; one visible Rex window is supported");
    if (panel != NSNotFound && panel >= match) return RejectedGeometry("helper panel is behind the pinned Rex window");
    if (normalized && !MiloApproximatePane(geometry.window, *normalized, &geometry.pane))
        return RejectedGeometry("Rex chrome and tile layout do not yield a usable pane");
    // Pinning identifies the unique eligible Rex window; it grants no
    // permission to display. Only a subsequent pinned sample has a tile.
    if (!pinned) return geometry;

    for (NSUInteger i = 0; i < match; ++i) {
        if (i == panel) continue;
        NSDictionary *window = windows[i];
        double alpha;
        if (!WindowAlpha(window, &alpha)) return RejectedGeometry("occlusion cannot be checked: missing opacity");
        if (alpha == 0) continue;
        CGRect bounds = WindowBounds(window);
        if (CGRectIsNull(bounds)) return RejectedGeometry("occlusion cannot be checked: missing bounds");
        if (!CGRectIsEmpty(CGRectIntersection(bounds, geometry.pane))) {
            pid_t owner = [window[(__bridge NSString *)kCGWindowOwnerPID] intValue];
            if (owner == rexPID) return RejectedGeometry("a Rex overlay overlaps the tile; dismiss it");
            if (owner == helperPID) return RejectedGeometry("another helper window overlaps the tile");
            return RejectedGeometry("another window overlaps the tile; waiting for clear placement");
        }
    }
    if (orderedPanel && panel == NSNotFound) {
        if (sawTransparentHelper) return RejectedGeometry("only transparent helper windows listed in Quartz");
        // Carry the checked pin's geometry, but do not return a usable window ID.
        // Nothing other than the helper's own missing record can reach this path.
        geometry.windowID = 0;
        geometry.rejection = PanelNotListed;
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

static BOOL ReadDecimal(const char **cursor, const char *end, uint64_t limit, uint64_t *out) {
    const char *p = *cursor;
    if (p == end || *p < '0' || *p > '9') return NO;
    uint64_t value = 0;
    while (p != end && *p >= '0' && *p <= '9') {
        uint64_t digit = (uint64_t)(*p++ - '0');
        if (value > (limit - digit) / 10) return NO;
        value = value * 10 + digit;
    }
    *cursor = p;
    *out = value;
    return YES;
}

static BOOL ReadTile(const char *line, size_t length, pid_t *pid, CGRect *normalized) {
    const char *end = line + length;
    if (length < 5 || memcmp(line, "show ", 5)) return NO;
    line += 5;
    const char *p = line;
    uint64_t process, parts[4];
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
    uint64_t announcedAt; // Geometry retained independently of visibility; zero after hide.
} VisibilityLease;

static NSTimeInterval LeaseNow(void) {
    // Unlike uptime, this clock also counts time spent asleep.
    return (NSTimeInterval)clock_gettime_nsec_np(CLOCK_MONOTONIC_RAW) / NSEC_PER_SEC;
}

static NSTimeInterval RealtimeNow(void) {
    return (NSTimeInterval)clock_gettime_nsec_np(CLOCK_REALTIME) / NSEC_PER_SEC;
}

typedef struct {
    NSTimeInterval before, wall, after;
} ClockObservation;

typedef struct {
    ClockObservation previous;
    NSTimeInterval rejectThrough;
    BOOL uncertain;
} ObserverClock;

static ClockObservation ObserveClock(void) {
    NSTimeInterval before = LeaseNow(), wall = RealtimeNow();
    return (ClockObservation){before, wall, LeaseNow()};
}

// Return YES when previously issued proof must be discarded. Bound when the
// wall clock was read with the continuous clock, including across suspension.
static BOOL UpdateObserverClock(ObserverClock *clock, ClockObservation current) {
    const NSTimeInterval tolerance = 0.25; // Ignore ordinary drift and read jitter.
    BOOL uncertain = current.before <= 0 || current.wall <= 0 || current.after < current.before ||
        current.after - current.before >= tolerance;
    BOOL invalidate = uncertain && !clock->uncertain;
    clock->uncertain = uncertain;
    if (uncertain) return invalidate;
    ClockObservation previous = clock->previous;
    clock->previous = current;
    if (previous.before > 0 && previous.wall + (current.before - previous.after) - current.wall > tolerance) {
        // Even a buffered observation close to our next callback could
        // predate the jump. Fence off the old projected wall-clock range.
        NSTimeInterval upper = previous.wall + (current.after - previous.before) + tolerance;
        clock->rejectThrough = MAX(clock->rejectThrough, upper);
        return YES;
    }
    return invalidate;
}

typedef NS_ENUM(NSUInteger, FocusReply) { FocusWaiting, FocusApproved, FocusRefused, FocusEnded, FocusFailed };

static FocusReply ReadFocusReply(int fd, NSTimeInterval now, NSTimeInterval *deadline) {
    if (now >= *deadline) return FocusFailed;
    char byte;
    ssize_t count = read(fd, &byte, 1);
    if (!count) return FocusEnded;
    if (count < 0) return errno == EAGAIN || errno == EINTR ? FocusWaiting : FocusFailed;
    if (byte != '1' && byte != '0') return FocusFailed;
    *deadline = 0;
    return byte == '1' ? FocusApproved : FocusRefused;
}

static BOOL LeaseFresh(const VisibilityLease *lease, NSTimeInterval now) {
    return lease->checked > 0 && now >= lease->checked && now - lease->checked < LeaseLifetime;
}

typedef enum {
    VisibilityUnchanged, VisibilityChanged, VisibilityInvalid, VisibilityWrongOwner, VisibilityUnclaimed,
} VisibilityUpdate;

static VisibilityUpdate UpdateVisibility(VisibilityLease *lease, NSTimeInterval now, NSTimeInterval wall,
    NSTimeInterval rejectThrough, pid_t expected, const char *line, size_t length) {
    if (length == 4 && !memcmp(line, "hide", 4)) {
        *lease = (VisibilityLease){0};
        return VisibilityChanged;
    }
    if (length == 9 && !memcmp(line, "unclaimed", 9)) return VisibilityUnclaimed;

    const char *stamp = memchr(line, '@', length), *end = line + length;
    if (!stamp || stamp - line < 2 || stamp[-1] != ' ') return VisibilityInvalid;
    size_t recordLength = (size_t)(stamp - 1 - line);
    ++stamp;
    uint64_t epoch;
    if (stamp == end || *stamp == '0' || !ReadDecimal(&stamp, end, UINT64_MAX, &epoch) || stamp != end)
        return VisibilityInvalid;

    BOOL renew = recordLength == 5 && !memcmp(line, "renew", 5);
    CGRect tile;
    if (renew) {
        // A renewal may recover expired evidence but may not undo a hide.
        if (!lease->announcedAt) return VisibilityUnchanged;
        tile = lease->tile;
    } else {
        pid_t pid;
        if (!ReadTile(line, recordLength, &pid, &tile)) return VisibilityInvalid;
        if (pid != expected) return VisibilityWrongOwner;
    }

    // The stamp is truncated to seconds at the start of the observer's
    // refresh. Conservatively charge that fraction and all pipe delay.
    // Store the check on a continuous clock, not the adjustable wall clock.
    NSTimeInterval age = wall - (NSTimeInterval)epoch;
    BOOL plausible = age >= 0 && epoch > rejectThrough && epoch >= lease->announcedAt;
    NSTimeInterval checked = plausible && age < LeaseLifetime && now > age ? now - age : 0;
    BOOL unchanged = checked && LeaseFresh(lease, now) && CGRectEqualToRect(tile, lease->tile);
    if (renew) {
        lease->checked = checked;
    } else if (plausible) {
        *lease = (VisibilityLease){tile, checked, epoch};
    } else {
        *lease = (VisibilityLease){0};
    }
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
static const char *ScreenPlacementFailure(Geometry geometry, CGRect primary, const PreviewScreen *screens, size_t count,
    NSRect *pane, CGDirectDisplayID *display) {
    if (!ValidRect(primary) || primary.origin.x != 0 || primary.origin.y != 0) return "primary display coordinates unavailable";
    NSRect window = NSMakeRect(geometry.window.origin.x, primary.size.height - CGRectGetMaxY(geometry.window),
        geometry.window.size.width, geometry.window.size.height);
    NSRect body = NSMakeRect(geometry.pane.origin.x, primary.size.height - CGRectGetMaxY(geometry.pane),
        geometry.pane.size.width, geometry.pane.size.height);
    const PreviewScreen *target = NULL;
    for (size_t i = 0; i < count; ++i) {
        if (!NSContainsRect(screens[i].frame, window)) continue;
        if (target) return "multiple displays contain the candidate Rex window";
        target = &screens[i];
    }
    if (!target) return "candidate Rex window is not wholly contained on one display";
    if (!target->id || target->mirrored) return "candidate display is mirrored or its identity is unavailable";
    if (NSEqualRects(target->frame, window)) return "candidate Rex window occupies the entire display; that layout is unsupported";
    if (!NSContainsRect(window, body) || !NSContainsRect(target->visibleFrame, body))
        return "projected workspace is outside the Rex window or the display work area";
    *display = target->id;

    // Round inward at the selected screen's pixel scale; never cover a neighboring control.
    CGFloat scale = target->scale;
    if (!isfinite(scale) || scale <= 0) return "display pixel scale unavailable";
    CGFloat left = ceil(NSMinX(body) * scale) / scale, bottom = ceil(NSMinY(body) * scale) / scale;
    CGFloat right = floor(NSMaxX(body) * scale) / scale, top = floor(NSMaxY(body) * scale) / scale;
    *pane = NSMakeRect(left, bottom, right - left, top - bottom);
    return ValidRect(*pane) ? NULL : "projected workspace is empty after pixel alignment";
}

static const char *ScreenFrameFailure(Geometry geometry, NSRect *pane, CGDirectDisplayID *display) {
    PreviewScreen screens[32];
    size_t count = 0;
    for (NSScreen *screen in NSScreen.screens) {
        if (count == sizeof(screens) / sizeof(screens[0])) return "display inventory exceeds supported limit";
        CGDirectDisplayID id = [screen.deviceDescription[@"NSScreenNumber"] unsignedIntValue];
        screens[count++] = (PreviewScreen){screen.frame, screen.visibleFrame, screen.backingScaleFactor,
            id, id && CGDisplayIsInMirrorSet(id)};
    }
    return ScreenPlacementFailure(geometry, CGDisplayBounds(CGMainDisplayID()), screens, count, pane, display);
}

static CGWindowID PinForegroundWindow(NSRunningApplication *rex, pid_t pid, const char **reason) {
    if (!rex || rex.terminated || rex.processIdentifier != pid) {
        *reason = "Rex client PID does not identify a running local Rex application";
        return kCGNullWindowID;
    }
    if (!FrontmostRex(rex)) {
        *reason = "the identified Rex process was not foreground at the start of the window check";
        return kCGNullWindowID;
    }
    CGRect workspace = CGRectMake(0, 0, 1, 1);
    Geometry sampled = SampleGeometry(VisibleWindows(), pid, kCGNullWindowID, getpid(), NULL, &workspace);
    if (!sampled.windowID) {
        *reason = sampled.rejection;
        return kCGNullWindowID;
    }
    NSRect pane;
    CGDirectDisplayID display;
    *reason = ScreenFrameFailure(sampled, &pane, &display);
    if (*reason) return kCGNullWindowID;
    if (!FrontmostRex(rex)) {
        *reason = "the identified Rex process lost foreground during the window check";
        return kCGNullWindowID;
    }
    return sampled.windowID;
}

CGWindowID milo_preview_pin_window(pid_t pid, const char **reason) {
    return PinForegroundWindow(RexApplication(pid), pid, reason);
}

@interface MiloPreviewPanel : NSPanel
@property (nonatomic) BOOL placementProven;
- (void)prepareStaging;
@end

@implementation MiloPreviewPanel
@synthesize placementProven = _placementProven;

- (void)prepareStaging {
    // Keep window alpha at 1 for Quartz metadata. Only the empty, clear
    // window is ordered until its bounds and stacking have been verified.
    self.opaque = NO;
    self.backgroundColor = NSColor.clearColor;
    self.placementProven = NO;
}

- (void)setPlacementProven:(BOOL)proven {
    _placementProven = proven;
    if (!proven) self.ignoresMouseEvents = YES;
    self.contentView.hidden = !proven;
    if (_placementProven) self.ignoresMouseEvents = NO;
}

- (BOOL)canBecomeKeyWindow { return self.placementProven; }
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

// Fixed notices only. Never share the shell's descriptor flags or block the
// AppKit thread on a stopped terminal. The preview needs no terminal input.
static int OpenDiagnosticOutput(void) {
    if (signal(SIGTTOU, SIG_IGN) == SIG_ERR) return -1;
    return open("/dev/tty", O_WRONLY | O_NONBLOCK | O_NOCTTY | O_CLOEXEC);
}

static void WriteDiagnostic(int fd, const char *context, const char *phase, const char *reason) {
    if (fd < 0) return;
    char line[256];
    int length = context ? snprintf(line, sizeof(line), "milo preview: %s: %s: %s\n", context, phase, reason) :
        snprintf(line, sizeof(line), "milo preview: %s\n", reason);
    if (length > 0 && (size_t)length < sizeof(line)) WriteAll(fd, line, (size_t)length);
}

static int StartupFailure(const char *reason) {
    int fd = OpenDiagnosticOutput();
    if (fd >= 0) {
        WriteDiagnostic(fd, NULL, NULL, reason);
        close(fd);
    } else {
        fprintf(stderr, "milo preview: %s\n", reason);
    }
    return 1;
}

static int PrepareFocusInput(int stateFD) {
    const char *encoded = getenv("MILO_PREVIEW_FOCUS_FD");
    if (!encoded) return -1;
    const char *cursor = encoded, *end = encoded + strlen(encoded);
    uint64_t number;
    if (!ReadDecimal(&cursor, end, INT_MAX, &number) || cursor != end || number <= STDERR_FILENO ||
        (int)number == stateFD) return -1;
    int fd = (int)number;
    struct stat metadata;
    int flags = fcntl(fd, F_GETFL);
    if (flags < 0 || (flags & O_ACCMODE) != O_RDONLY || fstat(fd, &metadata) < 0 || !S_ISFIFO(metadata.st_mode)) return -1;
    if (fcntl(fd, F_SETFD, FD_CLOEXEC) < 0 || fcntl(fd, F_SETFL, flags | O_NONBLOCK) < 0) {
        close(fd);
        return -1;
    }
    return fd;
}

@interface MiloPreviewAnchor : NSObject <NSWindowDelegate> {
    NSURL *_url;
    NSRunningApplication *_rex;
    CGWindowID _pinned;
    int _stateFD, _focusFD, _diagnosticFD;
    pid_t _parent;
    MiloPreviewPanel *_panel;
    NSTimer *_timer;
    NSMutableArray *_notificationTokens;
    id _inputMonitor;
    dispatch_queue_t _sampleQueue;
    dispatch_source_t _stateSource, _parentSource;
    NSMutableArray<dispatch_source_t> *_signalSources;
    VisibilityLease _lease;
    ObserverClock _clock;
    Geometry _candidate;
    NSRect _candidateFrame;
    CGRect _orderedPanelFrame;
    CGDirectDisplayID _candidateDisplay;
    NSTimeInterval _lastSample, _nextPinCheck, _focusDeadline, _scopeSince, _revealedAt;
    NSUInteger _generation;
    const char *_lastPhase, *_lastReason;
    BOOL _firstOrderedSample, _samplePending, _finished, _sessionInactive, _screensAsleep;
    BOOL _drainingState;
    char _line[96];
    size_t _lineLength;
    int _status;
}
- (instancetype)initWithURL:(NSURL *)url rex:(NSRunningApplication *)rex pinned:(CGWindowID)pinned
    stateFD:(int)stateFD focusFD:(int)focusFD parent:(pid_t)parent;
- (int)run;
- (void)tick;
- (void)readState;
- (BOOL)acceptsPanelInput;
@end

@implementation MiloPreviewAnchor
- (instancetype)initWithURL:(NSURL *)url rex:(NSRunningApplication *)rex pinned:(CGWindowID)pinned
    stateFD:(int)stateFD focusFD:(int)focusFD parent:(pid_t)parent {
    if ((self = [super init])) {
        _url = url;
        _rex = rex;
        _pinned = pinned;
        _stateFD = stateFD;
        _focusFD = focusFD;
        _diagnosticFD = -1;
        _parent = parent;
        _sampleQueue = dispatch_queue_create("milo.preview.window", DISPATCH_QUEUE_SERIAL);
        _notificationTokens = [NSMutableArray array];
        _signalSources = [NSMutableArray array];
        UpdateObserverClock(&_clock, ObserveClock());
        if (_clock.uncertain) return nil;
    }
    return self;
}

- (void)hidePanel {
    if (_panel.placementProven) _panel.placementProven = NO;
    if (_panel.visible) [_panel orderOut:nil];
}

- (void)invalidatePlacement {
    ++_generation;
    _candidate = (Geometry){0};
    _candidateFrame = NSZeroRect;
    _candidateDisplay = 0;
    _lastSample = 0;
    _revealedAt = 0;
    _firstOrderedSample = NO;
    [self hidePanel];
}

- (void)rejectPlacementAt:(const char *)phase reason:(const char *)reason {
    BOOL visible = _panel.placementProven;
    [self invalidatePlacement];
    // Fixed reasons only; a suspended pane may keep retrying until the
    // window is usable again without flooding the terminal.
    if (_lastPhase && _lastReason && !strcmp(phase, _lastPhase) && !strcmp(reason, _lastReason)) return;
    _lastPhase = phase;
    _lastReason = reason;
    WriteDiagnostic(_diagnosticFD, visible ? "preview hidden" : "not shown", phase, reason);
}

- (void)finish:(const char *)reason {
    if (_finished) return;
    _finished = YES;
    [self invalidatePlacement];
    if (reason) {
        _status = 1;
        WriteDiagnostic(_diagnosticFD, NULL, NULL, reason);
    }
    [NSApp stop:nil];
    // Wake the run loop so a command-line AppKit process observes -stop:.
    NSEvent *wake = [NSEvent otherEventWithType:NSEventTypeApplicationDefined location:NSZeroPoint modifierFlags:0
        timestamp:0 windowNumber:0 context:nil subtype:0 data1:0 data2:0];
    [NSApp postEvent:wake atStart:YES];
}

- (BOOL)observerEligible {
    return !_drainingState && !_lineLength && !_clock.uncertain &&
        LeaseFresh(&_lease, LeaseNow()) && _lease.checked >= _scopeSince;
}

- (BOOL)canSample {
    return !_finished && !_sessionInactive && !_screensAsleep && !_focusDeadline &&
        [self observerEligible] && FrontmostRex(_rex);
}

- (ClockObservation)observeClock {
    ClockObservation observation = ObserveClock();
    if (UpdateObserverClock(&_clock, observation)) {
        _lease = (VisibilityLease){0};
        [self invalidatePlacement];
    }
    return observation;
}

- (void)handleLine:(size_t)length {
    ClockObservation observation = [self observeClock];
    NSTimeInterval rejectThrough = _clock.uncertain ? INFINITY : _clock.rejectThrough;
    switch (UpdateVisibility(&_lease, observation.before, observation.wall, rejectThrough,
        _rex.processIdentifier, _line, length)) {
        case VisibilityUnchanged: break;
        case VisibilityChanged: [self invalidatePlacement]; break;
        case VisibilityInvalid: [self finish:"invalid Rex visibility message"]; break;
        case VisibilityWrongOwner: [self finish:"the Rex client changed; reopen the preview in the current Rex client"]; break;
        case VisibilityUnclaimed:
            [self finish:"the Mac viewer did not claim this preview tile. Close it and retry from a Mac-created working terminal; no permissions are needed."];
            break;
    }
}

- (void)readState {
    // Bound continuous writer activity and never reveal or accept input
    // before draining queued revocations all the way to an empty read.
    [self observeClock];
    for (NSUInteger chunk = 0; chunk < 8 && !_finished; ++chunk) {
        char bytes[192];
        ssize_t count = read(_stateFD, bytes, sizeof(bytes));
        if (!count) {
            [self finish:_lineLength ? "incomplete Rex visibility message" : NULL];
            return;
        }
        if (count < 0) {
            if (errno == EINTR) continue;
            if (errno != EAGAIN) [self finish:"could not read Rex visibility state"];
            else {
                [self observeClock];
                BOOL wasDraining = _drainingState;
                _drainingState = _lineLength != 0;
                if (_drainingState && !wasDraining) [self invalidatePlacement];
            }
            return;
        }
        for (ssize_t i = 0; i < count && !_finished; ++i) {
            if (bytes[i] == '\n') {
                size_t length = _lineLength;
                _lineLength = 0;
                [self handleLine:length];
            } else if (_lineLength == sizeof(_line)) {
                [self finish:"oversized Rex visibility message"];
            } else {
                _line[_lineLength++] = bytes[i];
            }
        }
    }
    if (!_finished && !_drainingState) {
        // A finite burst larger than our per-callback budget is not a fatal
        // protocol error. Suspend until another pass reaches a complete drain.
        _drainingState = YES;
        [self invalidatePlacement];
    }
}

- (BOOL)acceptsPanelInput {
    if (_finished || !_panel.placementProven) return NO;
    // AppKit events and WebKit completions (including Copy) may run before
    // the dispatch read handler. A queued change must hide synchronously.
    [self readState];
    if (_finished || !_panel.placementProven) return NO;
    if (getppid() != _parent || _rex.terminated) { [self finish:NULL]; return NO; }
    if (![self canSample] || !SampleFresh(NSProcessInfo.processInfo.systemUptime, _lastSample)) {
        [self invalidatePlacement];
        return NO;
    }
    if (!_panel.visible || !_panel.onActiveSpace || !NSEqualRects(_panel.frame, _candidateFrame)) {
        [self rejectPlacementAt:"input" reason:"panel is no longer eligible for input"];
        return NO;
    }
    return YES;
}

- (void)returnToTerminal {
    if (!_panel.keyWindow || ![self acceptsPanelInput]) return;
    // Order out to give up the panel's nonactivating key focus. Resume only
    // after the original Rex working pane acknowledges focus and we restage.
    _focusDeadline = LeaseNow() + FocusReplyLifetime;
    [self invalidatePlacement];
    static const char focus[] = "focus\n";
    if (!WriteAll(STDOUT_FILENO, focus, sizeof(focus) - 1))
        [self finish:"could not return focus to the Rex terminal"];
}

- (BOOL)proofCurrent:(NSUInteger)generation started:(NSTimeInterval)started {
    [self readState];
    if (_finished || generation != _generation) return NO;
    if (getppid() != _parent || _rex.terminated) { [self finish:NULL]; return NO; }
    if (![self canSample] || !SampleFresh(NSProcessInfo.processInfo.systemUptime, started)) {
        [self invalidatePlacement];
        return NO;
    }
    return YES;
}

- (void)initializePanelAt:(NSRect)frame {
    // WebKit/AppKit initialization can block or run nested events. Retain
    // the instance even if the old geometry or eligibility is lost, but
    // never use a proof taken before initialization to order it.
    [self invalidatePlacement];
    __weak MiloPreviewAnchor *weakSelf = self;
    NSView *browser = MiloPreviewContent(_url, ^BOOL{ return [weakSelf acceptsPanelInput]; },
        ^{ [weakSelf returnToTerminal]; });
    if (_finished) return;
    if (!browser) { [self finish:"expected an explicit loopback HTTP or HTTPS URL"]; return; }

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
    [_panel prepareStaging];
}

- (void)stagePanelAt:(NSRect)frame geometry:(Geometry)geometry generation:(NSUInteger)generation started:(NSTimeInterval)started {
    [_panel setFrame:frame display:NO];
    CGRect primary = CGDisplayBounds(CGMainDisplayID());
    NSPoint center = NSMakePoint(NSMidX(frame), NSMidY(frame));
    // The Quartz sample found this point unobstructed; obtain the window
    // number separately from AppKit rather than reusing a Quartz identifier.
    if (!ValidRect(primary) || primary.origin.x != 0 || primary.origin.y != 0 ||
        !CGRectContainsPoint(geometry.pane, CGPointMake(center.x, primary.size.height - center.y))) {
        [self rejectPlacementAt:"order" reason:"tile center is no longer inside the projected workspace"];
        return;
    }
    NSInteger relativeWindow = [NSWindow windowNumberAtPoint:center belowWindowWithWindowNumber:0];
    if (![self proofCurrent:generation started:started]) return;
    if (relativeWindow <= 0 || !NSEqualRects(_panel.frame, frame)) {
        [self rejectPlacementAt:"order" reason:relativeWindow <= 0 ? "AppKit found no window at the tile center" :
            "AppKit adjusted the requested helper frame"];
        return;
    }

    _orderedPanelFrame = CGRectMake(NSMinX(frame), primary.size.height - NSMaxY(frame), NSWidth(frame), NSHeight(frame));
    _panel.placementProven = NO;
    [_panel orderWindow:NSWindowAbove relativeTo:relativeWindow];
    if (![self proofCurrent:generation started:started]) { [self hidePanel]; return; }
    if (!_panel.visible || !NSEqualRects(_panel.frame, frame)) {
        [self rejectPlacementAt:"order" reason:!_panel.visible ? "AppKit did not order the helper panel" :
            "AppKit changed the frame when ordering the helper panel"];
        return;
    }
    _firstOrderedSample = YES;
    // Leave the clear, click-through panel ordered for post-order proof.
}

- (void)applySample:(Geometry)geometry checkedPanel:(BOOL)checkedPanel lostPin:(BOOL)lostPin
    generation:(NSUInteger)generation started:(NSTimeInterval)started {
    if (lostPin) { [self finish:NULL]; return; }
    if (![self proofCurrent:generation started:started]) return;
    if (checkedPanel != _panel.visible || (checkedPanel && !_panel.onActiveSpace)) {
        [self rejectPlacementAt:"post-order" reason:checkedPanel && _panel.visible ?
            "helper panel is not on the active Space" : "panel changed during the sample"];
        return;
    }

    if (checkedPanel && _firstOrderedSample) {
        _firstOrderedSample = NO;
        if (geometry.rejection == PanelNotListed && !_panel.placementProven &&
            NSEqualRects(_panel.frame, _candidateFrame) && CGRectEqualToRect(geometry.window, _candidate.window) &&
            CGRectEqualToRect(geometry.pane, _candidate.pane)) {
            // AppKit can report visible one sample before Quartz lists the
            // helper. Retry only a missing record and keep it clear.
            return;
        }
    }
    if (!geometry.windowID) {
        [self rejectPlacementAt:checkedPanel ? "post-order" : "pre-order" reason:geometry.rejection];
        return;
    }
    NSRect frame;
    CGDirectDisplayID display;
    const char *reason = ScreenFrameFailure(geometry, &frame, &display);
    if (![self proofCurrent:generation started:started]) return;
    if (reason) {
        [self rejectPlacementAt:checkedPanel ? "post-order" : "pre-order" reason:reason];
        return;
    }
    if (!SameGeometry(_candidate, geometry) || display != _candidateDisplay || !NSEqualRects(frame, _candidateFrame)) {
        // A changed window/frame cannot reuse the prior order proof.
        [self invalidatePlacement];
        _candidate = geometry;
        _candidateFrame = frame;
        _candidateDisplay = display;
        _lastSample = started;
        return;
    }
    _lastSample = started;
    if (!_panel) {
        [self initializePanelAt:frame];
    } else if (!checkedPanel) {
        [self stagePanelAt:frame geometry:geometry generation:generation started:started];
    } else if (!_panel.visible || !_panel.onActiveSpace || !NSEqualRects(_panel.frame, frame)) {
        [self rejectPlacementAt:"post-order" reason:!_panel.visible ? "helper panel is not ordered" :
            !_panel.onActiveSpace ? "helper panel is not on the active Space" : "AppKit helper frame changed"];
    } else if (!_panel.placementProven) {
        if (![self proofCurrent:generation started:started]) return;
        _lastPhase = _lastReason = NULL;
        _revealedAt = NSProcessInfo.processInfo.systemUptime;
        _panel.placementProven = YES;
    }
}

- (void)tick {
    if (_finished) return;
    if (getppid() != _parent || _rex.terminated) { [self finish:NULL]; return; }
    [self readState];
    if (_finished) return;
    if (_focusDeadline) {
        FocusReply reply = ReadFocusReply(_focusFD, LeaseNow(), &_focusDeadline);
        if (reply == FocusEnded) { [self finish:NULL]; return; }
        if (reply == FocusFailed) { [self finish:"could not confirm focus returned to the Rex terminal"]; return; }
        if (reply == FocusRefused) {
            // A tab change can race the focus request. Keep the same page
            // suspended and wait for a fresh observer receipt and proof.
            _scopeSince = LeaseNow();
            WriteDiagnostic(_diagnosticFD, NULL, NULL, "could not return focus to the original Rex terminal");
        }
    }
    BOOL canSample = [self canSample];
    NSTimeInterval now = NSProcessInfo.processInfo.systemUptime;
    if ((!canSample && (_candidate.windowID || _panel.visible)) || (_lastSample && !SampleFresh(now, _lastSample)))
        [self invalidatePlacement];
    else if (_panel.visible && !_panel.onActiveSpace)
        [self rejectPlacementAt:"post-order" reason:"helper panel is not on the active Space"];
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
                MiloPreviewAnchor *anchor = weakSelf;
                if (!anchor) return;
                if (canSample || lostPin)
                    [anchor applySample:geometry checkedPanel:checkedPanel lostPin:lostPin generation:generation started:now];
                // Keep only one sample in flight, including across nested
                // run loops during WebKit creation or AppKit ordering.
                anchor->_samplePending = NO;
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
        if ([event.name isEqualToString:NSApplicationDidChangeScreenParametersNotification]) {
            [anchor invalidatePlacement];
            return;
        }
        if ([event.name isEqualToString:NSWorkspaceDidDeactivateApplicationNotification]) {
            NSRunningApplication *app = event.userInfo[NSWorkspaceApplicationKey];
            if (app && app.processIdentifier != anchor->_rex.processIdentifier) return;
        }
        // Require a new observer receipt after this local scope transition.
        // In particular, a queued pre-sleep renewal is not a fresh proof.
        [anchor invalidatePlacement];
        [anchor readState];
        anchor->_scopeSince = LeaseNow();
    }];
    [_notificationTokens addObject:@[center, token]];
}

- (int)run {
    // The parent starts the observer only after this process has sampled
    // both clocks. An old observation cannot queue before that baseline.
    if (!WriteAll(STDOUT_FILENO, "R", 1)) return StartupFailure("could not start the Rex preview observer");
    _diagnosticFD = OpenDiagnosticOutput();
    __weak MiloPreviewAnchor *weakSelf = self;
    _stateSource = dispatch_source_create(DISPATCH_SOURCE_TYPE_READ, (uintptr_t)_stateFD, 0, dispatch_get_main_queue());
    _parentSource = dispatch_source_create(DISPATCH_SOURCE_TYPE_PROC, (uintptr_t)_parent, DISPATCH_PROC_EXIT, dispatch_get_main_queue());
    if (!_stateSource || !_parentSource) {
        if (_stateSource) { dispatch_resume(_stateSource); dispatch_source_cancel(_stateSource); }
        if (_parentSource) { dispatch_resume(_parentSource); dispatch_source_cancel(_parentSource); }
        [self finish:"could not observe the Rex preview lifecycle"];
        if (_diagnosticFD >= 0) close(_diagnosticFD);
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
    NSEventMask panelInput = NSEventMaskKeyDown | NSEventMaskKeyUp | NSEventMaskFlagsChanged |
        NSEventMaskLeftMouseDown | NSEventMaskRightMouseDown | NSEventMaskOtherMouseDown |
        NSEventMaskLeftMouseUp | NSEventMaskRightMouseUp | NSEventMaskOtherMouseUp |
        NSEventMaskLeftMouseDragged | NSEventMaskRightMouseDragged | NSEventMaskOtherMouseDragged |
        NSEventMaskScrollWheel | NSEventMaskGesture | NSEventMaskMagnify | NSEventMaskSwipe | NSEventMaskRotate |
        NSEventMaskPressure;
    _inputMonitor = [NSEvent addLocalMonitorForEventsMatchingMask:panelInput handler:^NSEvent *(NSEvent *event) {
        MiloPreviewAnchor *anchor = weakSelf;
        if (!anchor || !anchor->_panel || event.window != anchor->_panel) return event;
        if (![anchor acceptsPanelInput] || event.timestamp < anchor->_revealedAt) return nil;
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
    if (_inputMonitor) [NSEvent removeMonitor:_inputMonitor];
    for (NSArray *pair in _notificationTokens) [pair[0] removeObserver:pair[1]];
    for (NSUInteger i = 0; i < _signalSources.count; ++i) {
        dispatch_source_cancel(_signalSources[i]);
        sigaction(signals[i], &oldActions[i], NULL);
    }
    dispatch_source_cancel(_parentSource);
    dispatch_source_cancel(_stateSource);
    _panel.delegate = nil;
    [_panel close];
    if (_diagnosticFD >= 0) close(_diagnosticFD);
    return _status;
}
@end

int milo_preview_run(const char *url, CGWindowID pinned_window, int state_fd) {
    @autoreleasepool {
        if (!NSThread.isMainThread || !url || !pinned_window || state_fd < 0)
            return StartupFailure("invalid native anchor arguments");
        NSString *address = [NSString stringWithUTF8String:url];
        NSURL *location = address ? [NSURL URLWithString:address] : nil;
        pid_t parent = getppid();
        int inputFlags = fcntl(state_fd, F_GETFL);
        if (!location || parent < 2 || inputFlags < 0 || fcntl(state_fd, F_SETFL, inputFlags | O_NONBLOCK) < 0)
            return StartupFailure("invalid URL or missing Rex watcher");
        int focusFD = PrepareFocusInput(state_fd);
        if (focusFD < 0) return StartupFailure("missing Rex focus acknowledgment pipe");
        pid_t owner;
        PinLookup lookup = LookupPin(pinned_window, &owner);
        NSRunningApplication *rex = lookup == PinFound ? RexApplication(owner) : nil;
        if (!rex) {
            close(focusFD);
            return StartupFailure(lookup == PinUnavailable ? "could not verify the pinned Rex window; reopen the preview"
                : "the pinned Rex window is no longer available; reopen the preview");
        }
        // The Rust launcher may disappear before a focus message. Treat that as an I/O error, not SIGPIPE.
        int outputFlags = fcntl(STDOUT_FILENO, F_GETFL);
        if (outputFlags < 0 || fcntl(STDOUT_FILENO, F_SETFL, outputFlags | O_NONBLOCK) < 0 ||
            fcntl(STDOUT_FILENO, F_SETNOSIGPIPE, 1) < 0 ||
            ![[NSApplication sharedApplication] setActivationPolicy:NSApplicationActivationPolicyAccessory]) {
            close(focusFD);
            return StartupFailure("could not initialize the nonactivating panel");
        }
        MiloPreviewAnchor *anchor = [[MiloPreviewAnchor alloc] initWithURL:location rex:rex pinned:pinned_window
            stateFD:state_fd focusFD:focusFD parent:parent];
        if (!anchor) {
            close(focusFD);
            return StartupFailure("could not establish the preview observation clock");
        }
        int status = [anchor run];
        close(focusFD);
        return status;
    }
}
