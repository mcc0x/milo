#import "Browser.h"

#import <WebKit/WebKit.h>
#include <math.h>

// Returns the serialized web origin, not any user- or page-controlled URL path.
static NSString *PreviewOrigin(NSURL *url, BOOL initial) {
    if (!url) return nil;
    NSURLComponents *parts = [NSURLComponents componentsWithURL:url resolvingAgainstBaseURL:NO];
    NSString *scheme = parts.scheme.lowercaseString;
    BOOL tls = [scheme isEqualToString:@"https"];
    if (!tls && ![scheme isEqualToString:@"http"]) return nil;
    if (parts.percentEncodedUser || parts.percentEncodedPassword ||
        (initial && (parts.percentEncodedQuery || parts.percentEncodedFragment))) return nil;

    NSString *host = parts.percentEncodedHost.lowercaseString;
    if ([host isEqualToString:@"::1"]) host = @"[::1]";
    if (![host isEqualToString:@"[::1]"] && ![host isEqualToString:@"127.0.0.1"] &&
        ![host isEqualToString:@"localhost"]) return nil;
    NSInteger standardPort = tls ? 443 : 80;
    NSInteger port = parts.port ? parts.port.integerValue : standardPort;
    if (port < 1 || port > 65535) return nil;
    NSString *origin = [NSString stringWithFormat:@"%@://%@", scheme, host];
    return port == standardPort ? origin : [origin stringByAppendingFormat:@":%ld", (long)port];
}

static NSArray *PreviewRules(NSString *origin) {
    BOOL tls = [origin hasPrefix:@"https:"];
    NSString *authority = [origin substringFromIndex:tls ? 8 : 7];
    NSString *standardPort = @"";
    if ([authority isEqualToString:@"localhost"] || [authority isEqualToString:@"127.0.0.1"] ||
        [authority isEqualToString:@"[::1]"]) standardPort = tls ? @"(:443)?" : @"(:80)?";
    NSString *hostPort = [NSRegularExpression escapedPatternForString:authority];
    NSString *http = [NSString stringWithFormat:@"^%@://%@%@/", tls ? @"https" : @"http", hostPort, standardPort];
    NSString *ws = [NSString stringWithFormat:@"^%@://%@%@/", tls ? @"wss" : @"ws", hostPort, standardPort];
    return @[
        @{@"trigger": @{@"url-filter": @".*"}, @"action": @{@"type": @"block"}},
        @{@"trigger": @{@"url-filter": http}, @"action": @{@"type": @"ignore-previous-rules"}},
        @{@"trigger": @{@"url-filter": ws}, @"action": @{@"type": @"ignore-previous-rules"}},
        // These URLs refer to data already in the page, not another network origin.
        @{@"trigger": @{@"url-filter": @"^data:"}, @"action": @{@"type": @"ignore-previous-rules"}},
        @{@"trigger": @{@"url-filter": [@"^blob:" stringByAppendingString:[http substringFromIndex:1]]},
          @"action": @{@"type": @"ignore-previous-rules"}},
    ];
}

// Only the indices of known tags and sibling positions cross the WebKit boundary.
// In particular, custom-element names and attributes cannot become clipboard text.
static NSArray<NSString *> *PreviewTags(void) {
    return @[@"*", @"a", @"article", @"aside", @"audio", @"b", @"blockquote", @"body", @"br",
             @"button", @"canvas", @"caption", @"circle", @"code", @"dd", @"details", @"dialog",
             @"div", @"dl", @"dt", @"em", @"fieldset", @"figcaption", @"figure", @"footer", @"form",
             @"g", @"h1", @"h2", @"h3", @"h4", @"h5", @"h6", @"head", @"header", @"hr", @"html",
             @"i", @"iframe", @"img", @"input", @"label", @"legend", @"li", @"line", @"main", @"mark",
             @"menu", @"nav", @"ol", @"optgroup", @"option", @"output", @"p", @"path", @"picture",
             @"polygon", @"pre", @"progress", @"rect", @"section", @"select", @"slot", @"small", @"span",
             @"strong", @"summary", @"svg", @"table", @"tbody", @"td", @"text", @"textarea", @"tfoot",
             @"th", @"thead", @"time", @"tr", @"ul", @"use", @"video"];
}

