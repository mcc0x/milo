#import "Anchor.h"
#import "Browser.h"

#import <AppKit/AppKit.h>
#import <ApplicationServices/ApplicationServices.h>
#import <dispatch/dispatch.h>
#import <errno.h>
#import <fcntl.h>
#import <limits.h>
#import <signal.h>
#import <stdio.h>
#import <string.h>
#import <math.h>
#import <sys/ioctl.h>
#import <termios.h>
#import <unistd.h>

static const double LayoutProofInterval = 1.0;
static const double LayoutProofTimeout = 6.0;
static const double ProofInterval = 0.15;
static const double ProofLifetime = 0.35;
static const float AXMessageTimeout = 0.05f;
static const NSUInteger MarkerLength = 32;
static const char AccessibilityUnavailable[] = "macOS Accessibility is unavailable for this launch context. No permission was requested.";

typedef struct {
    CGRect window;
    CGRect pane;
    CGWindowID windowID;
} Geometry;

static BOOL SameGeometry(Geometry a, Geometry b) {
    return a.windowID == b.windowID && CGRectEqualToRect(a.window, b.window) && CGRectEqualToRect(a.pane, b.pane);
}

typedef struct {
    NSTimeInterval deadline;
    NSTimeInterval (*clock)(void *);
    void *context;
} AXBudget;

static NSTimeInterval ProofClock(void *context) {
    (void)context;
    return NSProcessInfo.processInfo.systemUptime;
}

static BOOL AXCanIssue(const AXBudget *budget) {
    // Leave time for an outstanding synchronous AX message to hit its system timeout.
    return budget->deadline - budget->clock(budget->context) > AXMessageTimeout;
}

static BOOL BeforeAXDeadline(const AXBudget *budget) {
    return budget->clock(budget->context) < budget->deadline;
}

static BOOL ValidRect(CGRect r) {
    return isfinite(r.origin.x) && isfinite(r.origin.y) && isfinite(r.size.width) && isfinite(r.size.height) &&
        r.size.width > 0 && r.size.height > 0;
}

static BOOL HasType(id value, CFTypeID type) {
    return value != nil && CFGetTypeID((__bridge CFTypeRef)value) == type;
}

static BOOL IsAXString(id value, CFStringRef expected) {
    return HasType(value, CFStringGetTypeID()) && CFEqual((__bridge CFTypeRef)value, expected);
}

static AXUIElementRef Element(id value) {
    return HasType(value, AXUIElementGetTypeID()) ? (__bridge AXUIElementRef)value : NULL;
}

static id Attribute(const AXBudget *budget, AXUIElementRef element, CFStringRef attribute) {
    CFTypeRef value = NULL;
    if (!element || !AXCanIssue(budget)) return nil;
    AXError status = AXUIElementCopyAttributeValue(element, attribute, &value);
    id result = CFBridgingRelease(value);
    return status == kAXErrorSuccess && BeforeAXDeadline(budget) ? result : nil;
}

static NSArray *Attributes(const AXBudget *budget, AXUIElementRef element, NSArray *keys) {
    if (!AXCanIssue(budget)) return nil;
    CFArrayRef raw = NULL;
    AXError status = AXUIElementCopyMultipleAttributeValues(element, (__bridge CFArrayRef)keys, 0, &raw);
    NSArray *result = CFBridgingRelease(raw);
    return status == kAXErrorSuccess && BeforeAXDeadline(budget) ? result : nil;
}

static id ParameterizedAttribute(const AXBudget *budget, AXUIElementRef element, CFStringRef attribute, AXValueRef parameter) {
    if (!AXCanIssue(budget)) return nil;
    CFTypeRef raw = NULL;
    AXError status = AXUIElementCopyParameterizedAttributeValue(element, attribute, parameter, &raw);
    id result = CFBridgingRelease(raw);
    return status == kAXErrorSuccess && BeforeAXDeadline(budget) ? result : nil;
}

static BOOL ReadAXValue(id value, AXValueType type, void *out) {
    return HasType(value, AXValueGetTypeID()) && AXValueGetValue((__bridge AXValueRef)value, type, out);
}

typedef enum { MarkerUnreadable, MarkerMissing, MarkerText, MarkerMatched } MarkerStatus;

static MarkerStatus ReadMarker(const AXBudget *budget, AXUIElementRef element, NSString *marker, CGRect frame) {
    CFRange visible;
    if (!ReadAXValue(Attribute(budget, element, kAXVisibleCharacterRangeAttribute), kAXValueCFRangeType, &visible) ||
        visible.location < 0 || visible.location > LONG_MAX - (CFIndex)MarkerLength || visible.length < 0) return MarkerUnreadable;
    if (visible.length < (CFIndex)MarkerLength) return MarkerMissing;

    // Only the first visible marker-length span is read. Never fetch a pane's AXValue or scrollback.
    CFRange prefix = CFRangeMake(visible.location, (CFIndex)MarkerLength);
    AXValueRef parameter = AXValueCreate(kAXValueCFRangeType, &prefix);
    if (!parameter) return MarkerUnreadable;
    id text = ParameterizedAttribute(budget, element, kAXStringForRangeParameterizedAttribute, parameter);
    BOOL readable = HasType(text, CFStringGetTypeID());
    BOOL matches = readable && CFEqual((__bridge CFTypeRef)text, (__bridge CFStringRef)marker);
    CGRect markerFrame;
    BOOL bounded = matches && ReadAXValue(ParameterizedAttribute(budget, element, kAXBoundsForRangeParameterizedAttribute, parameter),
        kAXValueCGRectType, &markerFrame) && ValidRect(markerFrame) && CGRectContainsRect(frame, markerFrame);
    CFRelease(parameter);
    if (bounded) return MarkerMatched;
    if (matches) return MarkerText;
    return readable ? MarkerMissing : MarkerUnreadable;
}

