// Compiled only by smoke.sh. Include the implementation to test its private pure policy helpers.
#import <objc/runtime.h>
#import "../Browser.m"

#define CHECK(condition) do { if (!(condition)) { \
    fprintf(stderr, "Browser smoke failed at line %d: %s\n", __LINE__, #condition); exit(1); \
} } while (0)

static void CheckPolicies(void) {
    CHECK(PreviewOrigin(nil, NO) == nil);
    CHECK([PreviewOrigin([NSURL URLWithString:@"HTTP://LOCALHOST:80/a"], YES) isEqual:@"http://localhost"]);
    CHECK([PreviewOrigin([NSURL URLWithString:@"https://[::1]:5443/a"], YES) isEqual:@"https://[::1]:5443"]);
    CHECK([PreviewOrigin([NSURL URLWithString:@"http://127.0.0.1:3000/a?query#fragment"], NO) isEqual:@"http://127.0.0.1:3000"]);
    for (NSString *url in @[@"http://localhost:0/", @"http://localhost:65536/", @"http://user@localhost/", @"http://localhost@elsewhere/",
                             @"http://localhost/a?q", @"http://localhost/#f", @"http://127.1/", @"http://127.0.0.2/",
                             @"http://%6cocalhost/", @"http://localhost.example/", @"http://[::2]/", @"http://localhost./",
                             @"file:///tmp/file", @"ws://localhost/", @"mailto:local@example.com"]) {
        CHECK(PreviewOrigin([NSURL URLWithString:url], YES) == nil);
    }
    CHECK((PreviewPath(@[@[@0, @2]]) && [PreviewPath(@[@[@0, @2]]) isEqual:@"*:nth-child(2)"]));
    NSArray *invalid = @[@"html", @[], @[@[@1, @0]], @[@[@900, @1]], @[@[@1, @4097]], @[@[@YES, @1]],
                         @[@[@1, @1.5]], @[@[@1, @"123"]], @[@[@1, @1, @"secret"]], @[@[@"custom-name", @1]]];
    for (id selection in invalid) CHECK(PreviewPath(selection) == nil);
    NSMutableArray *tooDeep = [NSMutableArray array];
    for (NSUInteger i = 0; i <= PreviewMaxDepth; ++i) [tooDeep addObject:@[@0, @1]];
    CHECK(PreviewPath(tooDeep) == nil);
}

static BOOL WaitFor(BOOL (^condition)(void)) {
    NSTimeInterval end = NSProcessInfo.processInfo.systemUptime + 10;
    do {
        if (condition()) return YES;
        [NSRunLoop.currentRunLoop runUntilDate:[NSDate dateWithTimeIntervalSinceNow:0.025]];
    } while (NSProcessInfo.processInfo.systemUptime < end);
    return NO;
}

static id Evaluate(WKWebView *web, NSString *script) {
    __block BOOL completed = NO;
    __block id value;
    [web evaluateJavaScript:script completionHandler:^(id result, NSError *error) {
        value = result;
        completed = YES;
    }];
    CHECK(WaitFor(^BOOL{ return completed; }));
    return value;
}

static void Click(NSView *view, CGFloat x, CGFloat y) {
    [view.window.contentView layoutSubtreeIfNeeded];
    NSWindow *window = view.window;
    NSPoint point = [view convertPoint:NSMakePoint(x, view.isFlipped ? y : NSHeight(view.bounds) - y) toView:nil];
    NSEvent *(^mouse)(NSEventType) = ^NSEvent *(NSEventType type) {
        return [NSEvent mouseEventWithType:type location:point modifierFlags:0 timestamp:NSProcessInfo.processInfo.systemUptime
                             windowNumber:window.windowNumber context:nil eventNumber:0 clickCount:1 pressure:1];
    };
    [window sendEvent:mouse(NSEventTypeLeftMouseDown)];
    [window sendEvent:mouse(NSEventTypeLeftMouseUp)];
}