enum { PreviewMaxDepth = 24, PreviewMaxPosition = 4096 };

static BOOL PreviewInteger(id value, NSUInteger lower, NSUInteger upper) {
    if (![value isKindOfClass:NSNumber.class] || CFGetTypeID((__bridge CFTypeRef)value) == CFBooleanGetTypeID()) return NO;
    double number = [value doubleValue];
    return number >= lower && number <= upper && floor(number) == number;
}

static NSString *PreviewPath(id selection) {
    if (![selection isKindOfClass:NSArray.class]) return nil;
    NSArray *nodes = selection;
    if (!nodes.count || nodes.count > PreviewMaxDepth) return nil;
    NSArray<NSString *> *tags = PreviewTags();
    NSMutableArray<NSString *> *path = [NSMutableArray arrayWithCapacity:nodes.count];
    for (id node in nodes) {
        if (![node isKindOfClass:NSArray.class] || [node count] != 2) return nil;
        NSArray *step = node;
        if (!PreviewInteger(step[0], 0, tags.count - 1) || !PreviewInteger(step[1], 1, PreviewMaxPosition)) return nil;
        [path addObject:[NSString stringWithFormat:@"%@:nth-child(%lu)",
            tags[[step[0] unsignedIntegerValue]], (unsigned long)[step[1] unsignedIntegerValue]]];
    }
    return [path componentsJoinedByString:@" > "];
}

// No persistent event handlers or message handlers, and nil frame in the call
// means WebKit's main frame. The token lives only in this isolated world/document.
static NSString *const PreviewPickerScript =
    @"if (self !== top || self.origin !== origin || location.origin !== origin) return null;"
     "if (mode === 'arm') { globalThis.miloPick = { token, document }; return true; }"
     "const pick = globalThis.miloPick;"
     "if (!pick || pick.token !== token || pick.document !== document) return null;"
     "const viewport = self.visualViewport;"
     "const xCSS = viewport.offsetLeft + x * self.innerWidth;"
     "const yCSS = viewport.offsetTop + y * self.innerHeight;"
     "let node;"
     "if (mode === 'copy') {"
     "    node = pick.target?.deref();"
     "    delete globalThis.miloPick;"
     "} else {"
     "    node = Document.prototype.elementFromPoint.call(document, xCSS, yCSS);"
     "    if (node) pick.target = new WeakRef(node);"
     "}"
     "if (!node?.isConnected) return null;"
     "const nodes = [];"
     "while (node) {"
     "    if (nodes.length === maxDepth) return null;"
     "    let position = 1;"
     "    for (let sibling = node.previousElementSibling; sibling; sibling = sibling.previousElementSibling)"
     "        if (++position > maxPosition) return null;"
     "    nodes.push([Math.max(0, tags.indexOf(node.localName)), position]);"
     "    if (node === document.documentElement) return nodes.reverse();"
     "    node = node.parentElement;"
     "}"
     "return null;";

@interface MiloPickerSurface : NSView
@property (nonatomic, copy) void (^pick)(NSPoint point);
@property (nonatomic, copy) void (^cancel)(void);
@end

@implementation MiloPickerSurface
- (BOOL)isFlipped { return YES; }
- (BOOL)acceptsFirstResponder { return YES; }
- (BOOL)needsPanelToBecomeKey { return YES; }
- (void)resetCursorRects { [self addCursorRect:self.bounds cursor:NSCursor.crosshairCursor]; }
- (void)mouseDown:(NSEvent *)event {
    NSPoint point = [self convertPoint:event.locationInWindow fromView:nil];
    NSRect bounds = self.bounds;
    if (event.window == self.window && NSMouseInRect(point, bounds, YES)) {
        self.pick(NSMakePoint(point.x / NSWidth(bounds), point.y / NSHeight(bounds)));
    }
}
- (void)cancelOperation:(id)sender { self.cancel(); }
@end

@interface MiloPreviewButton : NSButton
@end

@implementation MiloPreviewButton
- (BOOL)needsPanelToBecomeKey { return YES; }
@end

@interface MiloPreviewWebView : WKWebView
@end