// A bounded walk of the focused window, not the application's other windows or tabs.
// Roles, not titles or window-chrome insets, define the terminal and its exclusion regions.
enum { MaxAXNodes = 192, MaxAXDepth = 24, MaxBackgroundChecks = 12, NoNode = -1 };

typedef struct {
    CGRect frame;
    int parent;
    BOOL container;
    BOOL splitGroup;
    BOOL tiledControl;
} AXNode;

typedef struct {
    AXNode nodes[MaxAXNodes];
    int count;
    int markerNode;
    AXUIElementRef markerElement;
    AXUIElementRef focus;
    BOOL terminalFocused;
    BOOL markerMissing, markerPresent;
} AXTree;

@interface MiloAXProof : NSObject {
@public
    Geometry geometry;
    id window, pane, focus;
    NSTimeInterval layoutCheckedAt;
    CGRect backgrounds[MaxBackgroundChecks];
    size_t backgroundCount;
}
@end

@implementation MiloAXProof
@end

static BOOL IsPalette(id role, id subrole) {
    return IsAXString(role, kAXMenuRole) || IsAXString(role, kAXMenuItemRole) ||
        IsAXString(role, kAXSheetRole) || IsAXString(role, CFSTR("AXPopover")) ||
        IsAXString(role, CFSTR("AXDialog")) || IsAXString(subrole, kAXDialogSubrole) ||
        IsAXString(subrole, kAXSystemDialogSubrole) || IsAXString(subrole, kAXFloatingWindowSubrole) ||
        IsAXString(subrole, CFSTR("AXSearchField"));
}

static CGRect MeasuredFrame(id positionValue, id sizeValue) {
    CGPoint position;
    CGSize size;
    if (!ReadAXValue(positionValue, kAXValueCGPointType, &position) || !ReadAXValue(sizeValue, kAXValueCGSizeType, &size)) {
        return CGRectNull;
    }
    CGRect frame = {position, size};
    return ValidRect(frame) ? frame : CGRectNull;
}

static CGRect ElementFrame(const AXBudget *budget, AXUIElementRef element) {
    static NSArray *keys;
    static dispatch_once_t once;
    dispatch_once(&once, ^{
        keys = @[(__bridge NSString *)kAXPositionAttribute, (__bridge NSString *)kAXSizeAttribute];
    });
    NSArray *values = Attributes(budget, element, keys);
    return values.count == 2 ? MeasuredFrame(values[0], values[1]) : CGRectNull;
}

static BOOL WalkAX(const AXBudget *budget, AXTree *tree, AXUIElementRef element, int parent, unsigned depth, NSString *marker) {
    if (!element || depth >= MaxAXDepth || tree->count >= MaxAXNodes) return NO;
    static NSArray *keys;
    static dispatch_once_t once;
    dispatch_once(&once, ^{
        keys = @[
            (__bridge NSString *)kAXRoleAttribute, (__bridge NSString *)kAXSubroleAttribute,
            (__bridge NSString *)kAXPositionAttribute, (__bridge NSString *)kAXSizeAttribute,
            (__bridge NSString *)kAXChildrenAttribute,
        ];
    });
    NSArray *values = Attributes(budget, element, keys);
    if (values.count != keys.count) return NO;
    id role = values[0], subrole = values[1];
    if (!HasType(role, CFStringGetTypeID()) || IsPalette(role, subrole) ||
        (IsAXString(role, kAXWindowRole) != (parent == NoNode))) return NO;

    CGRect frame = MeasuredFrame(values[2], values[3]);
    BOOL splitGroup = IsAXString(role, kAXSplitGroupRole);
    BOOL container = splitGroup || IsAXString(role, kAXGroupRole) || IsAXString(role, kAXScrollAreaRole);
    BOOL textArea = IsAXString(role, kAXTextAreaRole);
    BOOL tiledControl = container || textArea || IsAXString(role, kAXSplitterRole) || IsAXString(role, kAXScrollBarRole);
    int index = tree->count++;
    tree->nodes[index] = (AXNode){frame, parent, container, splitGroup, tiledControl};
    NSArray *children = HasType(values[4], CFArrayGetTypeID()) ? values[4] : nil;
    if (!children) {
        AXError error;
        if (!ReadAXValue(values[4], kAXValueAXErrorType, &error) ||
            (error != kAXErrorNoValue && error != kAXErrorAttributeUnsupported)) return NO;
    }
    if (textArea && !CGRectIsNull(frame)) {
        if (CFEqual(element, tree->focus)) tree->terminalFocused = YES;
        MarkerStatus status = ReadMarker(budget, element, marker, frame);
        tree->markerMissing |= status == MarkerMissing;
        tree->markerPresent |= status == MarkerText || status == MarkerMatched;
        if (status == MarkerMatched) {
            // A text pane may be a leaf, but it cannot itself contain other controls to cover.
            if (tree->markerNode != NoNode || children.count) return NO;
            tree->markerNode = index;
            CFRetain(element);
            tree->markerElement = element;
        }
    }
    for (id child in children) {
        if (!WalkAX(budget, tree, Element(child), index, depth + 1, marker)) return NO;
    }
    return BeforeAXDeadline(budget);
}