static NSPasteboard *TestBoard;

static NSPasteboard *TestGeneralPasteboard(id receiver, SEL selector) {
    return TestBoard;
}

@interface SmokePanel : NSPanel
@end

@implementation SmokePanel
- (BOOL)canBecomeKeyWindow { return YES; }
- (BOOL)canBecomeMainWindow { return NO; }
@end

static void DoSmoke(NSURL *url, NSString *hmrFile, NSString *escapedRequest) {
    // Do not read, clear or write the user's pasteboard. Only this test process
    // resolves generalPasteboard to a newly named, disposable pasteboard.
    TestBoard = NSPasteboard.pasteboardWithUniqueName;
    CHECK([TestBoard setString:@"smoke sentinel" forType:NSPasteboardTypeString]);
    Method method = class_getClassMethod(NSPasteboard.class, @selector(generalPasteboard));
    IMP original = method_setImplementation(method, (IMP)TestGeneralPasteboard);

    __block BOOL returned = NO;
    NSView *content = MiloPreviewContent(url, ^{ returned = YES; });
    CHECK(content != nil);
    NSPanel *window = [[SmokePanel alloc] initWithContentRect:NSMakeRect(60, 60, 430, 410)
                                                   styleMask:NSWindowStyleMaskTitled | NSWindowStyleMaskClosable |
                                                             NSWindowStyleMaskNonactivatingPanel
                                                     backing:NSBackingStoreBuffered defer:NO];
    window.title = @"Milo WebKit Smoke Test";
    window.releasedWhenClosed = NO;
    window.hidesOnDeactivate = NO;
    window.becomesKeyOnlyIfNeeded = NO;
    window.contentView = content;
    [window makeKeyAndOrderFront:nil];
    CHECK(WaitFor(^BOOL{ return window.isKeyWindow; }));

    // Resolve native controls just as a test would by accessibility names. No
    // Browser.m test hooks, message injection, or synthetic page-side clicks.
    WKWebView *web = nil;
    NSTextView *review = nil;
    NSButton *select = nil, *copy = nil, *cancel = nil, *back = nil, *reload = nil, *terminal = nil;
    for (NSView *view in content.subviews) {
        if ([view isKindOfClass:WKWebView.class]) web = (WKWebView *)view;
        if ([view isKindOfClass:NSScrollView.class]) review = ((NSScrollView *)view).documentView;
        if (![view isKindOfClass:NSButton.class]) continue;
        NSButton *button = (NSButton *)view;
        NSString *name = [button accessibilityLabel] ?: button.title;
        if ([name isEqual:@"Select an element"]) select = button;
        if ([name isEqual:@"Copy path"]) copy = button;
        if ([name isEqual:@"Cancel"]) cancel = button;
        if ([name isEqual:@"Go back"]) back = button;
        if ([name isEqual:@"Reload preview"]) reload = button;
        if ([name isEqual:@"Return to terminal"]) terminal = button;
    }
    CHECK(web && review && select && copy && cancel && back && reload && terminal);
    CHECK(!web.configuration.websiteDataStore.isPersistent);
    CHECK(WaitFor(^BOOL{ return select.enabled && !web.isLoading; }));
    CHECK([Evaluate(web, @"document.title") isEqual:@"Milo fixture"]);
    CHECK(WaitFor(^BOOL{ return [Evaluate(web, @"document.querySelector('#hmr').textContent") isEqual:@"ready"]; }));
    CHECK([@"updated" writeToFile:hmrFile atomically:YES encoding:NSUTF8StringEncoding error:NULL]);
    CHECK(WaitFor(^BOOL{ return [Evaluate(web, @"document.querySelector('#hmr').textContent") isEqual:@"updated"]; }));

    // Real native click and key events to a WebKit form.
    Click(web, 80, 23);
    CHECK(WaitFor(^BOOL{ return [Evaluate(web, @"document.activeElement.id") isEqual:@"typed"]; }));
    NSEvent *key = [NSEvent keyEventWithType:NSEventTypeKeyDown location:NSZeroPoint modifierFlags:0
                                  timestamp:NSProcessInfo.processInfo.systemUptime windowNumber:window.windowNumber context:nil
                                 characters:@"x" charactersIgnoringModifiers:@"x" isARepeat:NO keyCode:7];
    [window sendEvent:key];
    CHECK(WaitFor(^BOOL{ return [Evaluate(web, @"document.querySelector('#typed').value") isEqual:@"x"]; }));
    Click(web, 171, 23);
    CHECK(WaitFor(^BOOL{ return [web.URL.path isEqual:@"/form"] && !web.isLoading; }));
    CHECK([Evaluate(web, @"document.title") isEqual:@"Form passed"]);
    [back performClick:nil];
    CHECK(WaitFor(^BOOL{ return [web.URL.path isEqual:@"/start"] && select.enabled && !web.isLoading; }));

    // Native element pick selects the custom element containing planted secrets.
    // Verify no page strings escape, cancel does not copy, and explicit copy does.
    for (int i = 0; i < 3; ++i) {
        [select performClick:nil];
        CHECK(WaitFor(^BOOL{ return [window.firstResponder isKindOfClass:MiloPickerSurface.class]; }));
        Click((NSView *)window.firstResponder, 70, 67);
        CHECK(WaitFor(^BOOL{ return !copy.isHidden && copy.enabled; }));
        NSString *path = @"html:nth-child(1) > body:nth-child(2) > *:nth-child(2)";
        CHECK([review.string isEqual:path]);
        CHECK([[TestBoard stringForType:NSPasteboardTypeString] isEqual:@"smoke sentinel"]);
        if (i == 0) {
            [cancel performClick:nil];
            CHECK(copy.isHidden);
        } else if (i == 1) {
            [reload performClick:nil];
            CHECK(copy.isHidden);
            CHECK(WaitFor(^BOOL{ return select.enabled && !web.isLoading; }));
        } else {
            [copy performClick:nil];
            CHECK(WaitFor(^BOOL{ return copy.isHidden && [[TestBoard stringForType:NSPasteboardTypeString] isEqual:path]; }));
        }
    }

    Click(web, 50, 107); // a target=_blank link to the unapproved port
    CHECK(WaitFor(^BOOL {
        for (NSView *view in content.subviews) {
            if ([view isKindOfClass:NSTextField.class] &&
                [((NSTextField *)view).stringValue hasPrefix:@"Blocked a navigation"]) return YES;
        }
        return NO;
    }));
    Click(web, 50, 145); // same-origin path that redirects to that port
    CHECK(WaitFor(^BOOL {
        return [NSFileManager.defaultManager fileExistsAtPath:[escapedRequest stringByAppendingString:@".redirect"]];
    }));
    CHECK(WaitFor(^BOOL{ return !web.isLoading; }));
    CHECK([Evaluate(web, @"document.title") isEqual:@"Milo fixture"]);
    CHECK(![NSFileManager.defaultManager fileExistsAtPath:escapedRequest]);
    [terminal performClick:nil];
    CHECK(returned);
    [window orderOut:nil];
    window.contentView = nil;
    [window close];
    method_setImplementation(method, original);
    [TestBoard releaseGlobally];
    TestBoard = nil;
}

int main(int argc, char **argv) {
    @autoreleasepool {
        CHECK(argc == 4);
        CheckPolicies();
        [NSApplication.sharedApplication setActivationPolicy:NSApplicationActivationPolicyAccessory];
        [NSApp finishLaunching];
        DoSmoke([NSURL URLWithString:@(argv[1])], @(argv[2]), @(argv[3]));
        puts("Native WebKit navigation, form, websocket hot reload, element picker and origin policy passed.");
    }
}