@implementation MiloPreviewWebView
- (BOOL)needsPanelToBecomeKey { return YES; }
- (NSMenu *)menuForEvent:(NSEvent *)event { return nil; }
- (void)rightMouseDown:(NSEvent *)event {}
- (void)showContextMenuForSelection:(id)sender {}
- (void)mouseDown:(NSEvent *)event {
    if (!(event.modifierFlags & NSEventModifierFlagControl)) [super mouseDown:event];
}
- (BOOL)performKeyEquivalent:(NSEvent *)event {
    // The accessory helper has no Edit menu. Handle only the focused page's
    // standard editing commands through AppKit's normal responder actions.
    NSEventModifierFlags modifiers = event.modifierFlags &
        (NSEventModifierFlagCommand | NSEventModifierFlagOption | NSEventModifierFlagControl | NSEventModifierFlagShift);
    if (self.window.isKeyWindow && self.window.firstResponder == self) {
        NSString *key = event.charactersIgnoringModifiers.lowercaseString;
        if ([key isEqualToString:@"z"]) {
            if (modifiers == NSEventModifierFlagCommand) {
                [self.undoManager undo];
                return YES;
            }
            if (modifiers == (NSEventModifierFlagCommand | NSEventModifierFlagShift)) {
                [self.undoManager redo];
                return YES;
            }
        }
        if (modifiers == NSEventModifierFlagCommand) {
            SEL action = NULL;
            if ([key isEqualToString:@"a"]) action = @selector(selectAll:);
            else if ([key isEqualToString:@"c"]) action = @selector(copy:);
            else if ([key isEqualToString:@"v"]) action = @selector(paste:);
            else if ([key isEqualToString:@"x"]) action = @selector(cut:);
            if (action && [NSApp sendAction:action to:self from:self]) return YES;
        }
    }
    return [super performKeyEquivalent:event];
}
@end

typedef NS_ENUM(NSUInteger, PreviewPickState) {
    PickIdle, PickArming, PickArmed, PickReading, PickReview, PickCopying,
};

@interface MiloPreviewBrowser : NSView <WKNavigationDelegate, WKUIDelegate> {
    NSURL *_initialURL;
    NSString *_origin;
    void (^_returnFocus)(void);
    WKWebView *_web;
    WKContentWorld *_pickerWorld;
    WKNavigation *_navigation;
    WKBackForwardListItem *_committedItem;
    BOOL _policyCancelled;
    BOOL _rulesReady;
    BOOL _pageCommitted;
    BOOL _observingWeb;

    NSBox *_separator;
    NSTextField *_address;
    NSTextField *_status;
    NSButton *_back;
    NSButton *_reload;
    NSButton *_select;
    NSButton *_terminal;
    NSButton *_cancel;
    NSButton *_copy;
    NSScrollView *_review;
    NSTextView *_path;
    MiloPickerSurface *_picker;
    PreviewPickState _pickState;
    NSUInteger _pickID;
    NSURL *_pickURL;
}
- (instancetype)initWithURL:(NSURL *)url origin:(NSString *)origin returnFocus:(void (^)(void))returnFocus;
@end

@implementation MiloPreviewBrowser

- (BOOL)isFlipped { return YES; }

- (BOOL)isOpaque { return YES; }

- (void)drawRect:(NSRect)dirtyRect {
    [NSColor.windowBackgroundColor setFill];
    NSRectFill(dirtyRect);
}

- (NSButton *)buttonWithSymbol:(NSString *)symbol title:(NSString *)title action:(SEL)action {
    NSImage *image = [NSImage imageWithSystemSymbolName:symbol accessibilityDescription:title];
    NSButton *button = [[MiloPreviewButton alloc] initWithFrame:NSZeroRect];
    button.image = image;
    button.imagePosition = NSImageOnly;
    button.target = self;
    button.action = action;
    button.bezelStyle = NSBezelStyleToolbar;
    button.controlSize = NSControlSizeSmall;
    button.toolTip = title;
    [button setAccessibilityLabel:title];
    [self addSubview:button];
    return button;
}