static BOOL IsAncestor(const AXTree *tree, int ancestor, int child) {
    for (int i = tree->nodes[child].parent; i != NoNode; i = tree->nodes[i].parent) {
        if (i == ancestor) return YES;
    }
    return NO;
}

static BOOL InTiledBackground(const AXTree *tree, int sibling) {
    if (!tree->nodes[sibling].tiledControl) return NO;
    for (int i = sibling; i != NoNode && !IsAncestor(tree, i, tree->markerNode); i = tree->nodes[i].parent) {
        if (tree->nodes[i].splitGroup) return YES;
    }
    return NO;
}

static BOOL ClearAXPane(const AXTree *tree, BOOL floating, CGRect *backgrounds, size_t *count) {
    *count = 0;
    CGRect pane = tree->nodes[tree->markerNode].frame;
    if (!CGRectContainsRect(tree->nodes[0].frame, pane)) return NO;
    for (int i = 1; i < tree->count; ++i) {
        if (i == tree->markerNode) continue;
        if (IsAncestor(tree, i, tree->markerNode)) {
            // Respect clipping by every measurable ancestor of the text area.
            if (!CGRectIsNull(tree->nodes[i].frame) && !CGRectContainsRect(tree->nodes[i].frame, pane)) return NO;
            continue;
        }
        CGRect sibling = tree->nodes[i].frame;
        if (CGRectIsNull(sibling)) {
            if (!tree->nodes[i].container) return NO;
            continue;
        }
        CGRect intersection = CGRectIntersection(sibling, pane);
        if (CGRectIsEmpty(intersection)) continue;
        // Rex's watcher verifies the marked float is topmost. A separate AXSplitGroup may lie
        // beneath it, but a sibling float, arbitrary control, or uncertain layer is never ignored.
        if (!floating || !InTiledBackground(tree, i)) return NO;
        if (CGRectEqualToRect(intersection, pane)) continue; // Already covered by the whole-pane hit test.
        BOOL duplicate = NO;
        for (size_t j = 0; j < *count; ++j) duplicate |= CGRectEqualToRect(backgrounds[j], intersection);
        if (duplicate) continue;
        if (*count == MaxBackgroundChecks) return NO;
        backgrounds[(*count)++] = intersection;
    }
    return YES;
}

static BOOL HitTestsMatch(const AXBudget *budget, AXUIElementRef app, AXUIElementRef pane, CGRect frame) {
    if (!ValidRect(frame)) return NO;
    CGFloat dx = MIN(1, frame.size.width / 4), dy = MIN(1, frame.size.height / 4);
    const CGFloat xs[] = {CGRectGetMinX(frame) + dx, CGRectGetMidX(frame), CGRectGetMaxX(frame) - dx};
    const CGFloat ys[] = {CGRectGetMinY(frame) + dy, CGRectGetMidY(frame), CGRectGetMaxY(frame) - dy};
    for (unsigned y = 0; y < 3; ++y) {
        for (unsigned x = 0; x < 3; ++x) {
            if (!AXCanIssue(budget)) return NO;
            AXUIElementRef hit = NULL;
            BOOL matches = AXUIElementCopyElementAtPosition(app, (float)xs[x], (float)ys[y], &hit) == kAXErrorSuccess &&
                hit && CFEqual(hit, pane);
            if (hit) CFRelease(hit);
            if (!matches || !BeforeAXDeadline(budget)) return NO;
        }
    }
    return YES;
}

static CGRect WindowBounds(NSDictionary *window) {
    id bounds = window[(__bridge NSString *)kCGWindowBounds];
    CGRect r;
    return HasType(bounds, CFDictionaryGetTypeID()) && CGRectMakeWithDictionaryRepresentation((__bridge CFDictionaryRef)bounds, &r) &&
        ValidRect(r) ? r : CGRectNull;
}

static CGWindowID MatchingVisibleWindow(NSArray<NSDictionary *> *windows, pid_t pid, pid_t selfPID,
    CGRect windowFrame, CGRect paneFrame) {
    CGWindowID match = kCGNullWindowID;
    BOOL blocked = NO;
    int normal = CGWindowLevelForKey(kCGNormalWindowLevelKey);
    for (NSDictionary *window in windows) {
        NSNumber *number = window[(__bridge NSString *)kCGWindowNumber];
        NSNumber *owner = window[(__bridge NSString *)kCGWindowOwnerPID];
        NSNumber *layer = window[(__bridge NSString *)kCGWindowLayer];
        if (!HasType(layer, CFNumberGetTypeID())) layer = nil;
        NSNumber *alpha = window[(__bridge NSString *)kCGWindowAlpha];
        CGWindowID identifier = number.unsignedIntValue;
        if (owner && owner.intValue == selfPID) continue;
        // A normal-level panel cannot cover floating system windows, even with full-display bounds.
        if (layer && layer.intValue > normal) continue;
        CGRect frame = WindowBounds(window);
        BOOL rex = owner && owner.intValue == pid;
        if (rex && layer && layer.intValue == normal &&
            CGRectEqualToRect(frame, windowFrame)) {
            if (!identifier || match || blocked || !alpha || alpha.doubleValue <= 0) return kCGNullWindowID;
            match = identifier;
        } else if (!match) {
            if (alpha && alpha.doubleValue == 0) continue;
            // An earlier Rex window at the same level could be a palette. Missing z-order metadata is unsafe.
            if (rex || !identifier || !owner || !layer || CGRectIsNull(frame) ||
                !CGRectIsEmpty(CGRectIntersection(frame, paneFrame))) blocked = YES;
        }
    }
    return match;
}

