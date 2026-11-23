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
#import <termios.h>
#import <time.h>
#import <unistd.h>

static const NSTimeInterval SampleInterval = 0.25;
static const NSTimeInterval SampleLifetime = 0.60;
static const NSTimeInterval LeaseLifetime = 3.0;
static const NSTimeInterval PeekLifetime = 12.0;
static const NSTimeInterval PresentationLifetime = 8.0;
// Rex's synchronous RPC is bounded at 15 seconds (src/rex.rs).
static const NSTimeInterval FocusReplyLifetime = 16.0;
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

static BOOL ReadTile(const char *line, size_t length, pid_t *pid, CGRect *normalized, BOOL *guarded) {
    const char *end = line + length;
    if (length >= 6 && !memcmp(line, "guard ", 6)) {
        *guarded = YES;
        line += 6;
    } else if (length >= 5 && !memcmp(line, "show ", 5)) {
        *guarded = NO;
        line += 5;
    } else {
        return NO;
    }
    const char *p = line;
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
    BOOL guarded;
} VisibilityLease;

typedef NS_ENUM(NSUInteger, PeekPhase) { PeekIdle, PeekStarting, PeekVisible };

typedef struct {
    NSTimeInterval deadline, consentEnd;
    PeekPhase phase;
    BOOL ttyReady, announced, discarding, pendingIdleReset, announcedIdleReset;
} GuardedPeek;

static BOOL PeekActive(const GuardedPeek *peek, NSTimeInterval now) {
    return peek->phase != PeekIdle && now < peek->deadline;
}

static void StartPeek(GuardedPeek *peek, NSTimeInterval now) {
    peek->phase = PeekStarting;
    peek->consentEnd = now + PeekLifetime;
    peek->deadline = MIN(now + PresentationLifetime, peek->consentEnd);
}

static BOOL ShowPeek(GuardedPeek *peek, NSTimeInterval now) {
    if (peek->phase != PeekStarting || !PeekActive(peek, now) || now >= peek->consentEnd) return NO;
    peek->phase = PeekVisible;
    peek->deadline = peek->consentEnd;
    return YES;
}

static void EndPeek(GuardedPeek *peek, BOOL resetTTY) {
    if (resetTTY || peek->phase != PeekIdle) peek->ttyReady = NO;
    peek->phase = PeekIdle;
    peek->deadline = peek->consentEnd = 0;
}

static BOOL ConsentAllowsReveal(const VisibilityLease *lease, const GuardedPeek *peek, NSTimeInterval now,
    BOOL pointerInside) {
    return !lease->guarded || (PeekActive(peek, now) && pointerInside);
}

static NSTimeInterval LeaseNow(void) {
    // Unlike uptime, this clock also counts time spent asleep.
    return (NSTimeInterval)clock_gettime_nsec_np(CLOCK_MONOTONIC_RAW) / NSEC_PER_SEC;
}

typedef NS_ENUM(NSUInteger, FocusReply) { FocusWaiting, FocusApproved, FocusEnded, FocusFailed };

static FocusReply ReadFocusReply(int fd, NSTimeInterval now, NSTimeInterval *deadline) {
    if (now >= *deadline) return FocusFailed;
    char byte;
    ssize_t count = read(fd, &byte, 1);
    if (!count) return FocusEnded;
    if (count < 0) return errno == EAGAIN || errno == EINTR ? FocusWaiting : FocusFailed;
    if (byte != '1') return FocusFailed;
    *deadline = 0;
    return FocusApproved;
}

static BOOL LeaseFresh(const VisibilityLease *lease, NSTimeInterval now) {
    return lease->checked > 0 && now >= lease->checked && now - lease->checked < LeaseLifetime;
}

static BOOL LeaseAllowsSampling(const VisibilityLease *lease, NSTimeInterval now, NSTimeInterval focusDeadline) {
    return !focusDeadline && LeaseFresh(lease, now);
}

typedef enum {
    VisibilityUnchanged, VisibilityChanged, VisibilityInvalid, VisibilityWrongOwner, VisibilityUnclaimed,
} VisibilityUpdate;