- (instancetype)initWithURL:(NSURL *)url origin:(NSString *)origin returnFocus:(void (^)(void))returnFocus {
    if (!(self = [super initWithFrame:NSZeroRect])) return nil;
    _initialURL = url;
    _origin = origin;
    _returnFocus = [returnFocus copy];
    _pickerWorld = [WKContentWorld worldWithName:@"Milo Element Picker"];

    WKWebViewConfiguration *configuration = [WKWebViewConfiguration new];
    configuration.websiteDataStore = WKWebsiteDataStore.nonPersistentDataStore;
    configuration.preferences.javaScriptCanOpenWindowsAutomatically = NO;
    configuration.preferences.elementFullscreenEnabled = NO;
    _web = [[MiloPreviewWebView alloc] initWithFrame:NSZeroRect configuration:configuration];
    _web.navigationDelegate = self;
    _web.UIDelegate = self;
    _web.allowsLinkPreview = NO;
    [self addSubview:_web];

    _picker = [[MiloPickerSurface alloc] initWithFrame:NSZeroRect];
    _picker.hidden = YES;
    __weak typeof(self) weakSelf = self;
    _picker.pick = ^(NSPoint point) { [weakSelf pickAt:point]; };
    _picker.cancel = ^{ [weakSelf cancelPick:nil]; };
    [self addSubview:_picker];

    _back = [self buttonWithSymbol:@"chevron.left" title:@"Go back" action:@selector(goBack:)];
    _reload = [self buttonWithSymbol:@"arrow.clockwise" title:@"Reload preview" action:@selector(reload:)];
    _reload.keyEquivalent = @"r";
    _reload.keyEquivalentModifierMask = NSEventModifierFlagCommand;
    _select = [self buttonWithSymbol:@"scope" title:@"Select an element" action:@selector(armPick:)];
    _terminal = [self buttonWithSymbol:@"terminal" title:@"Return to terminal" action:@selector(returnToTerminal:)];

    _address = [NSTextField labelWithString:origin];
    _address.font = [NSFont monospacedSystemFontOfSize:11 weight:NSFontWeightRegular];
    _address.lineBreakMode = NSLineBreakByTruncatingMiddle;
    _address.toolTip = origin;
    _address.textColor = NSColor.secondaryLabelColor;
    [self addSubview:_address];
    _separator = [NSBox new];
    _separator.boxType = NSBoxSeparator;
    [self addSubview:_separator];

    _status = [NSTextField labelWithString:@""];
    _status.font = [NSFont systemFontOfSize:11];
    _status.lineBreakMode = NSLineBreakByTruncatingTail;
    [self addSubview:_status];
    _cancel = [NSButton buttonWithTitle:@"Cancel" target:self action:@selector(cancelPick:)];
    _cancel.keyEquivalent = @"\e";
    _cancel.keyEquivalentModifierMask = 0;
    [self addSubview:_cancel];
    _copy = [NSButton buttonWithTitle:@"Copy path" target:self action:@selector(copyPick:)];
    [self addSubview:_copy];
    _review = [NSTextView scrollableTextView];
    _review.hasVerticalScroller = YES;
    _review.borderType = NSBezelBorder;
    _path = _review.documentView;
    _path.editable = NO;
    _path.selectable = NO;
    _path.font = [NSFont monospacedSystemFontOfSize:11 weight:NSFontWeightRegular];
    [_path setAccessibilityLabel:@"Element path to copy"];
    [self addSubview:_review];

    _back.nextKeyView = _reload;
    _reload.nextKeyView = _select;
    _select.nextKeyView = _terminal;
    _terminal.nextKeyView = _web;
    [_web addObserver:self forKeyPath:@"URL" options:0 context:NULL];
    [_web addObserver:self forKeyPath:@"canGoBack" options:0 context:NULL];
    _observingWeb = YES;
    [self finishPick:@"Preparing local preview…"];
    [self compileRules];
    return self;
}

- (void)dealloc {
    if (_observingWeb) {
        [_web removeObserver:self forKeyPath:@"URL" context:NULL];
        [_web removeObserver:self forKeyPath:@"canGoBack" context:NULL];
    }
    [NSNotificationCenter.defaultCenter removeObserver:self];
}

