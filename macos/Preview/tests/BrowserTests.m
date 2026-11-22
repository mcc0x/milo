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
    NSData *nonRFC = [@"http://127.0.0.1:3000/a?filter[status]=open|{ready}#x^y" dataUsingEncoding:NSUTF8StringEncoding];
    NSURL *unusual = [NSURL URLWithDataRepresentation:nonRFC relativeToURL:nil];
    CHECK([PreviewOrigin(unusual, NO) isEqual:@"http://127.0.0.1:3000"]);
    CHECK(PreviewOrigin(unusual, YES) == nil);
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

static NSEvent *KeyEvent(NSWindow *window, NSString *key, unsigned short code, NSEventModifierFlags flags) {
    return [NSEvent keyEventWithType:NSEventTypeKeyDown location:NSZeroPoint modifierFlags:flags
                           timestamp:NSProcessInfo.processInfo.systemUptime windowNumber:window.windowNumber context:nil
                          characters:key charactersIgnoringModifiers:key isARepeat:NO keyCode:code];
}

static void Key(NSWindow *window, NSString *key, unsigned short code, NSEventModifierFlags flags) {
    [window sendEvent:KeyEvent(window, key, code, flags)];
}

static void CheckShortcuts(void) {
    NSEventModifierFlags shortcut = NSEventModifierFlagCommand | NSEventModifierFlagShift;
    CHECK(PreviewShortcutAction(KeyEvent(nil, @"\r", 36, shortcut)) == @selector(returnToTerminal:));
    CHECK(PreviewShortcutAction(KeyEvent(nil, @"\r", 36, 0)) == NULL);
    CHECK(PreviewShortcutAction(KeyEvent(nil, @"\r", 36, NSEventModifierFlagCommand)) == NULL);
    CHECK(PreviewShortcutAction(KeyEvent(nil, @"\r", 36, NSEventModifierFlagShift)) == NULL);
    CHECK(PreviewShortcutAction(KeyEvent(nil, @"\r", 36, shortcut | NSEventModifierFlagOption)) == NULL);
    CHECK(PreviewShortcutAction(KeyEvent(nil, @"\r", 36, shortcut | NSEventModifierFlagControl)) == NULL);
    CHECK(PreviewShortcutAction(KeyEvent(nil, @"\x03", 76, shortcut)) == NULL);
    CHECK(PreviewShortcutAction(KeyEvent(nil, @"T", 17, shortcut)) == NULL);
}

static NSPasteboard *TestBoard;
static NSPasteboard *(*OriginalNamedPasteboard)(id, SEL, NSPasteboardName);

static NSPasteboard *TestGeneralPasteboard(id receiver, SEL selector) {
    return TestBoard;
}

static NSPasteboard *TestNamedPasteboard(id receiver, SEL selector, NSPasteboardName name) {
    return [name isEqualToString:NSPasteboardNameGeneral] ? TestBoard : OriginalNamedPasteboard(receiver, selector, name);
}