static VisibilityUpdate UpdateVisibility(VisibilityLease *lease, NSTimeInterval now, pid_t expected,
    const char *line, size_t length) {
    BOOL guardHide = length == 10 && !memcmp(line, "guard-hide", 10);
    if (guardHide || (length == 4 && !memcmp(line, "hide", 4))) {
        if (guardHide) lease->guarded = YES;
        lease->checked = 0;
        return VisibilityChanged;
    }
    if (length == 9 && !memcmp(line, "unclaimed", 9)) return VisibilityUnclaimed;
    CGRect tile;
    BOOL guarded = lease->guarded;
    if (length == 5 && !memcmp(line, "renew", 5)) {
        // A renewal may recover expired eligibility, but cannot undo a hide or grant consent.
        if (!lease->checked) return VisibilityUnchanged;
        tile = lease->tile;
    } else {
        pid_t pid;
        if (!ReadTile(line, length, &pid, &tile, &guarded)) return VisibilityInvalid;
        if (pid != expected) return VisibilityWrongOwner;
        if (lease->guarded && !guarded) {
            // Never turn stale auto-placement records into guarded eligibility.
            lease->checked = 0;
            return VisibilityChanged;
        }
    }
    BOOL unchanged = LeaseFresh(lease, now) && guarded == lease->guarded && CGRectEqualToRect(tile, lease->tile);
    *lease = (VisibilityLease){tile, now, guarded};
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
    if (proven) self.ignoresMouseEvents = NO;
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

static int OpenTTY(const char *path) {
    return open(path, O_RDWR | O_CLOEXEC | O_NOCTTY | O_NONBLOCK);
}

static void WritePeekHint(int tty) {
    static const char hint[] =
        "This preview needs confirmation in this tile. Click the tile and wait for ready to confirm.\n"
        "Keep the pointer inside, then type show and press Return for up to 12 seconds of preview.\n"
        "Before switching sessions, move the pointer out and wait until the page disappears.\n"
        "Input queued before readiness or across a confirmation reset is discarded for safety.\n";
    WriteAll(tty, hint, sizeof(hint) - 1);
}

static BOOL ForegroundCanonicalTTY(int tty) {
    struct termios mode;
    return tcgetpgrp(tty) == getpgrp() && tcgetattr(tty, &mode) == 0 && (mode.c_lflag & ICANON) &&
        (mode.c_iflag & (ICRNL | IGNCR | INLCR)) == ICRNL;
}

static BOOL ArmPeekTTY(GuardedPeek *peek, int tty) {
    static const char ready[] = "Milo Preview: ready to confirm.\n";
    // Only a new consent epoch needs to discard canonical partial input. A
    // sample, a resize, or an ordinary lease renewal is not a new epoch.
    if (!ForegroundCanonicalTTY(tty) || tcflush(tty, TCIFLUSH) < 0 || !ForegroundCanonicalTTY(tty)) return NO;
    if (!peek->announced && !WriteAll(tty, ready, sizeof(ready) - 1)) return NO;
    if (peek->pendingIdleReset && !peek->announcedIdleReset) {
        static const char reset[] =
            "Milo Preview: Rex focus or eligibility changed; any pending input was discarded.\n"
            "Click this tile and type show after focus settles.\n";
        WriteAll(tty, reset, sizeof(reset) - 1);
        peek->announcedIdleReset = YES;
        peek->pendingIdleReset = NO;
    }
    peek->announced = peek->ttyReady = YES;
    peek->discarding = NO;
    return YES;
}

typedef NS_ENUM(NSUInteger, PeekInput) { PeekNoInput, PeekIgnored, PeekShow, PeekHide, PeekBadTTY };

static PeekInput ReadPeekInput(GuardedPeek *peek, int tty) {
    if (!ForegroundCanonicalTTY(tty)) return PeekBadTTY;
    char line[64];
    ssize_t count = read(tty, line, sizeof(line));
    if (count < 0) return errno == EAGAIN ? PeekNoInput : errno == EINTR ? PeekIgnored : PeekBadTTY;
    if (!count) return PeekIgnored; // A canonical EOF record does not prove the queue is empty.
    // Never accept the tail of a long canonical line as a new command.
    BOOL discarding = peek->discarding;
    peek->discarding = count == (ssize_t)sizeof(line) && line[sizeof(line) - 1] != '\n';
    if (!ForegroundCanonicalTTY(tty)) return PeekBadTTY;
    if (discarding || count != 5) return PeekIgnored;
    if (!memcmp(line, "hide\n", 5)) return PeekHide;
    return !memcmp(line, "show\n", 5) ? PeekShow : PeekIgnored;
}

static int PrepareFocusInput(int stateFD) {
    const char *encoded = getenv("MILO_PREVIEW_FOCUS_FD");
    if (!encoded) return -1;
    const char *cursor = encoded, *end = encoded + strlen(encoded);
    uint32_t number;
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

static BOOL WriteStatus(int fd, const char *prefix, const char *reason) {
    char line[256];
    int length = snprintf(line, sizeof(line), "milo preview: %s%s\n", prefix, reason);
    return length > 0 && (size_t)length < sizeof(line) && WriteAll(fd, line, (size_t)length);
}

static BOOL WriteDiagnostic(int fd, const char *reason) {
    return WriteStatus(fd, "", reason);
}

typedef NS_ENUM(NSUInteger, PeekFailure) {
    PeekOK, PeekFocus, PeekLease, PeekStale, PeekStack, PeekPlacement, PeekPointer,
    PeekTTY, PeekWebKit, PeekExpired, PeekClosed, PeekDismissed,
};

static const char *PeekFailureMessage(PeekFailure failure) {
    switch (failure) {
        case PeekFocus: return "the Rex tile lost focus or eligibility; click it and retry";
        case PeekLease: return "Rex stopped confirming tile visibility; wait for the tile to settle or reopen the preview";
        case PeekStale: return "Rex tile geometry is stale or changing; wait for it to settle, then type show and press Return";
        case PeekStack: return "the Rex window stack could not be verified; uncover the tile and use one visible Rex window";
        case PeekPlacement: return "the tile cannot be placed on this display; use a regular window entirely on one display";
        case PeekPointer: return "keep the pointer inside the preview tile and retry";
        case PeekTTY: return "this is not the foreground canonical Rex terminal; click the preview tile or reopen it";
        case PeekWebKit: return "WebKit startup did not finish in time; reopen the preview";
        case PeekExpired: return "the confirmation expired before presentation; type show and press Return again";
        case PeekClosed: return "the preview or its original Rex window closed; reopen it";
        case PeekDismissed: return "the confirmation was canceled; type show and press Return again";
        case PeekOK: return NULL;
    }
    return NULL;
}

static const char *PeekStatusPrefix(PeekPhase phase) {
    return phase == PeekVisible ? "preview hidden: " : "not shown: ";
}

static void WritePeekFailure(int tty, PeekPhase phase, PeekFailure failure) {
    const char *message = PeekFailureMessage(failure);
    if (message) WriteStatus(tty, PeekStatusPrefix(phase), message);
}

@interface MiloPreviewAnchor : NSObject <NSWindowDelegate> {
    NSURL *_url;
    NSRunningApplication *_rex;
    CGWindowID _pinned;
    int _stateFD, _focusFD, _tty;
    pid_t _parent;
    MiloPreviewPanel *_panel;
    NSTimer *_timer, *_peekTimer;
    NSMutableArray *_notificationTokens;
    id _keyMonitor;
    dispatch_queue_t _sampleQueue;
    dispatch_source_t _stateSource, _parentSource;
    NSMutableArray<dispatch_source_t> *_signalSources;
    VisibilityLease _lease;
    GuardedPeek _peek;
    Geometry _candidate;
    NSRect _candidateFrame;
    CGRect _orderedPanelFrame;
    CGDirectDisplayID _candidateDisplay;
    NSTimeInterval _lastSample;
    NSTimeInterval _nextPinCheck;
    NSTimeInterval _focusDeadline;
    NSUInteger _generation;
    PeekFailure _geometryFailure;
    BOOL _samplePending, _initializingPanel, _stableGeometry, _reportedTTYFailure, _finished, _sessionInactive, _screensAsleep;
    char _line[64];
    size_t _lineLength;
    int _status;
}
- (instancetype)initWithURL:(NSURL *)url rex:(NSRunningApplication *)rex pinned:(CGWindowID)pinned
    stateFD:(int)stateFD focusFD:(int)focusFD parent:(pid_t)parent tty:(int)tty confirm:(BOOL)confirm;
- (int)run;
- (void)tick;
- (void)readState;
- (BOOL)readPeek;
@end

@implementation MiloPreviewAnchor
- (instancetype)initWithURL:(NSURL *)url rex:(NSRunningApplication *)rex pinned:(CGWindowID)pinned
    stateFD:(int)stateFD focusFD:(int)focusFD parent:(pid_t)parent tty:(int)tty confirm:(BOOL)confirm {
    if ((self = [super init])) {
        _url = url;
        _rex = rex;
        _pinned = pinned;
        _stateFD = stateFD;
        _focusFD = focusFD;
        _parent = parent;
        _tty = tty;
        _lease.guarded = confirm;
        _sampleQueue = dispatch_queue_create("milo.preview.window", DISPATCH_QUEUE_SERIAL);
        _notificationTokens = [NSMutableArray array];
        _signalSources = [NSMutableArray array];
    }
    return self;
}

- (void)hidePanel {
    if (_panel.placementProven) _panel.placementProven = NO;
    if (_panel.visible) [_panel orderOut:nil];
}

- (void)clearGeometry:(PeekFailure)failure {
    _candidate = (Geometry){0};
    _lastSample = 0;
    _stableGeometry = NO;
    _geometryFailure = failure == PeekStack || failure == PeekPlacement ? failure : PeekStale;
    [self hidePanel];
}

- (PeekFailure)eligibilityFailure {
    if (_sessionInactive || _screensAsleep || _lineLength || _focusDeadline || !FrontmostRex(_rex) || !_lease.checked)
        return PeekFocus;
    return LeaseAllowsSampling(&_lease, LeaseNow(), _focusDeadline) ? PeekOK : PeekLease;
}

- (void)invalidateFor:(PeekFailure)failure resetTTY:(BOOL)resetTTY {
    PeekPhase previous = _peek.phase;
    if (previous == PeekIdle && resetTTY && _peek.ttyReady && failure != PeekOK && failure != PeekDismissed)
        _peek.pendingIdleReset = YES;
    EndPeek(&_peek, resetTTY);
    [_peekTimer invalidate];
    _peekTimer = nil;
    ++_generation;
    [self clearGeometry:failure];
    if (previous != PeekIdle) {
        // Close the old input epoch before telling the user they can retry.
        if (!resetTTY && [self eligibilityFailure] == PeekOK) ArmPeekTTY(&_peek, _tty);
        if (previous != PeekVisible || failure != PeekDismissed) WritePeekFailure(_tty, previous, failure);
    }
}

- (void)expirePeek {
    if (_peek.phase == PeekIdle) return;
    PeekFailure failure = _peek.phase == PeekStarting ? (_initializingPanel ? PeekWebKit : PeekExpired) : PeekOK;
    [self invalidateFor:failure resetTTY:NO];
}

- (void)schedulePeekExpiry {
    [_peekTimer invalidate];
    __weak MiloPreviewAnchor *weakSelf = self;
    _peekTimer = [NSTimer timerWithTimeInterval:MAX(0, _peek.deadline - LeaseNow()) repeats:NO block:^(NSTimer *timer) {
        MiloPreviewAnchor *anchor = weakSelf;
        if (!anchor || anchor->_peekTimer != timer) return;
        if (PeekActive(&anchor->_peek, LeaseNow())) [anchor schedulePeekExpiry];
        else [anchor expirePeek];
    }];
    [NSRunLoop.mainRunLoop addTimer:_peekTimer forMode:NSRunLoopCommonModes];
}

- (void)finish:(const char *)reason {
    if (_finished) return;
    PeekPhase previous = _peek.phase;
    _finished = YES;
    [self invalidateFor:reason ? PeekOK : PeekClosed resetTTY:YES];
    if (reason) {
        _status = 1;
        WriteStatus(_tty, previous == PeekIdle ? "" : PeekStatusPrefix(previous), reason);
    }
    [NSApp stop:nil];
    // Wake the run loop so a command-line AppKit process observes -stop:.
    NSEvent *wake = [NSEvent otherEventWithType:NSEventTypeApplicationDefined location:NSZeroPoint modifierFlags:0
        timestamp:0 windowNumber:0 context:nil subtype:0 data1:0 data2:0];
    [NSApp postEvent:wake atStart:YES];
}

- (void)finishTTYFailure {
    if (_peek.phase == PeekIdle && !_reportedTTYFailure) WritePeekFailure(_tty, PeekIdle, PeekTTY);
    [self invalidateFor:PeekTTY resetTTY:YES];
    _status = 1;
    [self finish:NULL];
}

- (BOOL)canSample {
    return [self eligibilityFailure] == PeekOK;
}

- (BOOL)pointerInsideTile {
    return NSPointInRect(NSEvent.mouseLocation, _candidateFrame);
}

- (BOOL)canReveal {
    return [self canSample] && (!_lease.guarded || ForegroundCanonicalTTY(_tty)) &&
        ConsentAllowsReveal(&_lease, &_peek, LeaseNow(), _lease.guarded && [self pointerInsideTile]);
}

- (BOOL)acceptsPanelInput {
    // AppKit events and WebKit completions can run before the pipe source.
    // Never use a cached lease across a revocation already on the pipe.
    if (_finished || !_panel.visible || !_panel.placementProven) return NO;
    [self readState];
    return !_finished && _panel.visible && _panel.placementProven && _panel.onActiveSpace &&
        (!_lease.guarded || _peek.phase == PeekVisible) && [self canReveal] &&
        NSEqualRects(_panel.frame, _candidateFrame) && SampleFresh(NSProcessInfo.processInfo.systemUptime, _lastSample);
}

- (PeekFailure)proofFailureAt:(NSTimeInterval)started {
    PeekFailure failure = [self eligibilityFailure];
    if (failure != PeekOK) return failure;
    if (_lease.guarded) {
        if (!ForegroundCanonicalTTY(_tty)) return PeekTTY;
        if (![self pointerInsideTile]) return PeekPointer;
        if (!PeekActive(&_peek, LeaseNow())) return PeekExpired;
    }
    return SampleFresh(NSProcessInfo.processInfo.systemUptime, started) ? PeekOK : PeekStale;
}

// YES only after draining the current eligible input queue to an empty read.
- (BOOL)readPeek {
    if (_finished || !_lease.guarded) return NO;
    if (getppid() != _parent || _rex.terminated) { [self finish:NULL]; return NO; }
    // Drain observer records before checking consent, even if no geometry
    // sample can run. Only the foreground canonical controlling TTY is read.
    [self readState];
    if (_finished) return NO;
    PeekFailure failure = [self eligibilityFailure];
    if (failure != PeekOK) {
        if (_peek.ttyReady || _peek.phase != PeekIdle) [self invalidateFor:failure resetTTY:YES];
        if (_peek.announced && ForegroundCanonicalTTY(_tty)) {
            for (NSUInteger i = 0; i < 8; ++i) {
                PeekInput input = ReadPeekInput(&_peek, _tty);
                if (input == PeekNoInput || input == PeekBadTTY) break;
                if (input == PeekShow) WritePeekFailure(_tty, PeekIdle, failure);
            }
        }
        return NO;
    }
    if (!ForegroundCanonicalTTY(_tty)) {
        BOOL reported = _reportedTTYFailure || _peek.phase != PeekIdle;
        if (_peek.ttyReady || _peek.phase != PeekIdle) [self invalidateFor:PeekTTY resetTTY:YES];
        if (!reported) WritePeekFailure(_tty, PeekIdle, PeekTTY);
        _reportedTTYFailure = YES;
        return NO;
    }
    if (_peek.phase != PeekIdle) {
        if (!PeekActive(&_peek, LeaseNow())) { [self expirePeek]; return NO; }
        if (![self pointerInsideTile]) { [self invalidateFor:PeekPointer resetTTY:NO]; return NO; }
    }
    if (!_peek.ttyReady) {
        if (!ArmPeekTTY(&_peek, _tty)) [self finishTTYFailure];
        return NO;
    }
    // Bound work per callback, including attempts with invalid placement.
    for (NSUInteger i = 0; i < 8; ++i) {
        PeekInput input = ReadPeekInput(&_peek, _tty);
        if (input == PeekNoInput) return YES;
        if (input == PeekBadTTY) {
            [self finishTTYFailure];
            return NO;
        }
        if (_peek.pendingIdleReset) {
            // After the first reset, only explain a discarded prefix if the
            // user actually completes an unrecognized command. Never echo it.
            if (input == PeekIgnored)
                WriteDiagnostic(_tty, "input followed a Rex focus change; type the complete show command and press Return");
            _peek.pendingIdleReset = NO;
        }
        if (input == PeekHide) {
            [self invalidateFor:PeekDismissed resetTTY:_peek.phase == PeekIdle];
            if (!_peek.ttyReady && [self eligibilityFailure] == PeekOK) ArmPeekTTY(&_peek, _tty);
            return NO;
        }
        if (input != PeekShow || _peek.phase != PeekIdle) continue;
        NSTimeInterval received = LeaseNow();
        failure = [self eligibilityFailure];
        if (failure == PeekOK && (!_stableGeometry ||
            !SampleFresh(NSProcessInfo.processInfo.systemUptime, _lastSample))) {
            failure = _geometryFailure ? _geometryFailure : PeekStale;
        }
        if (failure == PeekOK && ![self pointerInsideTile]) failure = PeekPointer;
        if (failure != PeekOK) {
            WritePeekFailure(_tty, PeekIdle, failure);
            continue;
        }
        if (!WriteDiagnostic(_tty, "confirmation accepted; checking the tile")) {
            [self finish:NULL];
            return NO;
        }
        StartPeek(&_peek, received);
        ++_generation; // An in-flight sample may have started before the command.
        _lastSample = 0; // Keep the geometry identity, not its pre-consent proof.
        _stableGeometry = NO;
        [self schedulePeekExpiry];
    }
    return NO; // More input, including a dismissal, may still be queued.
}

- (void)handleLine:(size_t)length {
    VisibilityLease previous = _lease;
    NSTimeInterval now = LeaseNow();
    VisibilityUpdate update = UpdateVisibility(&_lease, now, _rex.processIdentifier, _line, length);
    switch (update) {
        case VisibilityUnchanged: break;
        case VisibilityChanged:
            if (!_lease.checked || previous.guarded != _lease.guarded)
                [self invalidateFor:PeekFocus resetTTY:YES];
            else if (previous.checked && !LeaseFresh(&previous, now))
                [self invalidateFor:PeekLease resetTTY:YES];
            else
                [self invalidateFor:PeekStale resetTTY:NO];
            break;
        case VisibilityInvalid: [self finish:"invalid Rex visibility message"]; break;
        case VisibilityWrongOwner: [self finish:"the Rex client changed; reopen the preview in the current Rex client"]; break;
        case VisibilityUnclaimed:
            [self finish:"the Mac viewer did not claim this preview tile. Close it and retry from a Mac-created working terminal; no permissions are needed."];
            break;
    }
    if (!previous.guarded && _lease.guarded) WritePeekHint(_tty);
}

- (void)readState {
    // Bound continuous writer activity and never grant consent while a
    // previously queued visibility revocation might still be unread.
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
    if (!_finished) [self finish:"too many pending Rex visibility messages; reopen the preview"];
}

- (void)returnToTerminal {
    if (!_panel.keyWindow || ![self acceptsPanelInput]) return;
    // Keep the sample and click gates closed even if the watcher renews the
    // visible tile while Rex is still processing the focus request.
    _focusDeadline = LeaseNow() + FocusReplyLifetime;
    [self invalidateFor:PeekDismissed resetTTY:YES];
    static const char focus[] = "focus\n";
    static const char guardedFocus[] = "focus-confirm\n";
    BOOL sent = _lease.guarded ? WriteAll(STDOUT_FILENO, guardedFocus, sizeof(guardedFocus) - 1)
        : WriteAll(STDOUT_FILENO, focus, sizeof(focus) - 1);
    if (!sent) [self finish:"could not return focus to the Rex terminal"];
}

- (void)rejectProof:(PeekFailure)failure {
    if (failure == PeekExpired) {
        [self expirePeek];
        return;
    }
    BOOL hard = failure == PeekFocus || failure == PeekLease || failure == PeekTTY;
    [self invalidateFor:failure resetTTY:hard];
    if (failure == PeekTTY) _reportedTTYFailure = YES;
}

- (void)applySample:(Geometry)geometry checkedPanel:(BOOL)checkedPanel lostPin:(BOOL)lostPin
    generation:(NSUInteger)generation started:(NSTimeInterval)started {
    _samplePending = NO;
    if (_finished) return;
    if (getppid() != _parent || _rex.terminated) { [self finish:NULL]; return; }
    if (lostPin) { [self finish:NULL]; return; }
    // Prefer queued observer revocation over staging or revealing a panel.
    if (_lease.guarded) [self readState];
    if (_finished) return;
    if (generation != _generation) return;
    PeekFailure failure = [self eligibilityFailure];
    if (failure == PeekOK) {
        if (!SampleFresh(NSProcessInfo.processInfo.systemUptime, started)) failure = PeekStale;
        else if (!geometry.windowID) failure = PeekStack;
    }
    NSRect frame;
    CGDirectDisplayID display;
    if (failure == PeekOK && !ScreenFrame(geometry, &frame, &display)) failure = PeekPlacement;
    if (failure == PeekOK && !SampleFresh(NSProcessInfo.processInfo.systemUptime, started)) failure = PeekStale;
    if (failure != PeekOK) {
        [self rejectProof:failure];
        return;
    }
    if (!SameGeometry(_candidate, geometry) || display != _candidateDisplay ||
        !NSEqualRects(frame, _candidateFrame)) {
        [self invalidateFor:PeekStale resetTTY:NO];
        _candidate = geometry;
        _candidateFrame = frame;
        _candidateDisplay = display;
        _lastSample = started;
        return;
    }
    _lastSample = started;
    _stableGeometry = YES;
    _geometryFailure = PeekOK;
    if (_lease.guarded && _peek.phase == PeekIdle) return;
    failure = [self proofFailureAt:started];
    if (failure != PeekOK) { [self rejectProof:failure]; return; }
    if (!_panel) {
        // The old proof cannot survive a blocked or nested AppKit run loop
        // during WebKit construction. No panel is ordered in this phase.
        _lastSample = 0;
        _stableGeometry = NO;
        _initializingPanel = YES;
        __weak MiloPreviewAnchor *weakSelf = self;
        NSView *browser = MiloPreviewContent(_url, ^BOOL{ return [weakSelf acceptsPanelInput]; },
            ^{ [weakSelf returnToTerminal]; });
        _initializingPanel = NO;
        if (!browser) {
            [self finish:"expected an explicit loopback HTTP or HTTPS URL"];
            return;
        }
        if (_finished || generation != _generation) return;
        if (_lease.guarded && !PeekActive(&_peek, LeaseNow())) { [self rejectProof:PeekWebKit]; return; }
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
        // Keep the bounded, hidden attempt tied to the same candidate. Only
        // fresh Quartz proof taken after initialization can stage the panel.
        [self tick];
        return;
    }
    if (_panel.visible) {
        if (!checkedPanel || !_panel.onActiveSpace || !NSEqualRects(_panel.frame, frame)) {
            [self rejectProof:PeekStack];
            return;
        }
        if (!_panel.placementProven) {
            if (_lease.guarded) {
                if (![self readPeek] || _finished || generation != _generation) return;
                failure = [self proofFailureAt:started];
                if (failure != PeekOK) { [self rejectProof:failure]; return; }
                if (!_panel.visible || !_panel.onActiveSpace || !NSEqualRects(_panel.frame, frame)) {
                    [self rejectProof:PeekStack];
                    return;
                }
                if (!ShowPeek(&_peek, LeaseNow())) { [self expirePeek]; return; }
                [self schedulePeekExpiry];
            }
            _panel.placementProven = YES;
            if (_lease.guarded && !_finished && generation == _generation && _panel.placementProven)
                WriteDiagnostic(_tty, "preview visible");
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
        [self rejectProof:PeekPlacement];
        return;
    }
    NSInteger relativeWindow = [NSWindow windowNumberAtPoint:center belowWindowWithWindowNumber:0];
    if (_finished || generation != _generation) return;
    failure = [self proofFailureAt:started];
    if (failure == PeekOK && (relativeWindow <= 0 || !NSEqualRects(_panel.frame, frame))) failure = PeekStack;
    if (failure != PeekOK) {
        [self rejectProof:failure];
        return;
    }
    _orderedPanelFrame = CGRectMake(NSMinX(frame), primary.size.height - NSMaxY(frame),
        NSWidth(frame), NSHeight(frame));
    if (_panel.placementProven) _panel.placementProven = NO;
    [_panel orderWindow:NSWindowAbove relativeTo:relativeWindow];
    if (_finished || generation != _generation) { [self hidePanel]; return; }
    failure = [self proofFailureAt:started];
    if (failure == PeekOK && (!_panel.visible || !NSEqualRects(_panel.frame, frame))) failure = PeekStack;
    if (failure != PeekOK) {
        [self rejectProof:failure];
        return;
    }
    // Verify the clear, click-through panel before revealing the browser.
    [self tick];
}

- (void)tick {
    if (_finished) return;
    if (getppid() != _parent) { [self finish:NULL]; return; }
    if (_rex.terminated) { [self finish:NULL]; return; }
    if (_focusDeadline) {
        FocusReply reply = ReadFocusReply(_focusFD, LeaseNow(), &_focusDeadline);
        if (reply == FocusEnded) { [self finish:NULL]; return; }
        if (reply == FocusFailed) { [self finish:"could not confirm focus returned to the Rex terminal"]; return; }
    }
    // Nonblocking canonical input is serviced even while Quartz is stalled
    // or the tile is currently ineligible; never wait for a sample result.
    if (_lease.guarded) [self readPeek];
    if (_finished) return;
    PeekFailure eligibility = [self eligibilityFailure];
    BOOL canSample = eligibility == PeekOK;
    NSTimeInterval now = NSProcessInfo.processInfo.systemUptime;
    if (!canSample && (_candidate.windowID || _panel.visible || _peek.ttyReady))
        [self rejectProof:eligibility];
    else if (_peek.phase != PeekIdle && !PeekActive(&_peek, LeaseNow()))
        [self expirePeek];
    else if (_peek.phase != PeekIdle && ![self pointerInsideTile])
        [self rejectProof:PeekPointer];
    else if (_panel.visible && !_panel.onActiveSpace)
        [self invalidateFor:PeekStack resetTTY:YES];
    else if (_lastSample && !SampleFresh(now, _lastSample))
        [self rejectProof:PeekStale];
    if (_samplePending || _initializingPanel) return;
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
                if (canSample || lostPin) {
                    [anchor applySample:geometry checkedPanel:checkedPanel lostPin:lostPin generation:generation started:now];
                } else if (anchor) {
                    anchor->_samplePending = NO;
                }
            });
        }
    });
}