- (void)viewWillMoveToWindow:(NSWindow *)newWindow {
    [super viewWillMoveToWindow:newWindow];
    NSNotificationCenter *notifications = NSNotificationCenter.defaultCenter;
    if (self.window) {
        [notifications removeObserver:self name:NSWindowDidResignKeyNotification object:self.window];
        if (_pickState != PickIdle) [self finishPick:@""];
    }
    if (newWindow) {
        [notifications addObserver:self selector:@selector(windowLostKey:) name:NSWindowDidResignKeyNotification object:newWindow];
    }
}

- (void)windowLostKey:(NSNotification *)notification {
    if (_pickState != PickIdle) [self finishPick:@""];
}

- (void)setFrameSize:(NSSize)size {
    [super setFrameSize:size];
    self.needsLayout = YES;
}

- (void)layout {
    [super layout];
    CGFloat width = NSWidth(self.bounds);
    _back.frame = NSMakeRect(5, 5, 27, 26);
    _reload.frame = NSMakeRect(34, 5, 27, 26);
    _select.frame = NSMakeRect(MAX(63, width - 62), 5, 27, 26);
    _terminal.frame = NSMakeRect(MAX(92, width - 33), 5, 27, 26);
    _address.frame = NSMakeRect(68, 10, MAX(0, width - 136), 16);
    _separator.frame = NSMakeRect(0, 36, width, 1);

    BOOL reviewing = _pickState == PickReview || _pickState == PickCopying;
    BOOL picking = _pickState != PickIdle;
    CGFloat y = 37;
    if (_status.stringValue.length) {
        _status.frame = NSMakeRect(8, y + 8, MAX(0, width - (picking && !reviewing ? 80 : 16)), 16);
        y += 31;
    }
    if (reviewing) {
        _review.frame = NSMakeRect(8, y, MAX(0, width - 16), 72);
        _copy.frame = NSMakeRect(MAX(8, width - 92), y + 76, 86, 24);
        _cancel.frame = NSMakeRect(MAX(8, width - 160), y + 76, 65, 24);
        y += 104;
    } else {
        _cancel.frame = NSMakeRect(MAX(8, width - 71), y - 27, 65, 24);
    }
    NSRect webFrame = NSMakeRect(0, y, width, MAX(0, NSHeight(self.bounds) - y));
    _web.frame = webFrame;
    _picker.frame = webFrame;
}

- (void)compileRules {
    // The compiled rule list only contains the origin, but do not retain it across
    // runs. WebKit owns a mapped rule-list object once the callback is invoked.
    NSFileManager *files = NSFileManager.defaultManager;
    NSString *name = [@"milo-preview-" stringByAppendingString:NSUUID.UUID.UUIDString];
    NSURL *directory = [files.temporaryDirectory URLByAppendingPathComponent:name isDirectory:YES];
    NSData *rules = [NSJSONSerialization dataWithJSONObject:PreviewRules(_origin) options:0 error:NULL];
    if (!rules || ![files createDirectoryAtURL:directory withIntermediateDirectories:NO
                                    attributes:@{NSFilePosixPermissions: @0700} error:NULL]) {
        [self finishPick:@"Could not prepare local preview."];
        return;
    }
    NSString *encoded = [[NSString alloc] initWithData:rules encoding:NSUTF8StringEncoding];
    WKContentRuleListStore *store = [WKContentRuleListStore storeWithURL:directory];
    __weak typeof(self) weakSelf = self;
    [store compileContentRuleListForIdentifier:@"local-preview" encodedContentRuleList:encoded
                            completionHandler:^(WKContentRuleList *list, NSError *error) {
        [files removeItemAtURL:directory error:NULL];
        typeof(self) self = weakSelf;
        if (!self) return;
        if (!list || error) {
            [self finishPick:@"Could not prepare local preview."];
            return;
        }
        [self->_web.configuration.userContentController addContentRuleList:list];
        self->_rulesReady = YES;
        [self reload:nil];
    }];
}

- (void)updateChrome:(NSString *)status {
    _status.stringValue = status;
    _status.toolTip = status.length ? status : nil;
    _status.hidden = status.length == 0;
    BOOL reviewing = _pickState == PickReview || _pickState == PickCopying;
    _review.hidden = _copy.hidden = !reviewing;
    _copy.enabled = _pickState == PickReview;
    _cancel.hidden = _pickState == PickIdle;
    _picker.hidden = _pickState != PickArmed && _pickState != PickReading;
    _reload.enabled = _rulesReady;
    _back.enabled = _web.canGoBack;
    _select.enabled = _pageCommitted && _pickState == PickIdle;
    [self.window invalidateCursorRectsForView:_picker];
    self.needsLayout = YES;
}

