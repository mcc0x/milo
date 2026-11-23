#ifndef MILO_PREVIEW_CHROME_H
#define MILO_PREVIEW_CHROME_H

#import <AppKit/AppKit.h>
#import <float.h>
#import <math.h>

typedef struct { CGFloat low, high; } MiloChromeInset;
typedef struct {
    MiloChromeInset left, top, right, bottom;
    CGFloat header, dividerAllowance;
} MiloRexChrome;

static BOOL MiloChromeNumber(id value, CGFloat fallback, CGFloat lo, CGFloat hi, CGFloat *result) {
    if (!value) {
        *result = fallback;
        return YES;
    }
    if (CFGetTypeID((__bridge CFTypeRef)value) != CFNumberGetTypeID()) return NO;
    double number = [value doubleValue];
    if (!isfinite(number) || number < lo || number > hi) return NO;
    *result = round(number);
    return YES;
}

static BOOL MiloChromeBoolean(id value, BOOL fallback, BOOL *result) {
    if (!value) {
        *result = fallback;
        return YES;
    }
    if (CFGetTypeID((__bridge CFTypeRef)value) != CFBooleanGetTypeID()) return NO;
    *result = [value boolValue];
    return YES;
}

// The official Rex Mac Settings reference documents what each control does. The
// defaults, persisted spellings and numeric ranges below come from the installed
// Rex Beta build 2061, not the visible labels in Settings.
static BOOL MiloDecodeRexChrome(NSDictionary *prefs, MiloRexChrome *result) {
    id tabs = prefs[@"Workspace.tabStyle"] ?: @"horizontal";
    id toolbar = prefs[@"Workspace.toolbarBehavior"] ?: @"alwaysShow";
    id header = prefs[@"Workspace.paneHeaderVisibility"];
    id density = prefs[@"Workspace.paneDensity"] ?: @"comfortable";
    id style = prefs[@"Workspace.interfaceStyle"];
    if (style && ![@"modern" isEqual:style] && ![@"system" isEqual:style] &&
        ![@"useTheme" isEqual:style] && ![@"blendTheme" isEqual:style]) return NO;
    BOOL toolbarAlways = [@"alwaysShow" isEqual:toolbar];
    if (!toolbarAlways && ![@"onHover" isEqual:toolbar] && ![@"hidden" isEqual:toolbar]) return NO;
    if (!header) {
        BOOL legacyShows;
        if (!MiloChromeBoolean(prefs[@"Workspace.showsPaneHeaders"], YES, &legacyShows)) return NO;
        header = legacyShows ? @"always" : @"never";
    }
    BOOL hasHeader = [@"always" isEqual:header] || [@"onHover" isEqual:header];
    if (!hasHeader && ![@"never" isEqual:header]) return NO;
    BOOL compact = [@"compact" isEqual:density];
    if (!compact && ![@"comfortable" isEqual:density]) return NO;

    // Only the preferences (including the saved sidebar width) are known. The
    // point ranges for surrounds, controls, and divided panes are estimates,
    // not measurements of a live window; per-window chrome can still differ.
    CGFloat surround = compact ? 4 : 10;
    MiloRexChrome chrome = {
        .left = {0, surround}, .top = {0, surround},
        .right = {0, surround}, .bottom = {0, surround},
        .header = hasHeader ? (compact ? 32 : 36) : 0,
        .dividerAllowance = compact ? 4 : 10,
    };

    if ([@"vertical" isEqual:tabs]) {
        id side = prefs[@"Workspace.sidebarPosition"] ?: @"left";
        BOOL left = [@"left" isEqual:side];
        CGFloat width;
        if ((!left && ![@"right" isEqual:side]) ||
            !MiloChromeNumber(prefs[@"Workspace.sidebarWidth"], 240, 167, 480, &width)) return NO;
        MiloChromeInset *inset = left ? &chrome.left : &chrome.right;
        // Saved preferences do not say whether this window's sidebar transition
        // finished. Keep only tile space common to expanded and collapsed layouts,
        // even when toggling normally changes the outer window width.
        *inset = (MiloChromeInset){0, width + surround + 4};
        chrome.top.high += 30;
    } else if ([@"horizontal" isEqual:tabs]) {
        id edge = prefs[@"Workspace.tabBarEdge"] ?: @"top";
        BOOL above = [@"top" isEqual:edge];
        if (!above && ![@"bottom" isEqual:edge]) return NO;
        MiloChromeInset *inset = above ? &chrome.top : &chrome.bottom;
        // On-hover/reveal state isn't available. Keep only the part of the
        // tile that belongs to the pane with the toolbar both shown and hidden.
        inset->low += toolbarAlways ? 32 : 0;
        inset->high += 58;
        if (!above) chrome.top.high += 30;
    } else {
        return NO;
    }
    *result = chrome;
    return YES;
}