- (void)windowWillClose:(NSNotification *)notification {
    (void)notification;
    [self finish:NULL];
}

- (void)windowDidResignKey:(NSNotification *)notification {
    (void)notification;
    if (_lease.guarded && _peek.phase == PeekVisible) [self invalidateFor:PeekFocus resetTTY:YES];
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
            [anchor rejectProof:PeekStale];
            return;
        }
        if ([event.name isEqualToString:NSWorkspaceDidDeactivateApplicationNotification]) {
            NSRunningApplication *app = event.userInfo[NSWorkspaceApplicationKey];
            if (app && app.processIdentifier != anchor->_rex.processIdentifier) return;
        }
        if ([event.name isEqualToString:NSWorkspaceDidActivateApplicationNotification] && FrontmostRex(anchor->_rex)) return;
        [anchor invalidateFor:PeekFocus resetTTY:YES];
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
            PeekFailure failure = [anchor proofFailureAt:anchor->_lastSample];
            [anchor rejectProof:failure == PeekOK ? PeekStack : failure];
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

int milo_preview_run(const char *url, CGWindowID pinned_window, int state_fd, bool confirm) {
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
        int tty = OpenTTY("/dev/tty");
        if (tty < 0) {
            fprintf(stderr, "milo preview: no controlling Rex terminal\n");
            return 1;
        }
        int focusFD = PrepareFocusInput(state_fd);
        if (focusFD < 0) {
            WriteDiagnostic(tty, "missing Rex focus acknowledgment pipe");
            close(tty);
            return 1;
        }
        pid_t owner;
        PinLookup lookup = LookupPin(pinned_window, &owner);
        NSRunningApplication *rex = lookup == PinFound ? RexApplication(owner) : nil;
        if (!rex) {
            WriteDiagnostic(tty, lookup == PinUnavailable ? "could not verify the pinned Rex window; reopen the preview"
                : "the pinned Rex window is no longer available; reopen the preview");
            close(focusFD);
            close(tty);
            return 1;
        }
        // The Rust launcher may disappear before a focus message. Treat that as an I/O error, not SIGPIPE.
        // A foreground-group race on TTY read or flush must not stop the
        // helper (or the launcher's process group).
        int outputFlags = fcntl(STDOUT_FILENO, F_GETFL);
        if (outputFlags < 0 || fcntl(STDOUT_FILENO, F_SETFL, outputFlags | O_NONBLOCK) < 0 ||
            fcntl(STDOUT_FILENO, F_SETNOSIGPIPE, 1) < 0 ||
            signal(SIGTTIN, SIG_IGN) == SIG_ERR || signal(SIGTTOU, SIG_IGN) == SIG_ERR ||
            ![[NSApplication sharedApplication] setActivationPolicy:NSApplicationActivationPolicyAccessory]) {
            WriteDiagnostic(tty, "could not initialize the nonactivating panel");
            close(focusFD);
            close(tty);
            return 1;
        }
        static const char waiting[] = "Milo Preview: approximate placement. Waiting for a supported, visible Rex tile.\n";
        WriteAll(tty, waiting, sizeof(waiting) - 1);
        if (confirm) WritePeekHint(tty);
        MiloPreviewAnchor *anchor = [[MiloPreviewAnchor alloc] initWithURL:location rex:rex pinned:pinned_window
            stateFD:state_fd focusFD:focusFD parent:parent tty:tty confirm:confirm];
        int status = [anchor run];
        close(focusFD);
        close(tty);
        return status;
    }
}