- (void)finishPick:(NSString *)status {
    ++_pickID;
    _pickState = PickIdle;
    _pickURL = nil;
    _path.string = @"";
    if (self.window.firstResponder == _picker) [self.window makeFirstResponder:_web];
    [self updateChrome:status];
}

- (void)returnToTerminal:(id)sender {
    if (_pickState != PickIdle) [self finishPick:@""];
    _returnFocus();
}

- (void)goBack:(id)sender {
    [self finishPick:@""];
    if (_web.canGoBack) [_web goBack];
}

- (void)reload:(id)sender {
    if (!_rulesReady) return;
    [self finishPick:@""];
    if (![PreviewOrigin(_web.URL, NO) isEqualToString:_origin] || ![_web reload]) {
        [_web loadRequest:[NSURLRequest requestWithURL:_initialURL]];
    }
}

- (void)cancelPick:(id)sender {
    if (_pickState != PickIdle) [self finishPick:@""];
}

- (BOOL)canPick {
    return _pageCommitted && self.window.isKeyWindow && self.window.isVisible &&
        [PreviewOrigin(_web.URL, NO) isEqualToString:_origin];
}

- (void)runPicker:(NSString *)mode point:(NSPoint)point expected:(PreviewPickState)state completion:(void (^)(MiloPreviewBrowser *, id))completion {
    NSUInteger identifier = _pickID;
    NSDictionary *arguments = @{@"origin": _origin, @"mode": mode, @"token": @(identifier).stringValue,
        @"x": @(point.x), @"y": @(point.y), @"tags": PreviewTags(),
        @"maxDepth": @(PreviewMaxDepth), @"maxPosition": @(PreviewMaxPosition)};
    __weak typeof(self) weakSelf = self;
    [_web callAsyncJavaScript:PreviewPickerScript arguments:arguments inFrame:nil inContentWorld:_pickerWorld
           completionHandler:^(id result, NSError *error) {
        typeof(self) self = weakSelf;
        if (!self || self->_pickID != identifier || self->_pickState != state) return;
        if (![self canPick] || ![self->_pickURL isEqual:self->_web.URL]) {
            [self finishPick:@""];
            return;
        }
        completion(self, error ? nil : result);
    }];
}

- (void)armPick:(id)sender {
    if (![self canPick] || _pickState != PickIdle) return;
    ++_pickID;
    _pickURL = _web.URL;
    _pickState = PickArming;
    [self updateChrome:@"Preparing element picker…"];
    [self runPicker:@"arm" point:NSZeroPoint expected:PickArming completion:^(MiloPreviewBrowser *browser, id result) {
        if (![result isEqual:@YES]) {
            [browser finishPick:@"Element selection is unavailable."];
            return;
        }
        browser->_pickState = PickArmed;
        [browser updateChrome:@"Click an element to review its path."];
        [browser.window makeFirstResponder:browser->_picker];
    }];
}

- (void)pickAt:(NSPoint)point {
    if (_pickState != PickArmed) return;
    if (![self canPick] || ![_pickURL isEqual:_web.URL]) {
        [self finishPick:@""];
        return;
    }
    _pickState = PickReading;
    [self updateChrome:@"Reading element path…"];
    [self runPicker:@"hit" point:point expected:PickReading completion:^(MiloPreviewBrowser *browser, id result) {
        NSString *path = PreviewPath(result);
        if (!path) {
            [browser finishPick:@"No element path available at that point."];
            return;
        }
        browser->_path.string = path;
        [browser->_path scrollRangeToVisible:NSMakeRange(0, 0)];
        browser->_pickState = PickReview;
        [browser updateChrome:@"Review the path before copying."];
        // Keep keyboard focus on Cancel, not on the clipboard action.
        [browser.window makeFirstResponder:browser->_cancel];
    }];
}