static CGWindowID VisibleWindow(pid_t pid, Geometry geometry) {
    NSArray<NSDictionary *> *windows = CFBridgingRelease(CGWindowListCopyWindowInfo(
        kCGWindowListOptionOnScreenOnly | kCGWindowListExcludeDesktopElements, kCGNullWindowID));
    return MatchingVisibleWindow(windows, pid, getpid(), geometry.window, geometry.pane);
}

static BOOL Nonmodal(const AXBudget *budget, AXUIElementRef window) {
    if (!AXCanIssue(budget)) return NO;
    CFTypeRef value = NULL;
    AXError status = AXUIElementCopyAttributeValue(window, kAXModalAttribute, &value);
    BOOL nonmodal = status == kAXErrorSuccess ? value == kCFBooleanFalse :
        status == kAXErrorAttributeUnsupported || status == kAXErrorNoValue;
    if (value) CFRelease(value);
    return nonmodal && BeforeAXDeadline(budget);
}

static BOOL CurrentAXElement(const AXBudget *budget, AXUIElementRef app, CFStringRef key, id expected) {
    id actual = Attribute(budget, app, key);
    return Element(actual) && CFEqual((__bridge CFTypeRef)actual, (__bridge CFTypeRef)expected);
}

static BOOL ValidateProof(const AXBudget *budget, AXUIElementRef app, MiloAXProof *proof, pid_t pid,
    NSString *marker, BOOL full) {
    Geometry g = proof->geometry;
    AXUIElementRef window = Element(proof->window), pane = Element(proof->pane);
    if (!CurrentAXElement(budget, app, kAXFocusedWindowAttribute, proof->window) ||
        !CurrentAXElement(budget, app, kAXFocusedUIElementAttribute, proof->focus) ||
        !Nonmodal(budget, window) || !CGRectEqualToRect(ElementFrame(budget, window), g.window) ||
        !CGRectEqualToRect(ElementFrame(budget, pane), g.pane) || ReadMarker(budget, pane, marker, g.pane) != MarkerMatched ||
        !HitTestsMatch(budget, app, pane, g.pane)) return NO;
    if (full) {
        for (size_t i = 0; i < proof->backgroundCount; ++i) {
            if (!HitTestsMatch(budget, app, pane, proof->backgrounds[i])) return NO;
        }
    }
    return CurrentAXElement(budget, app, kAXFocusedWindowAttribute, proof->window) &&
        CurrentAXElement(budget, app, kAXFocusedUIElementAttribute, proof->focus) &&
        VisibleWindow(pid, g) == g.windowID && BeforeAXDeadline(budget);
}

static MiloAXProof *ProveGeometry(AXUIElementRef app, pid_t pid, NSString *marker,
    BOOL floating, NSTimeInterval now, MiloAXProof *cached, BOOL *markerNeedsRedraw) {
    *markerNeedsRedraw = NO;
    AXBudget budget = {now + ProofLifetime, ProofClock, NULL};
    if (!AXCanIssue(&budget) || !AXIsProcessTrusted()) return nil;
    if (cached) return ValidateProof(&budget, app, cached, pid, marker, NO) ? cached : nil;
    __attribute__((objc_precise_lifetime)) id focusedWindow = Attribute(&budget, app, kAXFocusedWindowAttribute);
    __attribute__((objc_precise_lifetime)) id focusedElement = Attribute(&budget, app, kAXFocusedUIElementAttribute);
    if (!IsAXString(Attribute(&budget, Element(focusedElement), kAXRoleAttribute), kAXTextAreaRole)) return nil;
    AXUIElementRef window = Element(focusedWindow);
    AXTree tree = {.markerNode = NoNode, .focus = Element(focusedElement)};
    MiloAXProof *proof = nil;
    BOOL walked = window && tree.focus && Nonmodal(&budget, window) && WalkAX(&budget, &tree, window, NoNode, 0, marker);
    if (walked && tree.terminalFocused) {
        // Do not disturb a present marker on placement or occlusion failures, or repaint when AX cannot read it.
        *markerNeedsRedraw = tree.markerMissing && !tree.markerPresent;
    }
    if (walked && tree.markerNode != NoNode && tree.terminalFocused) {
        Geometry g = {.window = tree.nodes[0].frame, .pane = tree.nodes[tree.markerNode].frame};
        g.windowID = VisibleWindow(pid, g);
        if (g.windowID) {
            proof = [[MiloAXProof alloc] init];
            proof->geometry = g;
            proof->window = focusedWindow;
            proof->pane = (__bridge id)tree.markerElement;
            proof->focus = focusedElement;
            proof->layoutCheckedAt = now;
            if (!ClearAXPane(&tree, floating, proof->backgrounds, &proof->backgroundCount) ||
                !ValidateProof(&budget, app, proof, pid, marker, YES)) proof = nil;
        }
    }
    if (tree.markerElement) CFRelease(tree.markerElement);
    if (!BeforeAXDeadline(&budget)) {
        *markerNeedsRedraw = NO;
        return nil;
    }
    return proof;
}