static BOOL HasStatus(NSView *content, NSString *prefix) {
    for (NSView *view in content.subviews) {
        if ([view isKindOfClass:NSTextField.class] &&
            [((NSTextField *)view).stringValue hasPrefix:prefix]) return YES;
    }
    return NO;
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
    Method namedMethod = class_getClassMethod(NSPasteboard.class, @selector(pasteboardWithName:));
    OriginalNamedPasteboard = (NSPasteboard *(*)(id, SEL, NSPasteboardName))
        method_setImplementation(namedMethod, (IMP)TestNamedPasteboard);

    __block NSUInteger returns = 0;
    NSView *content = MiloPreviewContent(url, ^{ ++returns; });
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
    Key(window, @"x", 7, 0);
    CHECK(WaitFor(^BOOL{ return [Evaluate(web, @"document.querySelector('#typed').value") isEqual:@"x"]; }));
    Key(window, @"z", 6, NSEventModifierFlagCommand);
    CHECK(WaitFor(^BOOL{ return [Evaluate(web, @"typed.value") isEqual:@""]; }));
    Key(window, @"Z", 6, NSEventModifierFlagCommand | NSEventModifierFlagShift);
    CHECK(WaitFor(^BOOL{ return [Evaluate(web, @"typed.value") isEqual:@"x"]; }));
    Key(window, @"a", 0, NSEventModifierFlagCommand);
    CHECK(WaitFor(^BOOL{ return [Evaluate(web, @"typed.selectionEnd - typed.selectionStart") isEqual:@1]; }));
    Key(window, @"c", 8, NSEventModifierFlagCommand);
    CHECK(WaitFor(^BOOL{ return [[TestBoard stringForType:NSPasteboardTypeString] isEqual:@"x"]; }));
    Key(window, @"x", 7, NSEventModifierFlagCommand);
    CHECK(WaitFor(^BOOL{ return [Evaluate(web, @"typed.value") isEqual:@""]; }));
    Key(window, @"v", 9, NSEventModifierFlagCommand);
    CHECK(WaitFor(^BOOL{ return [Evaluate(web, @"typed.value") isEqual:@"x"]; }));
    [TestBoard clearContents];
    CHECK([TestBoard setString:@"smoke sentinel" forType:NSPasteboardTypeString]);
    Click(web, 171, 23);
    CHECK(WaitFor(^BOOL{ return [web.URL.path isEqual:@"/form"] && !web.isLoading; }));
    CHECK([Evaluate(web, @"document.title") isEqual:@"Form passed"]);
    [back performClick:nil];
    CHECK(WaitFor(^BOOL{ return [web.URL.path isEqual:@"/start"] && select.enabled && !web.isLoading; }));

    // At native x = clientWidth - 4, reducing the scale to the visual viewport
    // would miss this eight-point target beside the fixture's classic scrollbar.
    NSNumber *scrollbar = Evaluate(web, @"innerWidth - document.documentElement.clientWidth");
    CHECK(scrollbar.integerValue == 32);
    CGFloat edge = [Evaluate(web, @"document.getElementById('edge').getBoundingClientRect().left + 4") doubleValue];
    [select performClick:nil];
    CHECK(WaitFor(^BOOL{ return [window.firstResponder isKindOfClass:MiloPickerSurface.class]; }));
    Click((NSView *)window.firstResponder, edge, 67);
    CHECK(WaitFor(^BOOL{ return !copy.isHidden && copy.enabled; }));
    CHECK([review.string isEqual:@"html:nth-child(1) > body:nth-child(2) > div:nth-child(8)"]);
    [cancel performClick:nil];

    // Native element pick selects the custom element containing planted secrets.
    // Verify no page strings escape, cancel does not copy, and explicit copy does.
    for (int i = 0; i < 4; ++i) {
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
        } else if (i == 2) {
            CHECK([@"move" writeToFile:hmrFile atomically:YES encoding:NSUTF8StringEncoding error:NULL]);
            CHECK(WaitFor(^BOOL{ return [Evaluate(web, @"hmr.textContent") isEqual:@"move"]; }));
            [copy performClick:nil];
            CHECK(WaitFor(^BOOL{ return HasStatus(content, @"The page changed."); }));
            CHECK([[TestBoard stringForType:NSPasteboardTypeString] isEqual:@"smoke sentinel"]);
            CHECK([@"reset" writeToFile:hmrFile atomically:YES encoding:NSUTF8StringEncoding error:NULL]);
            CHECK(WaitFor(^BOOL{ return [Evaluate(web, @"hmr.textContent") isEqual:@"reset"]; }));
        } else {
            [copy performClick:nil];
            CHECK(WaitFor(^BOOL{ return copy.isHidden && [[TestBoard stringForType:NSPasteboardTypeString] isEqual:path]; }));
        }
    }

    Click(web, 50, 107); // a target=_blank link to the unapproved port
    CHECK(WaitFor(^BOOL{ return HasStatus(content, @"Blocked a navigation"); }));
    Click(web, 50, 145); // same-origin path that redirects to that port
    CHECK(WaitFor(^BOOL {
        return [NSFileManager.defaultManager fileExistsAtPath:[escapedRequest stringByAppendingString:@".redirect"]];
    }));
    CHECK(WaitFor(^BOOL{ return !web.isLoading && select.enabled && HasStatus(content, @"Blocked a navigation"); }));
    CHECK([Evaluate(web, @"document.title") isEqual:@"Milo fixture"]);
    CHECK(![NSFileManager.defaultManager fileExistsAtPath:escapedRequest]);
    [terminal performClick:nil];
    CHECK(returns == 1);
    Click(web, 80, 23);
    CHECK(WaitFor(^BOOL{ return [Evaluate(web, @"document.activeElement.id") isEqual:@"typed"]; }));
    CHECK(window.firstResponder == web);
    Key(window, @"\r", 36, NSEventModifierFlagCommand | NSEventModifierFlagShift);
    CHECK(WaitFor(^BOOL{ return returns == 2; }));
    CHECK([Evaluate(web, @"document.activeElement.id") isEqual:@"typed"]);
    [window orderOut:nil];
    window.contentView = nil;
    [window close];
    method_setImplementation(method, original);
    method_setImplementation(namedMethod, (IMP)OriginalNamedPasteboard);
    [TestBoard releaseGlobally];
    TestBoard = nil;
}

int main(int argc, char **argv) {
    @autoreleasepool {
        CheckShortcuts();
        if (argc == 2 && [@(argv[1]) isEqualToString:@"--keys-only"]) {
            puts("Native NSEvent accelerator selection passed (no window or focus required).");
            return 0;
        }
        CHECK(argc == 4);
        CheckPolicies();
        [NSApplication.sharedApplication setActivationPolicy:NSApplicationActivationPolicyAccessory];
        [NSApp finishLaunching];
        DoSmoke([NSURL URLWithString:@(argv[1])], @(argv[2]), @(argv[3]));
        puts("Native WebKit navigation, form, websocket hot reload, element picker and origin policy passed.");
    }
}