- (void)copyPick:(id)sender {
    if (_pickState != PickReview) return;
    if (![self canPick] || ![_pickURL isEqual:_web.URL]) {
        [self finishPick:@""];
        return;
    }
    _pickState = PickCopying;
    [self updateChrome:@"Review the path before copying."];
    [self runPicker:@"copy" point:NSZeroPoint expected:PickCopying completion:^(MiloPreviewBrowser *browser, id result) {
        if (![PreviewPath(result) isEqualToString:browser->_path.string]) {
            [browser finishPick:@"The page changed. Select the element again."];
            return;
        }
        NSPasteboard *pasteboard = NSPasteboard.generalPasteboard;
        [pasteboard clearContents];
        BOOL copied = [pasteboard setString:browser->_path.string forType:NSPasteboardTypeString];
        [browser finishPick:copied ? @"Copied element path." : @"Could not copy element path."];
    }];
}

- (void)observeValueForKeyPath:(NSString *)keyPath ofObject:(id)object change:(NSDictionary *)change context:(void *)context {
    if (object == _web) {
        if ([keyPath isEqualToString:@"URL"] && _pickState != PickIdle) [self finishPick:@""];
        if ([keyPath isEqualToString:@"canGoBack"]) _back.enabled = _web.canGoBack;
        return;
    }
    [super observeValueForKeyPath:keyPath ofObject:object change:change context:context];
}

- (void)webView:(WKWebView *)webView decidePolicyForNavigationAction:(WKNavigationAction *)action
    decisionHandler:(void (^)(WKNavigationActionPolicy))decisionHandler {
    if (action.targetFrame.isMainFrame && _pickState != PickIdle) [self finishPick:@""];
    BOOL allowed = _rulesReady && action.targetFrame && !action.shouldPerformDownload &&
        [PreviewOrigin(action.request.URL, NO) isEqualToString:_origin];
    if (!allowed && (!action.targetFrame || action.targetFrame.isMainFrame)) {
        if (action.targetFrame.isMainFrame) _policyCancelled = YES;
        [self finishPick:@"Blocked a navigation outside the preview policy."];
    }
    decisionHandler(allowed ? WKNavigationActionPolicyAllow : WKNavigationActionPolicyCancel);
}

- (void)webView:(WKWebView *)webView decidePolicyForNavigationResponse:(WKNavigationResponse *)response
    decisionHandler:(void (^)(WKNavigationResponsePolicy))decisionHandler {
    NSURLResponse *resource = response.response;
    NSString *disposition = [resource isKindOfClass:NSHTTPURLResponse.class] ?
        [(NSHTTPURLResponse *)resource valueForHTTPHeaderField:@"Content-Disposition"] : nil;
    NSString *dispositionType = [[disposition componentsSeparatedByString:@";"] firstObject];
    BOOL attachment = [[dispositionType stringByTrimmingCharactersInSet:NSCharacterSet.whitespaceCharacterSet]
        caseInsensitiveCompare:@"attachment"] == NSOrderedSame && dispositionType != nil;
    BOOL allowed = _rulesReady && !attachment && response.canShowMIMEType &&
        [PreviewOrigin(resource.URL, NO) isEqualToString:_origin];
    if (!allowed && response.isForMainFrame) {
        _policyCancelled = YES;
        [self finishPick:@"Blocked a navigation outside the preview policy."];
    }
    decisionHandler(allowed ? WKNavigationResponsePolicyAllow : WKNavigationResponsePolicyCancel);
}

- (void)webView:(WKWebView *)webView didStartProvisionalNavigation:(WKNavigation *)navigation {
    _navigation = navigation;
    _policyCancelled = NO;
    _pageCommitted = NO;
    [self finishPick:@""];
}

- (void)webView:(WKWebView *)webView didCommitNavigation:(WKNavigation *)navigation {
    if (navigation != _navigation) return;
    _pageCommitted = [PreviewOrigin(webView.URL, NO) isEqualToString:_origin];
    _committedItem = _pageCommitted ? webView.backForwardList.currentItem : nil;
    [self finishPick:@""];
}

