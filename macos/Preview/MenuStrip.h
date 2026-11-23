#pragma once

#import <AppKit/AppKit.h>
#import <CoreGraphics/CoreGraphics.h>
#import <Security/Security.h>

// Only on an otherwise obstructed initial pin, never on the sampling timer.
// Window level, shape, bundle name and PID alone do not authenticate system UI.
static inline BOOL MiloSystemMenuOwner(pid_t owner) {
    SecCodeRef guest = NULL;
    NSDictionary *attrs = @{(__bridge NSString *)kSecGuestAttributePid: @(owner)};
    if (SecCodeCopyGuestWithAttributes(NULL, (__bridge CFDictionaryRef)attrs, kSecCSDefaultFlags, &guest) != errSecSuccess)
        return NO;
    SecRequirementRef requirement = NULL;
    BOOL verified = SecRequirementCreateWithString(CFSTR("anchor apple and identifier \"com.apple.WindowServer\""),
        kSecCSDefaultFlags, &requirement) == errSecSuccess &&
        SecCodeCheckValidity(guest, kSecCSDefaultFlags, requirement) == errSecSuccess;
    if (requirement) CFRelease(requirement);
    CFRelease(guest);
    return verified;
}

// Diagnostic only. A different helper process is an obstruction, not an exception.
static inline BOOL MiloPreviewProcess(pid_t owner) {
    NSRunningApplication *app = [NSRunningApplication runningApplicationWithProcessIdentifier:owner];
    return [app.executableURL.lastPathComponent isEqualToString:@"Milo Preview"];
}

static inline BOOL MiloMenuStripOnDisplay(CGRect strip, CGRect window, CGRect drawable, CGRect screen) {
    // A fixed upper bound deliberately refuses unfamiliar menu-bar geometry.
    return strip.size.height > 0 && strip.size.height <= 64 &&
        CGRectContainsRect(screen, window) && CGRectGetMinY(strip) == CGRectGetMinY(screen) &&
        CGRectGetMinX(strip) == CGRectGetMinX(screen) && CGRectGetWidth(strip) == CGRectGetWidth(screen) &&
        CGRectGetMinY(strip) <= CGRectGetMinY(window) && CGRectGetMaxY(strip) > CGRectGetMinY(window) &&
        CGRectIsEmpty(CGRectIntersection(strip, drawable));
}
