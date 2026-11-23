// Compiled only by smoke.sh. Include the implementation to test its private pure policy helpers.
#import <objc/runtime.h>
#import "../Browser.m"

#define CHECK(condition) do { if (!(condition)) { \
    fprintf(stderr, "Browser smoke failed at line %d: %s\n", __LINE__, #condition); exit(1); \
} } while (0)

static void CheckPolicies(void) {
    CHECK(!PreviewWebURL(nil));
    CHECK([PreviewLoopbackOrigin([NSURL URLWithString:@"HTTP://LOCALHOST:80/a"]) isEqual:@"http://localhost"]);
    CHECK([PreviewLoopbackOrigin([NSURL URLWithString:@"https://[::1]:5443/a"]) isEqual:@"https://[::1]:5443"]);
    CHECK([PreviewLoopbackOrigin([NSURL URLWithString:@"http://127.0.0.1:3000/a?query#fragment"]) isEqual:@"http://127.0.0.1:3000"]);
    NSData *nonRFC = [@"http://127.0.0.1:3000/a?filter[status]=open|{ready}#x^y" dataUsingEncoding:NSUTF8StringEncoding];
    NSURL *unusual = [NSURL URLWithDataRepresentation:nonRFC relativeToURL:nil];
    CHECK([PreviewLoopbackOrigin(unusual) isEqual:@"http://127.0.0.1:3000"]);
    CHECK(!MiloPreviewContent(unusual, ^BOOL{ return NO; }, ^{}));
    for (NSString *url in @[@"http://localhost:0/", @"http://localhost:65536/", @"http://user@localhost/", @"http://localhost@elsewhere/",
                             @"http://127.1/", @"http://127.0.0.2/", @"http://%6cocalhost/", @"http://localhost.example/",
                             @"http://[::2]/", @"http://localhost./", @"file:///tmp/file", @"ws://localhost/", @"mailto:local@example.com"])
        CHECK(!PreviewLoopbackOrigin([NSURL URLWithString:url]));
    CHECK([PreviewAddressURL(@" example.com/path?q=test#part ").absoluteString isEqual:@"https://example.com/path?q=test#part"]);
    CHECK([PreviewAddressURL(@"localhost:3000/a").absoluteString isEqual:@"http://localhost:3000/a"]);
    CHECK([PreviewAddressURL(@"127.0.0.1:3000/a").absoluteString isEqual:@"http://127.0.0.1:3000/a"]);
    CHECK([PreviewAddressURL(@"[::1]:3000/a").absoluteString isEqual:@"http://[::1]:3000/a"]);
    for (NSString *address in @[@"", @"/relative", @"file:///tmp/file", @"javascript:1+1", @"data:text/html,a",
                                  @"mailto:local@example.com", @"http://u:p@localhost/", @"https://u%40x@example.com/",
                                  @"https://", @"http://localhost:65536", @"ftp://host.com", @"https://ex\\am.ple/"])
        CHECK(!PreviewAddressURL(address));
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

static void DispatchKeyThroughApplication(NSEvent *event) {
    // AppKit need not return the same NSEvent object from its queue. Dispatch
    // through NSApplication directly without consuming unrelated input events.
    [NSApp sendEvent:event];
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
    CHECK(PreviewShortcutAction(KeyEvent(nil, @"l", 37, NSEventModifierFlagCommand)) == @selector(focusAddress:));
    CHECK(PreviewShortcutAction(KeyEvent(nil, @"r", 15, NSEventModifierFlagCommand)) == @selector(reload:));
    CHECK(PreviewShortcutAction(KeyEvent(nil, @"[", 33, NSEventModifierFlagCommand)) == @selector(goBack:));
    CHECK(PreviewShortcutAction(KeyEvent(nil, @"]", 30, NSEventModifierFlagCommand)) == @selector(goForward:));
    CHECK(PreviewShortcutAction(KeyEvent(nil, @"l", 37, NSEventModifierFlagCommand | NSEventModifierFlagOption)) == NULL);
}

static NSPasteboard *TestBoard;
static NSPasteboard *(*OriginalNamedPasteboard)(id, SEL, NSPasteboardName);

static NSPasteboard *TestGeneralPasteboard(id receiver, SEL selector) {
    CHECK(TestBoard != nil);
    return TestBoard;
}

static NSPasteboard *TestNamedPasteboard(id receiver, SEL selector, NSPasteboardName name) {
    return [name isEqualToString:NSPasteboardNameGeneral] ?
        TestGeneralPasteboard(receiver, selector) : OriginalNamedPasteboard(receiver, selector, name);
}

// Intercept both general-pasteboard lookups. Without a disposable board, any
// access fails the fixture before it can touch the user's clipboard.
static void WithIsolatedPasteboard(void (^test)(void)) {
    Method method = class_getClassMethod(NSPasteboard.class, @selector(generalPasteboard));
    IMP original = method_setImplementation(method, (IMP)TestGeneralPasteboard);
    Method namedMethod = class_getClassMethod(NSPasteboard.class, @selector(pasteboardWithName:));
    OriginalNamedPasteboard = (NSPasteboard *(*)(id, SEL, NSPasteboardName))
        method_setImplementation(namedMethod, (IMP)TestNamedPasteboard);
    test();
    method_setImplementation(method, original);
    method_setImplementation(namedMethod, (IMP)OriginalNamedPasteboard);
}

static BOOL HasStatus(NSView *content, NSString *prefix) {
    for (NSView *view in content.subviews) {
        if ([view isKindOfClass:NSTextField.class] &&
            [((NSTextField *)view).stringValue hasPrefix:prefix]) return YES;
    }
    return NO;
}

static void SetViewport(NSView *content, CGFloat width, CGFloat height) {
    NSSize size = NSMakeSize(width, height);
    [content.window setContentSize:size];
    CHECK(NSEqualSizes(content.bounds.size, size));
}

static WKWebView *VisibleWebView(NSView *content) {
    WKWebView *active = nil;
    for (NSView *view in content.subviews) {
        if (!view.isHidden && [view isKindOfClass:WKWebView.class]) {
            CHECK(!active);
            active = (WKWebView *)view;
        }
    }
    return active;
}

static WKWebView *WaitForPageAt(int line, NSView *content, NSTextField *address, NSURL *url, NSString *title) {
    BOOL arrived = WaitFor(^BOOL{
        WKWebView *web = VisibleWebView(content);
        return [web.URL isEqual:url] && !web.isLoading && [web.title isEqualToString:title] &&
            [address.stringValue isEqualToString:web.URL.absoluteString] && !address.currentEditor;
    });
    if (!arrived) {
        WKWebView *web = VisibleWebView(content);
        fprintf(stderr, "Navigation at line %d timed out (URL %d, title %d, loading %d, address %d, editing %d)\n",
            line, [web.URL isEqual:url], [web.title isEqualToString:title], web.isLoading,
            [address.stringValue isEqualToString:web.URL.absoluteString], address.currentEditor != nil);
        fprintf(stderr, "URL components matched (scheme %d, host %d, path %d, query %d, fragment %d)\n",
            [(web.URL.scheme ?: @"") isEqual:(url.scheme ?: @"")], [(web.URL.host ?: @"") isEqual:(url.host ?: @"")],
            [(web.URL.path ?: @"") isEqual:(url.path ?: @"")], [(web.URL.query ?: @"") isEqual:(url.query ?: @"")],
            [(web.URL.fragment ?: @"") isEqual:(url.fragment ?: @"")]);
        exit(1);
    }
    return VisibleWebView(content);
}

#define WaitForPage(...) WaitForPageAt(__LINE__, __VA_ARGS__)

static NSTextView *FocusAddress(NSView *content, NSTextField *address) {
    DispatchKeyThroughApplication(KeyEvent(content.window, @"l", 37, NSEventModifierFlagCommand));
    CHECK(WaitFor(^BOOL{ return address.currentEditor && content.window.firstResponder == address.currentEditor; }));
    NSTextView *editor = (NSTextView *)address.currentEditor;
    CHECK(NSEqualRanges(editor.selectedRange, NSMakeRange(0, editor.string.length)));
    return editor;
}

static void DraftAddress(NSView *content, NSTextField *address, NSString *value) {
    NSTextView *editor = FocusAddress(content, address);
    [editor insertText:value replacementRange:editor.selectedRange];
    CHECK([editor.string isEqualToString:value]);
}

static void SubmitAddress(NSView *content, NSTextField *address, NSString *value) {
    DraftAddress(content, address, value);
    DispatchKeyThroughApplication(KeyEvent(content.window, @"\r", 36, 0));
}

static void ClickElement(WKWebView *web, NSString *name) {
    [web.window.contentView layoutSubtreeIfNeeded];
    NSString *script = [NSString stringWithFormat:
        @"(() => { const a = document.getElementById('%@'); const r = a.getBoundingClientRect();"
        "const x = r.x + r.width / 2, y = r.y + r.height / 2;"
        "return document.elementFromPoint(x, y) === a ? [x, y] : null; })()", name];
    NSArray *point = Evaluate(web, script);
    CHECK(point.count == 2);
    Click(web, [point[0] doubleValue], [point[1] doubleValue]);
}

static void CheckOpaqueViewport(NSView *content, WKWebView *web) {
    [content layoutSubtreeIfNeeded];
    CHECK(content.isOpaque && content.clipsToBounds && web.clipsToBounds);
    CHECK(web.underPageBackgroundColor.alphaComponent == 1);
    CHECK(NSMinX(web.frame) == NSMinX(content.bounds) && NSMaxX(web.frame) == NSMaxX(content.bounds) &&
        NSMaxY(web.frame) == NSMaxY(content.bounds));
    CHECK([Evaluate(web, @"getComputedStyle(document.body).backgroundColor") isEqual:@"rgba(0, 0, 0, 0)"]);
    CHECK([Evaluate(web, @"getComputedStyle(document.getElementById('outside-edge')).backgroundColor") isEqual:@"rgb(229, 17, 39)"]);
    CHECK([Evaluate(web, @"document.getElementById('outside-edge').getBoundingClientRect().right === innerWidth") isEqual:@YES]);
    __block BOOL finished = NO;
    __block NSImage *snapshot = nil;
    WKSnapshotConfiguration *configuration = [WKSnapshotConfiguration new];
    configuration.afterScreenUpdates = YES;
    [web takeSnapshotWithConfiguration:configuration completionHandler:^(NSImage *image, NSError *error) {
        snapshot = image;
        finished = YES;
    }];
    CHECK(WaitFor(^BOOL{ return finished; }));
    CHECK(snapshot);
    NSBitmapImageRep *bitmap = [NSBitmapImageRep imageRepWithData:snapshot.TIFFRepresentation];
    CHECK(bitmap.pixelsWide >= 24 && bitmap.pixelsHigh >= 24);
    NSColor *center = [bitmap colorAtX:bitmap.pixelsWide / 2 y:bitmap.pixelsHigh / 2];
    NSColor *edge = [[bitmap colorAtX:bitmap.pixelsWide - 4 y:bitmap.pixelsHigh / 2] colorUsingColorSpace:NSColorSpace.sRGBColorSpace];
    CHECK(center.alphaComponent > 0.99 && edge.alphaComponent > 0.99);
    CHECK(edge.redComponent > 0.75 && edge.greenComponent < 0.25 && edge.blueComponent < 0.30);
}

static void CheckControlFrames(NSView *content, NSArray<NSButton *> *controls) {
    [content layoutSubtreeIfNeeded];
    for (NSUInteger i = 0; i < controls.count; ++i) {
        NSButton *button = controls[i];
        CHECK(!button.isHidden && !NSIsEmptyRect(button.frame) && NSContainsRect(content.bounds, button.frame));
        for (NSUInteger j = 0; j < i; ++j) CHECK(!NSIntersectsRect(button.frame, controls[j].frame));
    }
}

static void CheckControls(NSView *content, NSArray<NSButton *> *controls) {
    CheckControlFrames(content, controls);
    for (NSButton *button in controls) {
        if (!button.isEnabled) continue;
        NSPoint center = NSMakePoint(NSMidX(button.frame), NSMidY(button.frame));
        NSView *hit = [content hitTest:[content convertPoint:center toView:content.superview]];
        CHECK(hit == button || [hit isDescendantOf:button]);
    }
}

static NSView *ArmPicker(NSView *content, NSButton *select) {
    [content layoutSubtreeIfNeeded];
    CHECK(select.isEnabled);
    [select performClick:nil];
    CHECK(WaitFor(^BOOL{ return [content.window.firstResponder isKindOfClass:MiloPickerSurface.class]; }));
    return (NSView *)content.window.firstResponder;
}

static void ReviewElement(NSView *content, NSButton *select, NSButton *copy, NSTextView *review, NSString *path) {
    Click(ArmPicker(content, select), 70, 67);
    CHECK(WaitFor(^BOOL{ return !copy.isHidden && copy.isEnabled; }));
    CHECK([review.string isEqualToString:path]);
}

static void CheckNarrowPicker(NSView *content, WKWebView *web, NSTextView *review,
                              NSButton *back, NSButton *reload, NSButton *select, NSButton *terminal,
                              NSButton *cancel, NSButton *copy) {
    NSString *path = @"html:nth-child(1) > body:nth-child(2) > *:nth-child(2)";
    NSInteger changeCount = TestBoard.changeCount;
    ReviewElement(content, select, copy, review, path);

    // Even a resize to a viewport that supports review requires a fresh pick.
    SetViewport(content, 144, 240);
    CHECK(copy.isHidden && !copy.isEnabled && cancel.isHidden && review.string.length == 0);
    CheckControls(content, @[back, reload, select, terminal]);
    CHECK(WaitFor(^BOOL{ return [Evaluate(web, @"innerWidth") isEqual:@144]; }));

    ArmPicker(content, select);
    CheckControls(content, @[back, reload, select, terminal, cancel]);
    SetViewport(content, 144, 150);
    CHECK(!select.isEnabled && cancel.isHidden && content.window.firstResponder == web);
    CheckControls(content, @[back, reload, select, terminal]);
    // A direct action cannot bypass the unavailable Select button.
    CHECK([NSApp sendAction:select.action to:select.target from:select]);
    CHECK(cancel.isHidden);
    SetViewport(content, 144, 240);

    ReviewElement(content, select, copy, review, path);
    CheckControls(content, @[back, reload, select, terminal, cancel, copy]);
    CHECK(!review.enclosingScrollView.isHidden);
    CHECK(NSContainsRect(content.bounds, review.enclosingScrollView.frame));
    CHECK(!NSIntersectsRect(review.enclosingScrollView.frame, cancel.frame));
    CHECK(!NSIntersectsRect(review.enclosingScrollView.frame, copy.frame));
    CHECK(content.window.firstResponder != copy);
    [cancel performClick:nil];
    CHECK(copy.isHidden && TestBoard.changeCount == changeCount);

    ReviewElement(content, select, copy, review, path);
    // Dispatch synchronously instead of running a button animation while the
    // WebKit callback is pending. Shrink and grow before pumping the event loop.
    CHECK([NSApp sendAction:copy.action to:copy.target from:copy]);
    CHECK(!copy.isHidden && !copy.isEnabled);
    SetViewport(content, 110, 240);
    CHECK(!select.isEnabled && copy.isHidden && cancel.isHidden && review.string.length == 0);
    CheckControls(content, @[back, reload, terminal]);
    CHECK(select.isHidden);
    SetViewport(content, 144, 240);
    CHECK(copy.isHidden && cancel.isHidden && review.string.length == 0);

    ReviewElement(content, select, copy, review, path);
    CHECK(TestBoard.changeCount == changeCount);
    [copy performClick:nil];
    CHECK(WaitFor(^BOOL{ return copy.isHidden && [[TestBoard stringForType:NSPasteboardTypeString] isEqual:path]; }));
    [TestBoard clearContents];
    CHECK([TestBoard setString:@"smoke sentinel" forType:NSPasteboardTypeString]);
    SetViewport(content, 430, 410);
    [content layoutSubtreeIfNeeded];
}

@interface SmokePanel : NSPanel
@end

@implementation SmokePanel
- (BOOL)canBecomeKeyWindow { return YES; }
- (BOOL)canBecomeMainWindow { return NO; }
@end

@interface OffscreenSmokeWindow : NSWindow
@property (nonatomic) BOOL reportsKeyAndVisible;
@end

@implementation OffscreenSmokeWindow
- (BOOL)isKeyWindow { return self.reportsKeyAndVisible || [super isKeyWindow]; }
- (BOOL)isVisible { return self.reportsKeyAndVisible || [super isVisible]; }
@end

// Keep the browser's real state machine and hold only the WebKit IPC reply so
// the test can revoke authorization between dispatch and completion.
@interface PickerReplyWebView : MiloPreviewWebView
@property (nonatomic, strong) NSURL *fixtureURL;
@property (nonatomic, copy) NSString *mode;
@property (nonatomic, copy) void (^reply)(id, NSError *);
- (void)deliverMode:(NSString *)mode result:(id)result;
@end

@implementation PickerReplyWebView
- (NSURL *)URL { return self.fixtureURL; }
- (void)callAsyncJavaScript:(NSString *)script arguments:(NSDictionary<NSString *, id> *)arguments
    inFrame:(WKFrameInfo *)frame inContentWorld:(WKContentWorld *)world
    completionHandler:(void (^)(id, NSError *))completion {
    CHECK(!self.reply && completion);
    self.mode = arguments[@"mode"];
    self.reply = completion;
}
- (void)deliverMode:(NSString *)mode result:(id)result {
    void (^reply)(id, NSError *) = self.reply;
    CHECK(reply && [self.mode isEqualToString:mode]);
    self.reply = nil;
    reply(result, nil);
}
@end

@interface LayoutSmokeBrowser : MiloPreviewBrowser
- (void)checkLayout;
- (void)checkRevokedCopy:(void (^)(BOOL))setAuthorized;
@end

@implementation LayoutSmokeBrowser
// This fixture exercises native geometry and invalidation, not page loading
// or consent gestures. The full smoke uses a real committed document for those.
- (void)compileRules {}
- (BOOL)hasCommittedLocalPage {
    return [_web isKindOfClass:PickerReplyWebView.class] ? [super hasCommittedLocalPage] : _pageCommitted;
}

- (void)prepareReview {
    _pickState = PickReview;
    _pickURL = _initialURL;
    _path.string = @"html:nth-child(1) > body:nth-child(2) > div:nth-child(2)";
    [self updateChrome:@"Review element path"];
}

- (void)checkLayout {
    CHECK(self.window && !self.window.isVisible && !self.window.isKeyWindow);
    _pageCommitted = YES;
    [self updateChrome:@""];
    CheckControlFrames(self, @[_back, _reload, _select, _terminal]);
    CHECK(_select.isEnabled);

    [self prepareReview];
    CheckControlFrames(self, @[_back, _reload, _select, _terminal, _cancel, _copy]);
    SetViewport(self, 144, 240);
    CHECK(_pickState == PickIdle && !_pickURL && _path.string.length == 0);
    CHECK(_copy.isHidden && !_copy.isEnabled && _cancel.isHidden && _review.isHidden);
    CheckControlFrames(self, @[_back, _reload, _select, _terminal]);
    CHECK(_select.isEnabled);

    [self prepareReview];
    CheckControlFrames(self, @[_back, _reload, _select, _terminal, _cancel, _copy]);
    CHECK(_copy.isEnabled && !_review.isHidden && NSContainsRect(self.bounds, _review.frame));
    CHECK(!NSIntersectsRect(_review.frame, _cancel.frame) && !NSIntersectsRect(_review.frame, _copy.frame));
    CHECK(NSMaxY(_cancel.frame) <= NSMinY(_copy.frame));
    SetViewport(self, 144, 150);
    CHECK(_pickState == PickIdle && !_pickURL && _path.string.length == 0);
    CHECK(!_select.isEnabled && _copy.isHidden && !_copy.isEnabled && _cancel.isHidden && _review.isHidden);
    CheckControlFrames(self, @[_back, _reload, _select, _terminal]);

    SetViewport(self, 144, 240);
    [self prepareReview];
    _pickState = PickCopying;
    [self updateChrome:@"Review element path"];
    NSUInteger generation = _pickID;
    SetViewport(self, 110, 240);
    CHECK(!_select.isEnabled && _pickID != generation && _pickState == PickIdle);
    CheckControlFrames(self, @[_back, _reload, _terminal]);
    CHECK(_select.isHidden && _copy.isHidden && _cancel.isHidden);
    SetViewport(self, 144, 240);
    CHECK(_select.isEnabled && _copy.isHidden && _review.isHidden && _path.string.length == 0);
    CheckControlFrames(self, @[_back, _reload, _select, _terminal]);
    SetViewport(self, 80, 70);
    [self layoutSubtreeIfNeeded];
    CHECK(!_address.isHidden && _terminal.isHidden && NSHeight(_web.frame) >= PreviewMinPageHeight);
    SetViewport(self, 80, 45);
    [self layoutSubtreeIfNeeded];
    CHECK(_address.isHidden && NSEqualRects(_web.frame, self.bounds));
    SetViewport(self, 144, 240);
    CHECK(!self.window.isVisible && !self.window.isKeyWindow);
}

- (void)checkRevokedCopy:(void (^)(BOOL))setAuthorized {
    PickerReplyWebView *web = [[PickerReplyWebView alloc] initWithFrame:_web.frame configuration:_web.configuration];
    web.fixtureURL = _initialURL;
    [self stopObservingWebView:_web];
    [self replaceSubview:_web with:web];
    _localWeb = _web = web;
    [self observeWebView:web];
    _committedURL = _initialURL;
    _pickerDocument = YES; // the held-reply fixture models a validated HTML response

    CHECK(self.window.isVisible && self.window.isKeyWindow && _pageCommitted);
    [self armPick:nil];
    CHECK(_pickState == PickIdle && !web.reply);

    NSArray *nodes = @[@[@([PreviewTags() indexOfObject:@"html"]), @1]];
    setAuthorized(YES);
    _rulesReady = YES;
    [self armPick:nil];
    CHECK(web.reply && _pickState == PickArming);
    [_address selectText:nil]; // click/field-editor path, not the Cmd-L action
    NSTextView *draft = (NSTextView *)_address.currentEditor;
    CHECK(draft && self.window.firstResponder == draft);
    [draft insertText:@"draft.example" replacementRange:NSMakeRange(NSNotFound, 0)];
    [web deliverMode:@"arm" result:@YES];
    CHECK(WaitFor(^BOOL{ return _pickState == PickIdle; }) && self.window.firstResponder == draft);
    CHECK([draft.string isEqualToString:@"draft.example"]);
    [self endAddressEdit];
    [self armPick:nil];
    CHECK(_pickState == PickArming);
    [web deliverMode:@"arm" result:@YES];
    CHECK(_pickState == PickArmed);
    [self pickAt:NSMakePoint(0.5, 0.5)];
    CHECK(_pickState == PickReading);
    [web deliverMode:@"hit" result:nodes];
    CHECK(_pickState == PickReview && [_path.string isEqualToString:@"html:nth-child(1)"]);
    [self copyPick:_copy];
    CHECK(_pickState == PickCopying && !_copy.isEnabled && web.reply);

    setAuthorized(NO);
    [web deliverMode:@"copy" result:nodes];
    CHECK(_pickState == PickIdle && _copy.isHidden && _path.string.length == 0);
}
@end

static void DoLayoutSmoke(void) {
    [NSApplication.sharedApplication setActivationPolicy:NSApplicationActivationPolicyProhibited];
    OffscreenSmokeWindow *window = [[OffscreenSmokeWindow alloc] initWithContentRect:NSMakeRect(0, 0, 430, 410)
                                                                           styleMask:NSWindowStyleMaskBorderless
                                                                             backing:NSBackingStoreBuffered defer:YES];
    window.releasedWhenClosed = NO;
    __block BOOL authorized = NO;
    LayoutSmokeBrowser *content = [[LayoutSmokeBrowser alloc]
        initWithURL:[NSURL URLWithString:@"http://127.0.0.1/"] localOrigin:@"http://127.0.0.1"
        acceptsPanelInput:^BOOL{ return authorized; } returnFocus:^{}];
    window.contentView = content;
    [content checkLayout];
    window.reportsKeyAndVisible = YES;
    WithIsolatedPasteboard(^{
        [content checkRevokedCopy:^(BOOL allowed) { authorized = allowed; }];
    });
    window.reportsKeyAndVisible = NO;
    window.contentView = nil;
    [window close];
}

static void DoSmoke(NSURL *url, NSString *hmrFile, NSString *escapedRequest, NSURL *outside, NSURL *alias, NSURL *secure) {
    // Do not read, clear or write the user's pasteboard. Only this test process
    // resolves generalPasteboard to a newly named, disposable pasteboard.
    TestBoard = NSPasteboard.pasteboardWithUniqueName;
    CHECK([TestBoard setString:@"smoke sentinel" forType:NSPasteboardTypeString]);

    __block NSUInteger returns = 0;
    NSView *content = MiloPreviewContent(url, ^BOOL{ return YES; }, ^{ ++returns; });
    CHECK(content != nil);
    NSPanel *window = [[SmokePanel alloc] initWithContentRect:NSMakeRect(60, 60, 430, 410)
                                                   styleMask:NSWindowStyleMaskBorderless | NSWindowStyleMaskNonactivatingPanel
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
    NSTextField *address = nil;
    NSButton *select = nil, *copy = nil, *cancel = nil, *back = nil, *forward = nil, *reload = nil, *terminal = nil;
    for (NSView *view in content.subviews) {
        if ([view isKindOfClass:WKWebView.class]) web = (WKWebView *)view;
        if ([view isKindOfClass:NSScrollView.class]) review = ((NSScrollView *)view).documentView;
        if ([view isKindOfClass:NSTextField.class] && [[view accessibilityLabel] isEqual:@"Web address"])
            address = (NSTextField *)view;
        if (![view isKindOfClass:NSButton.class]) continue;
        NSButton *button = (NSButton *)view;
        NSString *name = [button accessibilityLabel] ?: button.title;
        if ([name isEqual:@"Select an element"]) select = button;
        if ([name isEqual:@"Copy path"]) copy = button;
        if ([name isEqual:@"Cancel"]) cancel = button;
        if ([name isEqual:@"Go back"]) back = button;
        if ([name isEqual:@"Go forward"]) forward = button;
        if ([name isEqual:@"Reload preview"]) reload = button;
        if ([name isEqual:@"Return to terminal"]) terminal = button;
    }
    CHECK(web && address && review && select && copy && cancel && back && forward && reload && terminal);
    CHECK(!web.configuration.websiteDataStore.isPersistent);
    CHECK(WaitFor(^BOOL{ return select.enabled && !web.isLoading; }));
    CHECK([address.stringValue isEqualToString:web.URL.absoluteString]);
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
    SetViewport(content, 144, 240);
    CheckControls(content, @[back, reload, select, terminal]);
    CHECK(back.isEnabled && reload.isEnabled && terminal.isEnabled);
    [back performClick:nil];
    CHECK(WaitFor(^BOOL{ return [web.URL.path isEqual:@"/start"] && select.enabled && !web.isLoading; }));
    SetViewport(content, 430, 410);
    [content layoutSubtreeIfNeeded];

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

    CheckNarrowPicker(content, web, review, back, reload, select, terminal, cancel, copy);

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

    Click(web, 50, 107); // a target=_blank link is never opened
    CHECK(WaitFor(^BOOL{ return HasStatus(content, @"Pop-up windows are disabled."); }));
    [terminal performClick:nil];
    CHECK(returns == 1);
    Click(web, 80, 23);
    CHECK(WaitFor(^BOOL{ return [Evaluate(web, @"document.activeElement.id") isEqual:@"typed"]; }));
    CHECK(window.firstResponder == web);
    // Positive control: ordinary Shift-Return must reach the focused page.
    CHECK([Evaluate(web, @"window.returnKeyDowns") isEqual:@0]);
    DispatchKeyThroughApplication(KeyEvent(window, @"\r", 36, NSEventModifierFlagShift));
    CHECK(WaitFor(^BOOL{ return [Evaluate(web, @"window.returnKeyDowns") isEqual:@1]; }));
    CHECK(returns == 1);
    DispatchKeyThroughApplication(KeyEvent(window, @"\r", 36, NSEventModifierFlagCommand | NSEventModifierFlagShift));
    CHECK(WaitFor(^BOOL{ return returns == 2; }));
    CHECK([Evaluate(web, @"document.activeElement.id") isEqual:@"typed"]);
    CHECK([Evaluate(web, @"window.returnKeyDowns") isEqual:@1]);

    SetViewport(content, 144, 240);
    CheckControls(content, @[back, forward, reload, select, terminal]);
    CHECK(address.isEditable && !address.isHidden && NSContainsRect(content.bounds, address.frame));
    DraftAddress(content, address, @"example.com");
    CHECK([web.URL isEqual:url]); // editing alone makes no navigation
    DispatchKeyThroughApplication(KeyEvent(window, @"\e", 53, 0));
    CHECK(!address.currentEditor && [address.stringValue isEqualToString:web.URL.absoluteString]);
    NSURL *typed = [NSURL URLWithString:@"start?mode=typed#address" relativeToURL:url].absoluteURL;
    SubmitAddress(content, address, [typed.absoluteString substringFromIndex:[@"http://" length]]);
    CHECK(WaitForPage(content, address, typed, @"Milo fixture") == web);
    SetViewport(content, 430, 470);

    NSURL *hash = [NSURL URLWithString:@"#section" relativeToURL:typed].absoluteURL;
    ClickElement(web, @"hash");
    CHECK(WaitForPage(content, address, hash, @"Milo fixture") == web);
    CHECK(select.enabled);
    DraftAddress(content, address, @"unsubmitted.example");
    DispatchKeyThroughApplication(KeyEvent(window, @"[", 33, NSEventModifierFlagCommand));
    CHECK(WaitForPage(content, address, typed, @"Milo fixture") == web);
    [forward performClick:nil];
    CHECK(WaitForPage(content, address, hash, @"Milo fixture") == web);
    SubmitAddress(content, address, @"file:///__milo_browser_fixture_missing");
    CHECK(HasStatus(content, @"Enter a valid HTTP or HTTPS address."));
    CHECK([((NSTextView *)address.currentEditor).string isEqual:@"file:///__milo_browser_fixture_missing"]);
    DispatchKeyThroughApplication(KeyEvent(window, @"\e", 53, 0));
    CHECK([address.stringValue isEqualToString:hash.absoluteString]);
    ClickElement(web, @"unsafe");
    CHECK([web.URL isEqual:hash]);
    ClickElement(web, @"download");
    CHECK(WaitFor(^BOOL{ return HasStatus(content, @"Downloads are disabled."); }));
    CHECK(![NSFileManager.defaultManager fileExistsAtPath:[escapedRequest stringByAppendingString:@".download"]]);
    ClickElement(web, @"attachment");
    CHECK(WaitFor(^BOOL{
        return [NSFileManager.defaultManager fileExistsAtPath:[escapedRequest stringByAppendingString:@".attachment"]] &&
            back.enabled && HasStatus(content, @"Downloads are disabled.");
    }));
    CHECK(WaitForPage(content, address, hash, @"Milo fixture") == web);

    // A local top-level scripted departure is not an explicit click.
    Evaluate(web, [NSString stringWithFormat:@"location.href = '%@'; true", outside.absoluteString]);
    CHECK(WaitFor(^BOOL{ return HasStatus(content, @"Open a link or enter an address"); }));
    CHECK([web.URL isEqual:hash]);
    ClickElement(web, @"outside");
    WKWebView *general = WaitForPage(content, address, outside, @"Outside fixture");
    CHECK(general != web && web.isHidden && !select.enabled);
    CHECK(general.configuration.userContentController.userScripts.count == 0);
    CHECK([NSApp sendAction:select.action to:select.target from:select]);
    CHECK(copy.isHidden && cancel.isHidden);
    CheckOpaqueViewport(content, general);
    SetViewport(content, 144, 240);
    CheckControls(content, @[back, forward, reload, select, terminal]);
    CHECK(VisibleWebView(content) == general && [Evaluate(general, @"innerWidth") isEqual:@144]);
    SetViewport(content, 430, 470);
    CHECK(VisibleWebView(content) == general);

    [back performClick:nil];
    CHECK(WaitForPage(content, address, hash, @"Milo fixture") == web);
    CHECK(select.enabled && copy.isHidden && cancel.isHidden);
    [forward performClick:nil];
    CHECK(WaitForPage(content, address, outside, @"Outside fixture") == general);
    Evaluate(general, @"window.fixtureReloadSentinel = true");
    DispatchKeyThroughApplication(KeyEvent(window, @"r", 15, NSEventModifierFlagCommand));
    CHECK(WaitFor(^BOOL{ return [Evaluate(general, @"typeof window.fixtureReloadSentinel") isEqual:@"undefined"]; }));
    CHECK(WaitForPage(content, address, outside, @"Outside fixture") == general);

    NSURL *outsideForm = [NSURL URLWithString:@"outside-form" relativeToURL:url].absoluteURL;
    ClickElement(general, @"outside-form");
    CHECK(WaitFor(^BOOL{
        return [general.URL isEqual:outsideForm] || HasStatus(content, @"This cross-site form cannot be sent");
    }));
    if ([general.URL isEqual:outsideForm]) {
        CHECK(WaitForPage(content, address, outsideForm, @"Outside form passed") == general);
        [back performClick:nil];
        CHECK(WaitForPage(content, address, outside, @"Outside fixture") == general);
    } else {
        puts("WebKit did not expose a complete cross-host POST body; preview refused replay.");
        CHECK([general.URL isEqual:outside]);
    }

    NSURL *cookieCheck = [NSURL URLWithString:@"cookie-check" relativeToURL:url].absoluteURL;
    CHECK([Evaluate(web, @"document.cookie.includes('milo_dev_fixture=private')") isEqual:@YES]);
    ClickElement(general, @"return");
    CHECK(WaitForPage(content, address, cookieCheck, @"Unprivileged local fixture") == general);
    CHECK(!select.enabled && ![NSFileManager.defaultManager fileExistsAtPath:[escapedRequest stringByAppendingString:@".cookie"]]);

    // The address draft stays the key recipient when any outstanding navigation,
    // including a view replacement, finally commits.
    SubmitAddress(content, address, url.absoluteString);
    CHECK(WaitForPage(content, address, url, @"Milo fixture") == web);
    NSURL *slow = [NSURL URLWithString:@"slow-outside" relativeToURL:outside].absoluteURL;
    SubmitAddress(content, address, slow.absoluteString);
    CHECK([address.stringValue isEqualToString:web.URL.absoluteString]);
    DraftAddress(content, address, @"retained-draft.example");
    CHECK(WaitFor(^BOOL{ return VisibleWebView(content) == general && [general.URL isEqual:slow] && !general.isLoading; }));
    CHECK(window.firstResponder == address.currentEditor &&
        [((NSTextView *)address.currentEditor).string isEqual:@"retained-draft.example"]);
    DispatchKeyThroughApplication(KeyEvent(window, @"\e", 53, 0));
    CHECK(WaitForPage(content, address, slow, @"Outside fixture") == general);
    SubmitAddress(content, address, url.absoluteString);
    CHECK(WaitForPage(content, address, url, @"Milo fixture") == web);
    CHECK(select.enabled && copy.isHidden && cancel.isHidden);
    NSInteger unchanged = TestBoard.changeCount;
    ReviewElement(content, select, copy, review, @"html:nth-child(1) > body:nth-child(2) > *:nth-child(2)");
    Evaluate(web, @"location.hash = 'during-review'; true");
    NSURL *duringReview = [NSURL URLWithString:@"#during-review" relativeToURL:url].absoluteURL;
    CHECK(WaitForPage(content, address, duringReview, @"Milo fixture") == web);
    CHECK(select.enabled && copy.isHidden && TestBoard.changeCount == unchanged);

    ClickElement(web, @"redirect");
    BOOL canAssociateRedirect = NO;
#if __MAC_OS_X_VERSION_MAX_ALLOWED >= 270000
    if (@available(macOS 27.0, *)) canAssociateRedirect = YES;
#endif
    if (!canAssociateRedirect) {
        CHECK(WaitFor(^BOOL{ return back.enabled && HasStatus(content, @"Open a link or enter an address"); }));
        CHECK(WaitForPage(content, address, duringReview, @"Milo fixture") == web);
        SubmitAddress(content, address, outside.absoluteString);
    }
    CHECK(WaitForPage(content, address, outside, @"Outside fixture") == general);
    CHECK(!select.enabled && [NSFileManager.defaultManager fileExistsAtPath:[escapedRequest stringByAppendingString:@".redirect"]]);
    CHECK(![NSFileManager.defaultManager fileExistsAtPath:[escapedRequest stringByAppendingString:@".referrer"]]);
    [back performClick:nil];
    CHECK(WaitForPage(content, address, duringReview, @"Milo fixture") == web);
    NSURL *failed = [NSURL URLWithString:@"failed" relativeToURL:url].absoluteURL;
    SubmitAddress(content, address, failed.absoluteString);
    CHECK(WaitFor(^BOOL{ return HasStatus(content, @"Could not load the page."); }));
    CHECK(WaitForPage(content, address, duringReview, @"Milo fixture") == web);
    SubmitAddress(content, address, secure.absoluteString);
    CHECK(WaitFor(^BOOL{ return HasStatus(content, @"Could not load the page."); }));
    CHECK(WaitForPage(content, address, duringReview, @"Milo fixture") == web);
    CHECK(![NSFileManager.defaultManager fileExistsAtPath:escapedRequest]);

    NSURL *sandbox = [NSURL URLWithString:@"sandbox" relativeToURL:url].absoluteURL;
    SubmitAddress(content, address, sandbox.absoluteString);
    CHECK(WaitForPage(content, address, sandbox, @"Sandbox fixture") == web);
    CHECK(!select.enabled && ![(MiloPreviewBrowser *)content canPick]);
    NSInteger beforeSandbox = TestBoard.changeCount;
    CHECK([NSApp sendAction:select.action to:select.target from:select]);
    CHECK(copy.isHidden && cancel.isHidden && TestBoard.changeCount == beforeSandbox);
    SubmitAddress(content, address, url.absoluteString);
    CHECK(WaitForPage(content, address, url, @"Milo fixture") == web && select.enabled);
    [back performClick:nil];
    CHECK(WaitForPage(content, address, sandbox, @"Sandbox fixture") == web);
    CHECK(!select.enabled && ![(MiloPreviewBrowser *)content canPick]);

    // A resolvable loopback alias is not literal startup authority. It cannot
    // create the trusted WebView, even after visiting a literal loopback URL.
    NSView *unprivileged = MiloPreviewContent(alias, ^BOOL{ return YES; }, ^{});
    CHECK(unprivileged && !PreviewLoopbackOrigin(alias));
    window.contentView = unprivileged;
    NSTextField *unprivilegedAddress = nil;
    NSButton *unprivilegedSelect = nil;
    for (NSView *view in unprivileged.subviews) {
        if ([view isKindOfClass:MiloPreviewAddress.class]) unprivilegedAddress = (NSTextField *)view;
        if ([view isKindOfClass:NSButton.class] && [[view accessibilityLabel] isEqual:@"Select an element"])
            unprivilegedSelect = (NSButton *)view;
    }
    CHECK(unprivilegedAddress && unprivilegedSelect);
    WKWebView *unprivilegedWeb = WaitForPage(unprivileged, unprivilegedAddress, alias, @"Outside fixture");
    CHECK(!unprivilegedSelect.enabled);
    SubmitAddress(unprivileged, unprivilegedAddress, url.absoluteString);
    CHECK(WaitForPage(unprivileged, unprivilegedAddress, url, @"Milo fixture") == unprivilegedWeb);
    CHECK(!unprivilegedSelect.enabled && unprivilegedWeb.configuration.userContentController.userScripts.count == 0);
    [window orderOut:nil];
    window.contentView = nil;
    [window close];
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
        if (argc == 2 && [@(argv[1]) isEqualToString:@"--layout-only"]) {
            DoLayoutSmoke();
            puts("Offscreen picker layout, resize invalidation, and async copy authorization passed without clipboard access.");
            return 0;
        }
        CHECK(argc == 7);
        CheckPolicies();
        [NSApplication.sharedApplication setActivationPolicy:NSApplicationActivationPolicyAccessory];
        [NSApp finishLaunching];
        WithIsolatedPasteboard(^{
            DoSmoke([NSURL URLWithString:@(argv[1])], @(argv[2]), @(argv[3]), [NSURL URLWithString:@(argv[4])],
                [NSURL URLWithString:@(argv[5])], [NSURL URLWithString:@(argv[6])]);
        });
        puts("Native WebKit address, HTTP navigation, reload/history, isolation, opaque viewport, forms/HMR, picker and shortcuts passed.");
    }
}