typedef struct {
    NSRect frame, visibleFrame;
    CGFloat scale;
    CGDirectDisplayID id;
    BOOL mirrored;
} PreviewScreen;

// AX and Quartz use the primary display's top-left as origin; AppKit uses its bottom-left.
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

@interface MiloPreviewPanel : NSPanel
@end

@implementation MiloPreviewPanel
- (BOOL)canBecomeKeyWindow { return YES; }
- (BOOL)canBecomeMainWindow { return NO; }
@end

// Both callers open their output nonblocking. A stalled PTY does not get to hold
// the AppKit loop or postpone restoring termios; partial writes advance the span.
static BOOL WriteAll(int fd, const char *bytes, size_t length) {
    while (length) {
        ssize_t count = write(fd, bytes, length);
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

static BOOL QuietTTY(int fd, struct termios *saved) {
    if (tcgetattr(fd, saved) != 0) return NO;
    struct termios quiet = *saved;
    quiet.c_lflag &= ~(ECHO | ECHONL);
    return tcsetattr(fd, TCSANOW, &quiet) == 0;
}

static BOOL RestoreTTY(int fd, const struct termios *saved) {
    return tcsetattr(fd, TCSANOW, saved) == 0;
}

static BOOL MarkerFits(unsigned short columns, unsigned short rows) {
    return columns >= MarkerLength && rows >= 2;
}

static BOOL CanProbeLayout(BOOL requestedInFront, BOOL markerReady, BOOL menuOpen) {
    return requestedInFront && markerReady && !menuOpen;
}

static BOOL LayoutDeadlineElapsed(NSTimeInterval now, NSTimeInterval *since, BOOL canProbe) {
    if (!canProbe) {
        *since = 0;
        return NO;
    }
    if (!*since) *since = now;
    return now - *since >= LayoutProofTimeout;
}

static BOOL ProofFresh(NSTimeInterval now, NSTimeInterval started) {
    return started > 0 && now >= started && now - started < ProofLifetime;
}

static BOOL CreditVisiblePanel(NSTimeInterval now, NSTimeInterval started, BOOL visible, BOOL *everShown) {
    BOOL provedVisible = visible && ProofFresh(now, started);
    *everShown = *everShown || provedVisible;
    return provedVisible;
}

static void UpdateMenuState(CFStringRef notification, BOOL *menuOpen, NSTimeInterval *unprovenSince) {
    if (CFEqual(notification, kAXMenuOpenedNotification)) {
        *menuOpen = YES;
        *unprovenSince = 0;
    } else if (CFEqual(notification, kAXMenuClosedNotification)) {
        *menuOpen = NO;
    }
}

static BOOL RestoreMarkerTerminal(int fd, BOOL alternateScreen, const struct termios *saved) {
    // Restore modes even if there is no space for the alternate-screen escape.
    BOOL restored = !saved || RestoreTTY(fd, saved);
    if (alternateScreen) {
        static const char restore[] = "\033[?25h\033[?1049l";
        restored = WriteAll(fd, restore, sizeof(restore) - 1) && restored;
    }
    return restored;
}

@interface MiloPreviewAnchor : NSObject <NSWindowDelegate> {
    NSURL *_url;
    NSString *_marker;
    int _stateFD, _tty;
    pid_t _parent;
    NSRunningApplication *_rex;
    AXUIElementRef _axApp;
    AXObserverRef _axObserver;
    BOOL _menuOpen;
    MiloPreviewPanel *_panel;
    NSTimer *_timer;
    NSMutableArray *_notificationTokens;
    id _keyMonitor;
    dispatch_queue_t _probeQueue;
    dispatch_source_t _stateSource, _parentSource;
    NSMutableArray<dispatch_source_t> *_signalSources;
    Geometry _candidate;
    MiloAXProof *_cached;
    NSRect _candidateFrame;
    CGDirectDisplayID _candidateDisplay;
    NSTimeInterval _lastProof, _lastFullAttempt, _lastMarkerPaint, _unprovenSince;
    NSUInteger _generation;
    BOOL _probePending, _requested, _finished, _alternateScreen;
    BOOL _everShown, _sessionInactive, _screensAsleep, _floating;
    struct termios _savedTTY;
    BOOL _ttyQuiet;
    unsigned short _rows, _columns;
    char _line[32];
    size_t _lineLength;
    int _status;
}
- (instancetype)initWithURL:(NSURL *)url marker:(NSString *)marker stateFD:(int)stateFD parent:(pid_t)parent tty:(int)tty;
- (int)run;
- (void)receivedAXNotification:(CFStringRef)notification;
@end

static void AXChanged(AXObserverRef observer, AXUIElementRef element, CFStringRef notification, void *context) {
    (void)observer;
    (void)element;
    [(__bridge MiloPreviewAnchor *)context receivedAXNotification:notification];
}

@implementation MiloPreviewAnchor
- (instancetype)initWithURL:(NSURL *)url marker:(NSString *)marker stateFD:(int)stateFD parent:(pid_t)parent tty:(int)tty {
    if ((self = [super init])) {
        _url = url;
        _marker = marker;
        _stateFD = stateFD;
        _parent = parent;
        _tty = tty;
        _probeQueue = dispatch_queue_create("milo.preview.ax", DISPATCH_QUEUE_SERIAL);
        _notificationTokens = [NSMutableArray array];
        _signalSources = [NSMutableArray array];
    }
    return self;
}

- (void)conceal {
    _candidate = (Geometry){0};
    _lastProof = 0;
    if (_panel.visible) [_panel orderOut:nil];
}

- (void)invalidate {
    ++_generation;
    _cached = nil;
    [self conceal];
}

- (void)finish:(const char *)reason {
    if (_finished) return;
    _finished = YES;
    [self invalidate];
    BOOL restored = RestoreMarkerTerminal(_tty, _alternateScreen, _ttyQuiet ? &_savedTTY : NULL);
    _alternateScreen = NO;
    _ttyQuiet = NO;
    if (reason || !restored) _status = 1;
    if (reason && restored) WriteDiagnostic(_tty, reason);
    [NSApp stop:nil];
    // An AppKit run started by a command-line process needs an event to observe -stop:.
    NSEvent *wake = [NSEvent otherEventWithType:NSEventTypeApplicationDefined location:NSZeroPoint modifierFlags:0
        timestamp:0 windowNumber:0 context:nil subtype:0 data1:0 data2:0];
    [NSApp postEvent:wake atStart:YES];
}

- (BOOL)drawMarker {
    static const char clear[] = "\033[H\033[2J";
    static const char narrow[] = "Need 32 columns and two rows.";
    BOOL fits = MarkerFits(_columns, _rows);
    const char *text = fits ? _marker.UTF8String : narrow;
    size_t length = fits ? MarkerLength : MIN((size_t)_columns, sizeof(narrow) - 1);
    if (!WriteAll(_tty, clear, sizeof(clear) - 1) || !WriteAll(_tty, text, length)) {
        [self finish:"could not draw the Rex terminal marker"];
        return NO;
    }
    _lastMarkerPaint = NSProcessInfo.processInfo.systemUptime;
    return YES;
}

- (BOOL)updateMarker {
    struct winsize size;
    if (ioctl(_tty, TIOCGWINSZ, &size) != 0) {
        [self finish:"the Rex terminal is no longer available"];
        return NO;
    }
    if (!_alternateScreen || size.ws_col != _columns || size.ws_row != _rows) {
        [self invalidate];
        static const char alternate[] = "\033[?1049h\033[?25l";
        if (!_alternateScreen) {
            if (!QuietTTY(_tty, &_savedTTY)) {
                [self finish:"could not disable input echo in the preview terminal"];
                return NO;
            }
            _ttyQuiet = YES;
            _alternateScreen = YES;
            if (!WriteAll(_tty, alternate, sizeof(alternate) - 1)) {
                [self finish:"could not set up the Rex terminal marker"];
                return NO;
            }
        }
        _columns = size.ws_col;
        _rows = size.ws_row;
        if (![self drawMarker]) return NO;
    }
    return MarkerFits(_columns, _rows);
}

- (BOOL)frontmostRex {
    return _rex && !_rex.terminated && _rex.active &&
        NSWorkspace.sharedWorkspace.frontmostApplication.processIdentifier == _rex.processIdentifier;
}

- (void)receivedAXNotification:(CFStringRef)notification {
    UpdateMenuState(notification, &_menuOpen, &_unprovenSince);
    [self invalidate];
}

- (void)pinRex:(pid_t)pid {
    _rex = [NSRunningApplication runningApplicationWithProcessIdentifier:pid];
    if (!_rex || _rex.terminated || ![_rex.bundleIdentifier isEqualToString:@"com.superlogical.rex"]) {
        [self finish:"the Rex client PID does not identify a running local Rex application"];
        return;
    }
    _axApp = AXUIElementCreateApplication(pid);
    if (!_axApp || AXObserverCreate(pid, AXChanged, &_axObserver) != kAXErrorSuccess) {
        [self finish:"could not observe the Rex Accessibility window"];
        return;
    }
    CFRunLoopAddSource(CFRunLoopGetMain(), AXObserverGetRunLoopSource(_axObserver), kCFRunLoopCommonModes);
    const CFStringRef notifications[] = {kAXFocusedWindowChangedNotification, kAXFocusedUIElementChangedNotification,
        kAXLayoutChangedNotification, kAXMenuOpenedNotification, kAXMenuClosedNotification,
        kAXWindowMovedNotification, kAXWindowResizedNotification};
    for (size_t i = 0; i < sizeof(notifications) / sizeof(notifications[0]); ++i) {
        AXError status = AXObserverAddNotification(_axObserver, _axApp, notifications[i], (__bridge void *)self);
        if (i < 3 && status != kAXErrorSuccess && status != kAXErrorNotificationAlreadyRegistered) {
            [self finish:"Rex does not expose Accessibility focus/layout notifications needed to safely place the preview"];
            return;
        }
    }
}

- (void)handleLine {
    if (_lineLength == 4 && !memcmp(_line, "hide", 4)) {
        _requested = NO;
        _unprovenSince = 0;
        [self invalidate];
        return;
    }
    BOOL floating = _lineLength >= 12 && !memcmp(_line + _lineLength - 6, " float", 6);
    BOOL tiled = _lineLength >= 11 && !memcmp(_line + _lineLength - 5, " tile", 5);
    if ((!floating && !tiled) || memcmp(_line, "show ", 5)) {
        [self finish:"invalid Rex visibility message"];
        return;
    }
    unsigned long pid = 0;
    size_t pidEnd = _lineLength - (floating ? 6 : 5);
    for (size_t i = 5; i < pidEnd; ++i) {
        unsigned char byte = (unsigned char)_line[i];
        if (byte < '0' || byte > '9' || pid > ((unsigned long)INT_MAX - (byte - '0')) / 10) {
            [self finish:"invalid Rex client PID"];
            return;
        }
        pid = pid * 10 + (byte - '0');
    }
    if (pid < 2 || (_rex && _rex.processIdentifier != (pid_t)pid)) {
        [self finish:"the Rex client changed; reopen the preview in the current Rex client"];
        return;
    }
    if (!_rex) [self pinRex:(pid_t)pid];
    if (!_finished && (!_requested || _floating != floating)) {
        _requested = YES;
        _floating = floating;
        [self invalidate];
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
    if (!_panel.keyWindow || ![self frontmostRex] || NSProcessInfo.processInfo.systemUptime - _lastProof >= ProofLifetime) return;
    [self invalidate];
    static const char focus[] = "focus\n";
    if (!WriteAll(STDOUT_FILENO, focus, sizeof(focus) - 1)) [self finish:"could not return focus to the Rex terminal"];
}

- (void)applyProof:(MiloAXProof *)result generation:(NSUInteger)generation started:(NSTimeInterval)started
    completed:(NSTimeInterval)completed full:(BOOL)full redraw:(BOOL)redraw {
    _probePending = NO;
    // A stale or unsuccessful full attempt cannot start another walk immediately after it returns.
    if (full) _lastFullAttempt = completed;
    if (_finished || generation != _generation) return;
    NSTimeInterval now = NSProcessInfo.processInfo.systemUptime;
    Geometry proof = result ? result->geometry : (Geometry){0};
    NSRect frame;
    CGDirectDisplayID display;
    if (!_requested || _menuOpen || _sessionInactive || _screensAsleep || ![self frontmostRex]) {
        [self invalidate];
        return;
    }
    if (!proof.windowID || !ProofFresh(now, started) || !ScreenFrame(proof, &frame, &display) ||
        !ProofFresh(ProofClock(NULL), started)) {
        _cached = nil;
        [self conceal];
        if (redraw && now - started < ProofLifetime && now - _lastMarkerPaint >= LayoutProofInterval) [self drawMarker];
        return;
    }
    _cached = result;
    if (full) _lastFullAttempt = started;
    if (!SameGeometry(_candidate, proof) || display != _candidateDisplay ||
        !NSEqualRects(frame, _candidateFrame) || now - _lastProof >= ProofLifetime) {
        [self conceal];
        _candidate = proof;
        _candidateFrame = frame;
        _candidateDisplay = display;
        _lastProof = started;
        return;
    }
    _lastProof = started;
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
        _panel.delegate = self;
        _panel.contentView = browser;
        // WebKit startup can outlast a frame proof. Use a fresh sample before ordering the panel.
        return;
    }
    if (!_panel.visible) {
        if (![self frontmostRex]) {
            [self invalidate];
            return;
        }
        [_panel setFrame:frame display:NO];
        if (generation != _generation || ![self frontmostRex] || !ProofFresh(ProofClock(NULL), started)) {
            [self invalidate];
            if (full) _lastFullAttempt = completed;
            return;
        }
        // Relative ordering at the normal level, never orderFrontRegardless or app activation.
        [_panel orderWindow:NSWindowAbove relativeTo:(NSInteger)proof.windowID];
        if (_panel.visible && (!NSEqualRects(_panel.frame, frame) || !ProofFresh(ProofClock(NULL), started))) {
            [self invalidate];
            if (full) _lastFullAttempt = completed;
            return;
        }
    }
}

- (void)tick {
    if (_finished) return;
    if (getppid() != _parent) { [self finish:NULL]; return; }
    if (!AXIsProcessTrusted()) { [self finish:AccessibilityUnavailable]; return; }
    if (_rex.terminated) { [self finish:NULL]; return; }
    BOOL markerReady = [self updateMarker];
    if (_finished) return;
    NSTimeInterval now = NSProcessInfo.processInfo.systemUptime;
    BOOL inFront = _requested && !_sessionInactive && !_screensAsleep && [self frontmostRex];
    BOOL canProbe = CanProbeLayout(inFront, markerReady, _menuOpen);
    if (canProbe) {
        // A panel may have reached the active Space on the last fresh frame before the cutoff.
        BOOL visible = _panel.visible && _panel.onActiveSpace && (_panel.occlusionState & NSWindowOcclusionStateVisible);
        now = ProofClock(NULL);
        if (!CreditVisiblePanel(now, _lastProof, visible, &_everShown)) {
            if (_panel.visible) [self invalidate];
            else if (_lastProof && !ProofFresh(now, _lastProof)) [self conceal];
        }
    }
    if (!_everShown && LayoutDeadlineElapsed(now, &_unprovenSince, canProbe)) {
        [self finish:"could not verify the Rex pane's Accessibility geometry; close palettes, focus a Rex terminal, and use a window on one display outside full screen"];
        return;
    }
    if (!canProbe) {
        if (_candidate.windowID || _cached || _probePending) [self invalidate];
        return;
    }
    if (_probePending) return;
    MiloAXProof *cached = _cached;
    if (cached && now - cached->layoutCheckedAt >= LayoutProofInterval) cached = nil;
    if (!cached && now - _lastFullAttempt < LayoutProofInterval) return;
    _probePending = YES;
    NSUInteger generation = _generation;
    pid_t pid = _rex.processIdentifier;
    NSString *marker = _marker;
    BOOL floating = _floating;
    id axApp = (__bridge id)_axApp;
    __weak MiloPreviewAnchor *weakSelf = self;
    dispatch_async(_probeQueue, ^{
        @autoreleasepool {
            BOOL redraw;
            MiloAXProof *result = ProveGeometry((__bridge AXUIElementRef)axApp, pid, marker, floating, now, cached, &redraw);
            NSTimeInterval completed = ProofClock(NULL);
            dispatch_async(dispatch_get_main_queue(), ^{
                [weakSelf applyProof:result generation:generation started:now completed:completed full:cached == nil redraw:redraw];
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
    NSNotificationCenter *workspace = NSWorkspace.sharedWorkspace.notificationCenter;
    for (NSNotificationName name in @[NSWorkspaceDidActivateApplicationNotification, NSWorkspaceDidDeactivateApplicationNotification,
        NSWorkspaceDidTerminateApplicationNotification, NSWorkspaceActiveSpaceDidChangeNotification,
        NSWorkspaceSessionDidResignActiveNotification, NSWorkspaceSessionDidBecomeActiveNotification,
        NSWorkspaceScreensDidSleepNotification, NSWorkspaceScreensDidWakeNotification]) [self observe:name center:workspace];
    [self observe:NSApplicationDidChangeScreenParametersNotification center:NSNotificationCenter.defaultCenter];

    _stateSource = dispatch_source_create(DISPATCH_SOURCE_TYPE_READ, (uintptr_t)_stateFD, 0, dispatch_get_main_queue());
    _parentSource = dispatch_source_create(DISPATCH_SOURCE_TYPE_PROC, (uintptr_t)_parent, DISPATCH_PROC_EXIT, dispatch_get_main_queue());
    if (!_stateSource || !_parentSource) {
        [self finish:"could not observe the Rex preview lifecycle"];
        return _status;
    }
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
            [self finish:"could not observe Rex terminal close signals"];
            break;
        }
        dispatch_source_set_event_handler(source, ^{ [weakSelf finish:NULL]; });
        [_signalSources addObject:source];
        dispatch_resume(source);
    }
    NSEventMask guardedInput = NSEventMaskKeyDown | NSEventMaskLeftMouseDown | NSEventMaskRightMouseDown |
        NSEventMaskOtherMouseDown | NSEventMaskScrollWheel;
    _keyMonitor = [NSEvent addLocalMonitorForEventsMatchingMask:guardedInput handler:^NSEvent *(NSEvent *event) {
        MiloPreviewAnchor *anchor = weakSelf;
        if (!anchor || !anchor->_panel || event.window != anchor->_panel) return event;
        if (!anchor->_panel.visible || ![anchor frontmostRex] ||
            NSProcessInfo.processInfo.systemUptime - anchor->_lastProof >= ProofLifetime) {
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
    _timer = [NSTimer timerWithTimeInterval:ProofInterval repeats:YES block:^(NSTimer *timer) {
        (void)timer;
        [weakSelf tick];
    }];
    [NSRunLoop.mainRunLoop addTimer:_timer forMode:NSRunLoopCommonModes];
    [self tick];
    if (!_finished) [NSApp run];
    [self finish:NULL];

    [_timer invalidate];
    if (_keyMonitor) [NSEvent removeMonitor:_keyMonitor];
    for (NSArray *pair in _notificationTokens) [pair[0] removeObserver:pair[1]];
    if (_axObserver) {
        CFRunLoopRemoveSource(CFRunLoopGetMain(), AXObserverGetRunLoopSource(_axObserver), kCFRunLoopCommonModes);
        CFRelease(_axObserver);
    }
    if (_axApp) CFRelease(_axApp);
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

int milo_preview_run(const char *url, const char *marker, int state_fd) {
    @autoreleasepool {
        if (!NSThread.isMainThread || !url || !marker || state_fd < 0 || strlen(marker) != MarkerLength ||
            strspn(marker, "0123456789abcdef") != MarkerLength) {
            fprintf(stderr, "milo preview: invalid native anchor arguments\n");
            return 1;
        }
        NSString *address = [NSString stringWithUTF8String:url];
        NSURL *location = address ? [NSURL URLWithString:address] : nil;
        pid_t parent = getppid();
        if (!location || parent < 2 || fcntl(state_fd, F_GETFD) < 0) {
            fprintf(stderr, "milo preview: invalid URL or missing Rex watcher\n");
            return 1;
        }
        int tty = OpenTTYOutput("/dev/tty");
        if (tty < 0) {
            fprintf(stderr, "milo preview: no controlling Rex terminal\n");
            return 1;
        }
        if (!AXIsProcessTrusted()) {
            WriteDiagnostic(tty, AccessibilityUnavailable);
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
        AXUIElementRef allApps = AXUIElementCreateSystemWide();
        AXError timeout = allApps ? AXUIElementSetMessagingTimeout(allApps, AXMessageTimeout) : kAXErrorFailure;
        if (allApps) CFRelease(allApps);
        if (timeout != kAXErrorSuccess) {
            WriteDiagnostic(tty, "could not bound Accessibility geometry requests");
            close(tty);
            return 1;
        }
        MiloPreviewAnchor *anchor = [[MiloPreviewAnchor alloc] initWithURL:location
            marker:[NSString stringWithUTF8String:marker] stateFD:state_fd parent:parent tty:tty];
        int status = [anchor run];
        close(tty);
        return status;
    }
}