- (void)webView:(WKWebView *)webView didFailProvisionalNavigation:(WKNavigation *)navigation withError:(NSError *)error {
    if (navigation != _navigation) return;
    // A failed provisional load does not commit a document. Restore the picker
    // only if the old history entry and URL are both still current.
    _pageCommitted = _committedItem && _committedItem == webView.backForwardList.currentItem &&
        [_committedItem.URL isEqual:webView.URL] && [PreviewOrigin(webView.URL, NO) isEqualToString:_origin];
    BOOL stopped = [error.domain isEqualToString:NSURLErrorDomain] && error.code == NSURLErrorCancelled;
    [self finishPick:_policyCancelled || stopped ?
        _status.stringValue : @"Could not load the local page. Try Reload."];
}

- (void)webView:(WKWebView *)webView didFailNavigation:(WKNavigation *)navigation withError:(NSError *)error {
    if (navigation != _navigation) return;
    _pageCommitted = NO;
    _committedItem = nil;
    [self finishPick:@"Could not load the local page. Try Reload."];
}

- (void)webViewWebContentProcessDidTerminate:(WKWebView *)webView {
    _pageCommitted = NO;
    _committedItem = nil;
    [self finishPick:@"Preview stopped. Use Reload to restart it."];
}

- (void)webView:(WKWebView *)webView didReceiveAuthenticationChallenge:(NSURLAuthenticationChallenge *)challenge
    completionHandler:(void (^)(NSURLSessionAuthChallengeDisposition, NSURLCredential *))completionHandler {
    BOOL tls = [challenge.protectionSpace.authenticationMethod isEqualToString:NSURLAuthenticationMethodServerTrust];
    completionHandler(tls ? NSURLSessionAuthChallengePerformDefaultHandling : NSURLSessionAuthChallengeCancelAuthenticationChallenge, nil);
}

- (WKWebView *)webView:(WKWebView *)webView createWebViewWithConfiguration:(WKWebViewConfiguration *)configuration
    forNavigationAction:(WKNavigationAction *)action windowFeatures:(WKWindowFeatures *)features {
    return nil;
}

- (void)webView:(WKWebView *)webView runJavaScriptAlertPanelWithMessage:(NSString *)message
    initiatedByFrame:(WKFrameInfo *)frame completionHandler:(void (^)(void))completionHandler {
    completionHandler();
}

- (void)webView:(WKWebView *)webView runJavaScriptConfirmPanelWithMessage:(NSString *)message
    initiatedByFrame:(WKFrameInfo *)frame completionHandler:(void (^)(BOOL))completionHandler {
    completionHandler(NO);
}

- (void)webView:(WKWebView *)webView runJavaScriptTextInputPanelWithPrompt:(NSString *)prompt defaultText:(NSString *)defaultText
    initiatedByFrame:(WKFrameInfo *)frame completionHandler:(void (^)(NSString *))completionHandler {
    completionHandler(nil);
}

- (void)webView:(WKWebView *)webView runOpenPanelWithParameters:(WKOpenPanelParameters *)parameters
    initiatedByFrame:(WKFrameInfo *)frame completionHandler:(void (^)(NSArray<NSURL *> *))completionHandler {
    completionHandler(nil);
}

- (void)webView:(WKWebView *)webView requestMediaCapturePermissionForOrigin:(WKSecurityOrigin *)origin
    initiatedByFrame:(WKFrameInfo *)frame type:(WKMediaCaptureType)type decisionHandler:(void (^)(WKPermissionDecision))decisionHandler {
    decisionHandler(WKPermissionDecisionDeny);
}

// This public delegate is available on macOS 27 and later. Earlier macOS
// releases use WebKit's own geolocation policy.
#if __MAC_OS_X_VERSION_MAX_ALLOWED >= 270000
- (void)webView:(WKWebView *)webView requestGeolocationPermissionForOrigin:(WKSecurityOrigin *)origin
    initiatedByFrame:(WKFrameInfo *)frame decisionHandler:(void (^)(WKPermissionDecision))decisionHandler {
    decisionHandler(WKPermissionDecisionDeny);
}
#endif

@end

NSView *MiloPreviewContent(NSURL *url, void (^returnFocus)(void)) {
    NSString *origin = PreviewOrigin(url, YES);
    return origin ? [[MiloPreviewBrowser alloc] initWithURL:url origin:origin returnFocus:returnFocus] : nil;
}