static BOOL MiloLoadRexChrome(MiloRexChrome *chrome) {
    CFStringRef domain = CFSTR("com.superlogical.rex");
    // Reload the preferences domain. This process never stages a preference
    // change, so synchronization has nothing to write back to Rex.
    if (!CFPreferencesSynchronize(domain, kCFPreferencesCurrentUser, kCFPreferencesAnyHost)) return NO;
    NSArray *keys = @[@"Workspace.tabStyle", @"Workspace.tabBarEdge", @"Workspace.sidebarPosition",
                      @"Workspace.sidebarWidth", @"Workspace.toolbarBehavior",
                      @"Workspace.paneHeaderVisibility", @"Workspace.showsPaneHeaders", @"Workspace.paneDensity",
                      @"Workspace.interfaceStyle"];
    NSDictionary *prefs = CFBridgingRelease(CFPreferencesCopyMultiple((__bridge CFArrayRef)keys, domain,
                                                     kCFPreferencesCurrentUser, kCFPreferencesAnyHost));
    return prefs && MiloDecodeRexChrome(prefs, chrome);
}

static BOOL MiloChromePositiveRect(CGRect rect) {
    CGFloat right = rect.origin.x + rect.size.width, bottom = rect.origin.y + rect.size.height;
    return !CGRectIsNull(rect) && !CGRectIsInfinite(rect) &&
           isfinite(rect.origin.x) && isfinite(rect.origin.y) && isfinite(right) && isfinite(bottom) &&
           right > rect.origin.x && bottom > rect.origin.y;
}

// Maximum plausible near edge and minimum plausible far edge, so shrinking
// the estimated workspace cannot move an internal tile edge into its neighbor.
static CGFloat MiloChromeEdge(CGFloat span, CGFloat fraction, CGFloat near, CGFloat far) {
    return near * (1 - fraction) + (span - far) * fraction;
}

static BOOL MiloPaneInRexChrome(CGRect window, CGRect normalized, const MiloRexChrome *chrome, CGRect *pane) {
    if (!pane || !MiloChromePositiveRect(window) || !MiloChromePositiveRect(normalized) ||
        normalized.origin.x < 0 || normalized.origin.y < 0) return NO;
    CGFloat x = normalized.origin.x, y = normalized.origin.y;
    CGFloat right = x + normalized.size.width, bottom = y + normalized.size.height;
    // Adding edges decoded from integer millionths can overshoot one by an ulp.
    if (right > 1 + 8 * DBL_EPSILON || bottom > 1 + 8 * DBL_EPSILON) return NO;
    right = fmin(right, 1);
    bottom = fmin(bottom, 1);
    CGFloat w = window.size.width, h = window.size.height;
    if (w <= chrome->left.high + chrome->right.high || h <= chrome->top.high + chrome->bottom.high) return NO;
    // Terminal text padding is inside the Rex pane body, not window chrome.
    // Fill that space too, retaining the surrounding and divider clearances
    // and a small edge guard for unmeasured pane borders.
    const CGFloat edgeGuard = 2;
    CGFloat gap = chrome->dividerAllowance;
    CGFloat minX = MiloChromeEdge(w, x, chrome->left.high, chrome->right.low) + edgeGuard + (x > 0 ? gap : 0);
    CGFloat maxX = MiloChromeEdge(w, right, chrome->left.low, chrome->right.high) - edgeGuard - (right < 1 ? gap : 0);
    CGFloat minY = MiloChromeEdge(h, y, chrome->top.high, chrome->bottom.low) + edgeGuard + chrome->header + (y > 0 ? gap : 0);
    CGFloat maxY = MiloChromeEdge(h, bottom, chrome->top.low, chrome->bottom.high) - edgeGuard - (bottom < 1 ? gap : 0);
    CGRect body = CGRectMake(window.origin.x + minX, window.origin.y + minY, maxX - minX, maxY - minY);
    if (!MiloChromePositiveRect(body) || !CGRectContainsRect(window, body)) return NO;
    *pane = body;
    return YES;
}

// Call on one serial sampling queue. Both inputs and output use a top-left
// origin: window and pane are Quartz points; normalized is a unit split-tree rect.
static inline BOOL MiloApproximatePane(CGRect window, CGRect normalized, CGRect *pane) {
    static NSTimeInterval expires;
    static MiloRexChrome chrome;
    static BOOL supported;
    NSTimeInterval now = NSProcessInfo.processInfo.systemUptime;
    if (now >= expires) {
        supported = MiloLoadRexChrome(&chrome);
        expires = now + 1;
    }
    return supported && MiloPaneInRexChrome(window, normalized, &chrome, pane);
}

#endif
