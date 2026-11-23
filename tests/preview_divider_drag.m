// Exercise AppKit's split responder without posting system events or activating a window.
// No args: report endpoints. --live: stream measured right-hand fractions during the drag.
#import <AppKit/AppKit.h>
#import <unistd.h>

int main(int argc, char **argv) {
    @autoreleasepool {
        BOOL live = argc == 2 && !strcmp(argv[1], "--live");
        if (argc != 1 && !live) return 2;
        [NSApplication sharedApplication];
        [NSApp setActivationPolicy:NSApplicationActivationPolicyProhibited];
        NSRect frame = NSMakeRect(-32000, -32000, 800, 480);
        NSPanel *host = [[NSPanel alloc] initWithContentRect:frame
            styleMask:NSWindowStyleMaskBorderless | NSWindowStyleMaskNonactivatingPanel
            backing:NSBackingStoreBuffered defer:NO];
        NSSplitView *split = [[NSSplitView alloc] initWithFrame:host.contentView.bounds];
        split.vertical = YES;
        split.dividerStyle = NSSplitViewDividerStyleThin;
        for (int i = 0; i < 2; i++) {
            [split addSubview:[[NSView alloc] initWithFrame:NSMakeRect(0, 0, 400, 480)]];
        }
        host.contentView = split;
        [split adjustSubviews];
        [split setPosition:480 ofDividerAtIndex:0];
        NSView *right = split.subviews.lastObject;
        NSRect oldPage = right.frame;
        NSPoint start = NSMakePoint(NSMinX(oldPage) - split.dividerThickness / 2, NSMidY(split.bounds));
        NSPoint end = NSMakePoint(start.x - 96, start.y);
        if (NSPointInRect(start, oldPage) || NSPointInRect(end, oldPage) || [split hitTest:start] != split) return 2;
        NSEvent *(^event)(NSEventType, NSPoint, NSInteger) = ^(NSEventType type, NSPoint p, NSInteger n) {
            return [NSEvent mouseEventWithType:type location:[split convertPoint:p toView:nil]
                modifierFlags:0 timestamp:NSProcessInfo.processInfo.systemUptime + n * .001
                windowNumber:host.windowNumber context:nil eventNumber:n clickCount:1 pressure:1];
        };
        // NSSplitView tracks dragged/up events from its application's private event queue.
        // Both endpoints are outside the old page. Nothing is posted to the HID event stream.
        CGFloat space = split.bounds.size.width - split.dividerThickness;
        id resizedObserver = nil;
        alarm(live ? 20 : 5);
        if (live) {
            resizedObserver = [NSNotificationCenter.defaultCenter addObserverForName:NSSplitViewDidResizeSubviewsNotification
                object:split queue:nil usingBlock:^(NSNotification *note) {
                    (void)note;
                    printf("%.6f\n", right.frame.size.width / space);
                    fflush(stdout);
                }];
            // Begin moving, hold while the sibling is used, then keep moving
            // faster than the geometry sampler until the final release.
            const NSUInteger last = 19;
            for (NSUInteger i = 0; i <= last; ++i) {
                BOOL up = i == last;
                CGFloat offset = i < 3 ? 24 * (i + 1) : i >= last - 1 ? 96 : 73 + i - 3;
                NSTimeInterval delay = i < 3 ? 0.1 + i * 0.15 : 11.6 + (i - 3) * 0.12;
                NSPoint p = NSMakePoint(start.x - offset, start.y);
                NSEvent *next = event(up ? NSEventTypeLeftMouseUp : NSEventTypeLeftMouseDragged, p, i + 2);
                NSTimer *timer = [NSTimer timerWithTimeInterval:delay repeats:NO block:^(NSTimer *t) {
                    (void)t;
                    [NSApp postEvent:next atStart:NO];
                }];
                [NSRunLoop.currentRunLoop addTimer:timer forMode:NSEventTrackingRunLoopMode];
            }
        } else {
            [NSApp postEvent:event(NSEventTypeLeftMouseDragged, end, 2) atStart:NO];
            [NSApp postEvent:event(NSEventTypeLeftMouseUp, end, 3) atStart:NO];
        }
        [split mouseDown:event(NSEventTypeLeftMouseDown, start, 1)];
        if (resizedObserver) [NSNotificationCenter.defaultCenter removeObserver:resizedObserver];
        NSRect resized = right.frame;
        if (fabs(NSMinX(resized) - NSMinX(oldPage) + 96) > 1) {
            fprintf(stderr, "AppKit divider did not follow the drag: %.1f -> %.1f\n", NSMinX(oldPage), NSMinX(resized));
            return 3;
        }
        if (!live) printf("%.6f %.6f\n", oldPage.size.width / space, resized.size.width / space);
        return 0;
    }
}
