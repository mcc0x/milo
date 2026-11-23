#import "Browser.h"

#import <WebKit/WebKit.h>
#include <math.h>

static NSEventModifierFlags PreviewShortcutModifiers(NSEvent *event) {
    return event.modifierFlags &
        (NSEventModifierFlagCommand | NSEventModifierFlagOption | NSEventModifierFlagControl | NSEventModifierFlagShift);
}

// Require a network URL, not a userinfo-bearing URL, an app URL, or a file.
static NSURLComponents *PreviewWebURL(NSURL *url) {
    if (!url) return nil;
    NSURLComponents *parts = [NSURLComponents componentsWithURL:url resolvingAgainstBaseURL:NO];
    NSString *scheme = parts.scheme.lowercaseString;
    if (![scheme isEqualToString:@"https"] && ![scheme isEqualToString:@"http"]) return nil;
    if (!parts.percentEncodedHost.length || parts.percentEncodedUser || parts.percentEncodedPassword ||
        [url.absoluteString containsString:@"\\"]) return nil;
    if (parts.port && (parts.port.integerValue < 1 || parts.port.integerValue > 65535)) return nil;
    return parts;
}

static NSString *PreviewLoopbackOrigin(NSURL *url) {
    NSURLComponents *parts = PreviewWebURL(url);
    NSString *scheme = parts.scheme.lowercaseString;
    NSString *host = parts.percentEncodedHost.lowercaseString;
    if ([host isEqualToString:@"::1"]) host = @"[::1]";
    if (![host isEqualToString:@"[::1]"] && ![host isEqualToString:@"127.0.0.1"] &&
        ![host isEqualToString:@"localhost"]) return nil;
    NSInteger standardPort = [scheme isEqualToString:@"https"] ? 443 : 80;
    NSInteger port = parts.port ? parts.port.integerValue : standardPort;
    NSString *origin = [NSString stringWithFormat:@"%@://%@", scheme, host];
    return port == standardPort ? origin : [origin stringByAppendingFormat:@":%ld", (long)port];
}

static NSURL *PreviewAddressURL(NSString *text) {
    NSString *value = [text stringByTrimmingCharactersInSet:NSCharacterSet.whitespaceAndNewlineCharacterSet];
    if (!value.length || [value rangeOfCharacterFromSet:NSCharacterSet.whitespaceAndNewlineCharacterSet].location != NSNotFound ||
        [value rangeOfCharacterFromSet:NSCharacterSet.controlCharacterSet].location != NSNotFound ||
        [value containsString:@"\\"]) return nil;
    NSString *lowercase = value.lowercaseString;
    if (![lowercase hasPrefix:@"http://"] && ![lowercase hasPrefix:@"https://"]) {
        // Bare hosts use HTTPS, except explicit loopback hosts for local dev.
        // A colon may introduce only a numeric port (or belong to a bracketed IPv6 host).
        NSString *authority = [value componentsSeparatedByCharactersInSet:
            [NSCharacterSet characterSetWithCharactersInString:@"/?#"]].firstObject;
        NSString *hostPort = authority;
        if ([authority hasPrefix:@"["]) {
            NSRange end = [authority rangeOfString:@"]"];
            if (end.location == NSNotFound) return nil;
            hostPort = [authority substringFromIndex:NSMaxRange(end)];
            if (hostPort.length && ![hostPort hasPrefix:@":"]) return nil;
        }
        NSRange colon = [hostPort rangeOfString:@":"];
        if (colon.location != NSNotFound) {
            NSString *port = [hostPort substringFromIndex:NSMaxRange(colon)];
            if (!port.length || [port rangeOfCharacterFromSet:
                [[NSCharacterSet characterSetWithCharactersInString:@"0123456789"] invertedSet]].location != NSNotFound) return nil;
        }
        NSURL *local = [NSURL URLWithString:[@"http://" stringByAppendingString:value]];
        if (PreviewLoopbackOrigin(local)) return local;
        value = [@"https://" stringByAppendingString:value];
    }
    NSURL *url = [NSURL URLWithString:value];
    return PreviewWebURL(url) ? url : nil;
}

static BOOL PreviewSubframeURL(NSURL *url) {
    if (PreviewWebURL(url)) return YES;
    NSString *scheme = url.scheme.lowercaseString;
    if ([scheme isEqualToString:@"data"]) return YES;
    if ([scheme isEqualToString:@"about"])
        return [url.resourceSpecifier isEqualToString:@"blank"] || [url.resourceSpecifier isEqualToString:@"srcdoc"];
    if ([scheme isEqualToString:@"blob"])
        return PreviewWebURL([NSURL URLWithString:url.resourceSpecifier]) != nil;
    return NO;
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
        // The main document uses its own destination as the top URL. Local
        // subresources use the displayed document's top URL instead.
        @{@"trigger": @{@"url-filter": @".*", @"if-top-url": @[http]}, @"action": @{@"type": @"block"}},
        @{@"trigger": @{@"url-filter": http}, @"action": @{@"type": @"ignore-previous-rules"}},
        @{@"trigger": @{@"url-filter": ws}, @"action": @{@"type": @"ignore-previous-rules"}},
        // Embedded contents, not additional network origins.
        @{@"trigger": @{@"url-filter": @"^data:"}, @"action": @{@"type": @"ignore-previous-rules"}},
        @{@"trigger": @{@"url-filter": @"^about:"}, @"action": @{@"type": @"ignore-previous-rules"}},
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

@interface MiloPreviewAddress : NSTextField
@end

@implementation MiloPreviewAddress
- (BOOL)needsPanelToBecomeKey { return YES; }
@end

@interface MiloPreviewWebView : WKWebView
@property (nonatomic, copy) void (^noteActivation)(void);
@end

@implementation MiloPreviewWebView
- (BOOL)needsPanelToBecomeKey { return YES; }
- (NSMenu *)menuForEvent:(NSEvent *)event { return nil; }
- (void)rightMouseDown:(NSEvent *)event {}
- (void)showContextMenuForSelection:(id)sender {}
- (void)mouseDown:(NSEvent *)event {
    if (event.modifierFlags & NSEventModifierFlagControl) return;
    if (self.noteActivation) self.noteActivation();
    [super mouseDown:event];
}
- (void)keyDown:(NSEvent *)event {
    if (self.noteActivation && !PreviewShortcutModifiers(event) &&
        ([event.charactersIgnoringModifiers isEqualToString:@"\r"] || [event.charactersIgnoringModifiers isEqualToString:@"\x03"]))
        self.noteActivation();
    [super keyDown:event];
}
- (BOOL)performKeyEquivalent:(NSEvent *)event {
    // The accessory helper has no Edit menu. Handle only the focused page's
    // standard editing commands through AppKit's normal responder actions.
    NSEventModifierFlags modifiers = PreviewShortcutModifiers(event);
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

enum {
    PreviewToolbarHeight = 36, PreviewAddressRowHeight = 27, PreviewWideToolbarWidth = 245,
    PreviewHistoryLimit = 100,
    PreviewToolbarInset = 5, PreviewToolbarButtonWidth = 27, PreviewCompactButtonWidth = 24, PreviewToolbarButtonGap = 2,
    PreviewToolbarMinWidth = 2 * PreviewToolbarInset + 4 * PreviewCompactButtonWidth + 3 * PreviewToolbarButtonGap,
    PreviewAddressHeight = 22, PreviewMinAddressWidth = 48, PreviewProgressSize = 14,
    PreviewPanelInset = 8, PreviewStatusHeight = 31, PreviewControlGap = 4,
    PreviewCancelWidth = 65, PreviewCopyWidth = 86, PreviewReviewButtonHeight = 24,
    PreviewReviewPairWidth = 2 * PreviewPanelInset + PreviewCancelWidth + PreviewControlGap + PreviewCopyWidth,
    PreviewMinPathHeight = 42, PreviewMaxPathHeight = 72, PreviewMinPageHeight = 24,
};

static CGFloat PreviewToolbarBottom(NSSize size) {
    if (size.height < PreviewAddressRowHeight + PreviewMinPageHeight + 1) return 0;
    if (size.width < PreviewWideToolbarWidth)
        return size.height >= PreviewToolbarHeight + PreviewAddressRowHeight + PreviewMinPageHeight + 1 ?
            PreviewToolbarHeight + PreviewAddressRowHeight : PreviewAddressRowHeight;
    return size.height >= PreviewToolbarHeight + PreviewMinPageHeight + 1 ? PreviewToolbarHeight : PreviewAddressRowHeight;
}

static CGFloat PreviewPathHeight(NSSize size) {
    CGFloat buttons = PreviewReviewButtonHeight;
    if (size.width < PreviewReviewPairWidth) buttons += PreviewControlGap + PreviewReviewButtonHeight;
    CGFloat reserved = PreviewToolbarBottom(size) + 1 + PreviewStatusHeight +
        buttons + 2 * PreviewControlGap + PreviewMinPageHeight;
    return MIN(PreviewMaxPathHeight, size.height - reserved);
}

static BOOL PreviewPickerFits(NSSize size) {
    return size.width >= PreviewToolbarMinWidth && PreviewPathHeight(size) >= PreviewMinPathHeight;
}

@interface MiloPreviewHistoryEntry : NSObject
@property (nonatomic, weak) WKWebView *web;
@property (nonatomic, strong) WKBackForwardListItem *item;
@end

@implementation MiloPreviewHistoryEntry
@end

@interface MiloPreviewBrowser : NSView <NSTextFieldDelegate, WKNavigationDelegate, WKUIDelegate> {
    NSURL *_initialURL;
    NSString *_localOrigin;
    BOOL (^_acceptsPanelInput)(void);
    void (^_returnFocus)(void);
    WKWebView *_web;
    WKWebView *_localWeb;
    WKWebView *_generalWeb;
    WKWebView *_pendingWeb;
    BOOL _localDocumentLive;
    BOOL _generalDocumentLive;
    BOOL _pickerDocument;
    NSURL *_pickerResponseURL;
    BOOL _pickerRestoring;
    NSHashTable<WKBackForwardListItem *> *_pickerItems;
    WKContentWorld *_pickerWorld;
    WKNavigation *_navigation;
    NSURL *_clientRequestURL;
    NSURL *_committedURL;
    WKBackForwardListItem *_committedItem;
    NSMutableArray<MiloPreviewHistoryEntry *> *_history;
    NSInteger _historyIndex;
    NSInteger _historyTargetIndex;
    BOOL _policyCancelled;
    BOOL _transitioning;
    WKNavigation *_authorizedLocalNavigation;
    NSTimeInterval _localActivationTime;
    NSUInteger _handoffCount;
    BOOL _rulesReady;
    BOOL _pageCommitted;

    NSBox *_separator;
    NSTextField *_address;
    NSProgressIndicator *_progress;
    BOOL _showLoading;
    NSTextField *_status;
    NSButton *_back;
    NSButton *_forward;
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
- (instancetype)initWithURL:(NSURL *)url localOrigin:(NSString *)localOrigin
          acceptsPanelInput:(BOOL (^)(void))acceptsPanelInput returnFocus:(void (^)(void))returnFocus;
- (void)focusAddress:(id)sender;
- (void)goBack:(id)sender;
- (void)goForward:(id)sender;
- (void)reload:(id)sender;
- (void)returnToTerminal:(id)sender;
- (BOOL)hasCommittedLocalPage;
- (BOOL)isStartupOrigin:(NSURL *)url;
- (void)updateNavigationControls;
- (void)displayCommittedPageInView:(WKWebView *)web;
@end

static SEL PreviewShortcutAction(NSEvent *event) {
    if (event.type != NSEventTypeKeyDown) return NULL;
    NSEventModifierFlags modifiers = PreviewShortcutModifiers(event);
    NSString *key = event.charactersIgnoringModifiers.lowercaseString;
    if (modifiers == (NSEventModifierFlagCommand | NSEventModifierFlagShift) && [key isEqualToString:@"\r"])
        return @selector(returnToTerminal:);
    if (modifiers == NSEventModifierFlagCommand) {
        if ([key isEqualToString:@"l"]) return @selector(focusAddress:);
        if ([key isEqualToString:@"r"]) return @selector(reload:);
        if ([key isEqualToString:@"["]) return @selector(goBack:);
        if ([key isEqualToString:@"]"]) return @selector(goForward:);
    }
    return NULL;
}

@implementation MiloPreviewBrowser

- (BOOL)isFlipped { return YES; }

- (BOOL)isOpaque { return YES; }

- (BOOL)performKeyEquivalent:(NSEvent *)event {
    // Handle browser shortcuts before the page or the field editor sees them.
    if (self.window.isKeyWindow && event.window == self.window) {
        SEL action = PreviewShortcutAction(event);
        if (action) {
            if (!event.isARepeat) [NSApp sendAction:action to:self from:self];
            return YES;
        }
        NSText *editor = _address.currentEditor;
        if (editor && self.window.firstResponder == editor) {
            NSEventModifierFlags modifiers = PreviewShortcutModifiers(event);
            NSString *key = event.charactersIgnoringModifiers.lowercaseString;
            if (modifiers == NSEventModifierFlagCommand) {
                if ([key isEqualToString:@"a"]) { [editor selectAll:self]; return YES; }
                if ([key isEqualToString:@"c"]) { [editor copy:self]; return YES; }
                if ([key isEqualToString:@"v"]) { [editor paste:self]; return YES; }
                if ([key isEqualToString:@"x"]) { [editor cut:self]; return YES; }
                if ([key isEqualToString:@"z"]) { [editor.undoManager undo]; return YES; }
            } else if (modifiers == (NSEventModifierFlagCommand | NSEventModifierFlagShift) && [key isEqualToString:@"z"]) {
                [editor.undoManager redo];
                return YES;
            }
        }
    }
    return [super performKeyEquivalent:event];
}

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

static NSString *const PreviewWebKeyPaths[] = {@"URL", @"canGoBack", @"canGoForward", @"loading"};

- (void)observeWebView:(WKWebView *)web {
    for (NSUInteger i = 0; i < sizeof(PreviewWebKeyPaths) / sizeof(*PreviewWebKeyPaths); ++i)
        [web addObserver:self forKeyPath:PreviewWebKeyPaths[i] options:0 context:NULL];
}

- (void)stopObservingWebView:(WKWebView *)web {
    for (NSUInteger i = 0; i < sizeof(PreviewWebKeyPaths) / sizeof(*PreviewWebKeyPaths); ++i)
        [web removeObserver:self forKeyPath:PreviewWebKeyPaths[i] context:NULL];
}

- (WKWebView *)newWebView {
    WKWebViewConfiguration *configuration = [WKWebViewConfiguration new];
    // The two modes have distinct ephemeral stores and content controllers.
    configuration.websiteDataStore = WKWebsiteDataStore.nonPersistentDataStore;
    configuration.preferences.javaScriptCanOpenWindowsAutomatically = NO;
    configuration.preferences.elementFullscreenEnabled = NO;
    WKWebView *web = [[MiloPreviewWebView alloc] initWithFrame:_web.frame configuration:configuration];
    web.navigationDelegate = self;
    web.UIDelegate = self;
    web.allowsLinkPreview = NO;
    web.underPageBackgroundColor = NSColor.windowBackgroundColor;
    web.clipsToBounds = YES;
    [self observeWebView:web];
    return web;
}

- (WKWebView *)generalWebView {
    if (!_generalWeb) {
        _generalWeb = [self newWebView];
        _generalWeb.hidden = YES;
        [self addSubview:_generalWeb positioned:NSWindowBelow relativeTo:_picker];
    }
    return _generalWeb;
}

- (instancetype)initWithURL:(NSURL *)url localOrigin:(NSString *)localOrigin
          acceptsPanelInput:(BOOL (^)(void))acceptsPanelInput returnFocus:(void (^)(void))returnFocus {
    if (!(self = [super initWithFrame:NSZeroRect])) return nil;
    _initialURL = url;
    _localOrigin = localOrigin;
    self.clipsToBounds = YES;
    _acceptsPanelInput = [acceptsPanelInput copy];
    _returnFocus = [returnFocus copy];
    _history = [NSMutableArray array];
    _historyIndex = _historyTargetIndex = -1;
    if (_localOrigin) {
        _pickerItems = [NSHashTable weakObjectsHashTable];
        _pickerWorld = [WKContentWorld worldWithName:@"Milo Element Picker"];
        _localWeb = _web = [self newWebView];
        __weak typeof(self) weakSelf = self;
        ((MiloPreviewWebView *)_localWeb).noteActivation = ^{
            typeof(self) self = weakSelf;
            if (self && self->_web == self->_localWeb && self.window.isKeyWindow && self->_acceptsPanelInput())
                self->_localActivationTime = NSProcessInfo.processInfo.systemUptime;
        };
    } else {
        _generalWeb = _web = [self newWebView];
    }
    [self addSubview:_web];

    _picker = [[MiloPickerSurface alloc] initWithFrame:NSZeroRect];
    _picker.hidden = YES;
    __weak typeof(self) weakSelf = self;
    _picker.pick = ^(NSPoint point) { [weakSelf pickAt:point]; };
    _picker.cancel = ^{ [weakSelf cancelPick:nil]; };
    [self addSubview:_picker];

    _back = [self buttonWithSymbol:@"chevron.left" title:@"Go back" action:@selector(goBack:)];
    _forward = [self buttonWithSymbol:@"chevron.right" title:@"Go forward" action:@selector(goForward:)];
    _reload = [self buttonWithSymbol:@"arrow.clockwise" title:@"Reload preview" action:@selector(reload:)];
    _select = [self buttonWithSymbol:@"scope" title:@"Select an element" action:@selector(armPick:)];
    _terminal = [self buttonWithSymbol:@"terminal" title:@"Return to terminal" action:@selector(returnToTerminal:)];
    _terminal.toolTip = @"Return to terminal (⇧⌘↩)";
    [_terminal setAccessibilityHelp:@"Keyboard shortcut: Command-Shift-Return."];

    _address = [[MiloPreviewAddress alloc] initWithFrame:NSZeroRect];
    _address.stringValue = url.absoluteString;
    _address.delegate = self;
    _address.font = [NSFont monospacedSystemFontOfSize:11 weight:NSFontWeightRegular];
    _address.editable = YES;
    _address.selectable = YES;
    _address.bezeled = YES;
    _address.bezelStyle = NSTextFieldRoundedBezel;
    _address.drawsBackground = YES;
    _address.automaticTextCompletionEnabled = NO;
    _address.cell.usesSingleLineMode = YES;
    _address.cell.scrollable = YES;
    _address.placeholderString = @"Address";
    _address.toolTip = @"Web address (⌘L)";
    [_address setAccessibilityLabel:@"Web address"];
    [_address setAccessibilityHelp:@"Command-L selects the address. Return opens it."];
    [self addSubview:_address];
    _progress = [[NSProgressIndicator alloc] initWithFrame:NSZeroRect];
    _progress.style = NSProgressIndicatorStyleSpinning;
    _progress.controlSize = NSControlSizeSmall;
    _progress.indeterminate = YES;
    _progress.displayedWhenStopped = NO;
    [_progress setAccessibilityLabel:@"Loading page"];
    [self addSubview:_progress];
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

    _back.nextKeyView = _forward;
    _forward.nextKeyView = _reload;
    _reload.nextKeyView = _address;
    _address.nextKeyView = _select;
    _select.nextKeyView = _terminal;
    _terminal.nextKeyView = _web;
    if (_localOrigin) {
        [self finishPick:@"Preparing local preview…"];
        [self compileRules];
    } else {
        _rulesReady = YES;
        [self reload:nil];
    }
    return self;
}

- (void)dealloc {
    [self stopObservingWebView:_localWeb];
    [self stopObservingWebView:_generalWeb];
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

- (void)updateSelectAvailability {
    BOOL fits = PreviewPickerFits(self.bounds.size);
    BOOL local = [self hasCommittedLocalPage];
    _select.enabled = fits && local && _pickState == PickIdle;
    _select.toolTip = !local ? @"Element selection is only available on the startup local origin." :
        fits ? @"Select an element" : @"Enlarge the preview to select an element.";
}

- (void)resizeSubviewsWithOldSize:(NSSize)oldSize {
    if (!NSEqualSizes(oldSize, self.bounds.size)) {
        // A pick is tied to the viewport in which it began. Revoke it immediately,
        // including an in-flight Copy, even if the pane grows again before layout.
        if (_pickState != PickIdle) {
            [self finishPick:@""];
        } else {
            [self updateSelectAvailability];
            self.needsLayout = YES;
        }
    }
    [super resizeSubviewsWithOldSize:oldSize];
}

- (void)layout {
    [super layout];
    NSSize size = self.bounds.size;
    CGFloat width = size.width;
    CGFloat toolbarBottom = PreviewToolbarBottom(size);
    BOOL compact = width < PreviewWideToolbarWidth;
    // Use two rows when narrow. In very short panes keep the address row, and
    // leave hidden navigation controls available through keyboard shortcuts.
    CGFloat buttonWidth = compact ? PreviewCompactButtonWidth : PreviewToolbarButtonWidth;
    CGFloat step = buttonWidth + PreviewToolbarButtonGap;
    CGFloat minimum = 2 * PreviewToolbarInset - PreviewToolbarButtonGap;
    BOOL toolbarFits = toolbarBottom >= PreviewToolbarHeight;
    _terminal.hidden = !toolbarFits || width < minimum + step;
    _reload.hidden = !toolbarFits || width < minimum + 2 * step;
    _back.hidden = !toolbarFits || width < minimum + 3 * step;
    _select.hidden = !toolbarFits || width < minimum + 4 * step;
    _forward.hidden = !toolbarFits || width < minimum + 5 * step;
    CGFloat trailing = width - PreviewToolbarInset - buttonWidth;
    _terminal.frame = _terminal.hidden ? NSZeroRect :
        NSMakeRect(trailing, PreviewToolbarInset, buttonWidth, 26);
    _select.frame = _select.hidden ? NSZeroRect :
        NSMakeRect(trailing - step, PreviewToolbarInset, buttonWidth, 26);
    CGFloat leading = PreviewToolbarInset;
    _back.frame = _back.hidden ? NSZeroRect : NSMakeRect(leading, PreviewToolbarInset, buttonWidth, 26);
    if (!_back.hidden) leading += step;
    _forward.frame = _forward.hidden ? NSZeroRect : NSMakeRect(leading, PreviewToolbarInset, buttonWidth, 26);
    if (!_forward.hidden) leading += step;
    _reload.frame = _reload.hidden ? NSZeroRect : NSMakeRect(leading, PreviewToolbarInset, buttonWidth, 26);
    if (!_reload.hidden) leading += step;

    CGFloat addressX = compact ? PreviewPanelInset : leading;
    CGFloat addressWidth = (compact ? width - PreviewPanelInset : trailing - step - PreviewToolbarButtonGap) - addressX;
    CGFloat addressY = compact ? MAX(0, toolbarBottom - PreviewAddressRowHeight + 1) : toolbarFits ? 7 : 2;
    _progress.hidden = toolbarBottom < PreviewAddressHeight ||
        addressWidth < PreviewMinAddressWidth + PreviewProgressSize + PreviewControlGap;
    if (!_progress.isHidden) {
        addressWidth -= PreviewProgressSize + PreviewControlGap;
        _progress.frame = NSMakeRect(addressX + addressWidth + PreviewControlGap,
            addressY + (PreviewAddressHeight - PreviewProgressSize) / 2, PreviewProgressSize, PreviewProgressSize);
    }
    _address.hidden = toolbarBottom < PreviewAddressHeight || addressWidth < PreviewMinAddressWidth;
    _address.frame = _address.hidden ? NSZeroRect :
        NSMakeRect(addressX, addressY, addressWidth, PreviewAddressHeight);
    _separator.frame = NSMakeRect(0, toolbarBottom, width, toolbarBottom ? 1 : 0);

    BOOL reviewing = _pickState == PickReview || _pickState == PickCopying;
    BOOL picking = _pickState != PickIdle;
    CGFloat y = toolbarBottom ? toolbarBottom + 1 : 0;
    _status.hidden = !_status.stringValue.length || size.height - y < PreviewStatusHeight + PreviewMinPageHeight ||
        width <= 2 * PreviewPanelInset;
    if (!_status.hidden) {
        CGFloat reserved = 2 * PreviewPanelInset;
        if (picking && !reviewing && !compact) reserved += PreviewCancelWidth + PreviewControlGap;
        _status.frame = NSMakeRect(PreviewPanelInset, y + 8, MAX(0, width - reserved), 16);
        y = MIN(size.height, y + PreviewStatusHeight);
    }
    if (reviewing) {
        CGFloat pathHeight = PreviewPathHeight(size);
        _review.frame = NSMakeRect(PreviewPanelInset, y, width - 2 * PreviewPanelInset, pathHeight);
        y += pathHeight + PreviewControlGap;
        CGFloat copyX = width - PreviewPanelInset - PreviewCopyWidth;
        if (width < PreviewReviewPairWidth) {
            _cancel.frame = NSMakeRect(width - PreviewPanelInset - PreviewCancelWidth, y,
                                       PreviewCancelWidth, PreviewReviewButtonHeight);
            y += PreviewReviewButtonHeight + PreviewControlGap;
        } else {
            _cancel.frame = NSMakeRect(copyX - PreviewControlGap - PreviewCancelWidth, y,
                                       PreviewCancelWidth, PreviewReviewButtonHeight);
        }
        _copy.frame = NSMakeRect(copyX, y, PreviewCopyWidth, PreviewReviewButtonHeight);
        y += PreviewReviewButtonHeight + PreviewControlGap;
    } else if (picking) {
        _cancel.frame = NSMakeRect(width - PreviewPanelInset - PreviewCancelWidth, compact ? y : y - 27,
                                   PreviewCancelWidth, PreviewReviewButtonHeight);
        if (compact) y += PreviewReviewButtonHeight + PreviewControlGap;
    }
    NSRect webFrame = NSMakeRect(0, y, width, size.height - y);
    _localWeb.frame = webFrame;
    _generalWeb.frame = webFrame;
    _picker.frame = webFrame;
}

- (void)compileRules {
    // The compiled rule list only contains the origin, but do not retain it across
    // runs. WebKit owns a mapped rule-list object once the callback is invoked.
    NSFileManager *files = NSFileManager.defaultManager;
    NSString *name = [@"milo-preview-" stringByAppendingString:NSUUID.UUID.UUIDString];
    NSURL *directory = [files.temporaryDirectory URLByAppendingPathComponent:name isDirectory:YES];
    NSData *rules = [NSJSONSerialization dataWithJSONObject:PreviewRules(_localOrigin) options:0 error:NULL];
    if (!rules || ![files createDirectoryAtURL:directory withIntermediateDirectories:NO
                                    attributes:@{NSFilePosixPermissions: @0700} error:NULL]) {
        [self finishPick:@"Could not prepare the local preview. Close and reopen it."];
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
            [self finishPick:@"Could not prepare the local preview. Close and reopen it."];
            return;
        }
        [self->_localWeb.configuration.userContentController addContentRuleList:list];
        self->_rulesReady = YES;
        [self reload:nil];
    }];
}

- (void)updateChrome:(NSString *)status {
    _status.stringValue = status;
    _status.toolTip = status.length ? status : nil;
    BOOL reviewing = _pickState == PickReview || _pickState == PickCopying;
    _review.hidden = _copy.hidden = !reviewing;
    _copy.enabled = _pickState == PickReview;
    _cancel.hidden = _pickState == PickIdle;
    _picker.hidden = _pickState != PickArmed && _pickState != PickReading;
    _reload.enabled = _rulesReady;
    _address.enabled = _rulesReady;
    [self updateNavigationControls];
    [self updateSelectAvailability];
    [self.window invalidateCursorRectsForView:_picker];
    self.needsLayout = YES;
}

- (void)finishPick:(NSString *)status {
    ++_pickID;
    _pickState = PickIdle;
    _pickURL = nil;
    _path.string = @"";
    NSResponder *responder = self.window.firstResponder;
    if (responder == _picker || responder == _cancel || responder == _copy) [self.window makeFirstResponder:_web];
    [self updateChrome:status];
}

- (void)returnToTerminal:(id)sender {
    if (_pickState != PickIdle) [self finishPick:@""];
    _returnFocus();
}

- (void)restoreAddress {
    _address.stringValue = (_committedURL ?: _initialURL).absoluteString;
}

- (void)endAddressEdit {
    if (_address.currentEditor) [self.window makeFirstResponder:_web];
    [self restoreAddress];
}

- (void)focusAddress:(id)sender {
    if (_pickState != PickIdle) [self finishPick:@""];
    [self layoutSubtreeIfNeeded];
    if (!_address.isHidden && _address.isEnabled) {
        [_address selectText:nil];
    } else if (_rulesReady) {
        NSBeep();
    }
}

- (void)controlTextDidBeginEditing:(NSNotification *)notification {
    NSTextView *editor = notification.userInfo[@"NSFieldEditor"];
    editor.continuousSpellCheckingEnabled = NO;
    editor.grammarCheckingEnabled = NO;
    editor.automaticSpellingCorrectionEnabled = NO;
    editor.automaticTextCompletionEnabled = NO;
    editor.automaticQuoteSubstitutionEnabled = NO;
    editor.automaticDashSubstitutionEnabled = NO;
    editor.automaticTextReplacementEnabled = NO;
    editor.automaticLinkDetectionEnabled = NO;
    editor.automaticDataDetectionEnabled = NO;
    if (@available(macOS 15.0, *)) editor.writingToolsBehavior = NSWritingToolsBehaviorNone;
}

- (void)controlTextDidEndEditing:(NSNotification *)notification {
    [self restoreAddress];
}

static BOOL PreviewFragmentChange(NSURL *from, NSURL *to) {
    if (!from || !to || [from isEqual:to]) return NO;
    NSURLComponents *a = PreviewWebURL(from);
    NSURLComponents *b = PreviewWebURL(to);
    if (!a || !b || [a.percentEncodedFragment isEqualToString:b.percentEncodedFragment]) return NO;
    a.fragment = nil;
    b.fragment = nil;
    return [a isEqual:b];
}

- (BOOL)isStartupOrigin:(NSURL *)url {
    return _localOrigin && [PreviewLoopbackOrigin(url) isEqualToString:_localOrigin];
}

- (void)recordHistoryItem:(WKBackForwardListItem *)item enteringView:(BOOL)enteringView {
    NSInteger target = _historyTargetIndex;
    _historyTargetIndex = -1;
    if (!item) return;
    if (target >= 0) {
        MiloPreviewHistoryEntry *entry = _history[(NSUInteger)target];
        entry.item = item;
        entry.web = _web;
        _historyIndex = target;
        return;
    }
    // WebKit can reuse its current item when a handoff visits the same URL.
    // That is a new visit in the pane, even though it isn't a new native item.
    if (!enteringView && _historyIndex >= 0) {
        for (NSInteger distance = 0; distance < (NSInteger)_history.count; ++distance) {
            NSInteger before = _historyIndex - distance;
            NSInteger after = _historyIndex + distance;
            if (before >= 0 && _history[(NSUInteger)before].item == item) { _historyIndex = before; return; }
            if (after < (NSInteger)_history.count && _history[(NSUInteger)after].item == item) { _historyIndex = after; return; }
        }
    }
    NSUInteger next = (NSUInteger)(_historyIndex + 1);
    [_history removeObjectsInRange:NSMakeRange(next, _history.count - next)];
    MiloPreviewHistoryEntry *entry = [MiloPreviewHistoryEntry new];
    entry.item = item;
    entry.web = _web;
    [_history addObject:entry];
    _historyIndex = (NSInteger)next;
    if (_history.count > PreviewHistoryLimit) {
        [_history removeObjectAtIndex:0];
        --_historyIndex;
    }
}

- (void)updateNavigationControls {
    _back.enabled = !_pendingWeb && _historyIndex > 0;
    _forward.enabled = !_pendingWeb && _historyIndex + 1 < (NSInteger)_history.count;
    BOOL loading = _pendingWeb != nil;
    if (_showLoading != loading) {
        _showLoading = loading;
        if (loading) [_progress startAnimation:nil];
        else [_progress stopAnimation:nil];
        _address.toolTip = loading ? @"Loading… Address shows the current page." : @"Web address (⌘L)";
    }
}

- (void)displayCommittedPageInView:(WKWebView *)web {
    BOOL enteringView = web != _web;
    if (enteringView) {
        BOOL focusedPage = self.window.firstResponder == _web && !_address.currentEditor;
        _web.hidden = YES;
        _web = web;
        _web.hidden = NO;
        _terminal.nextKeyView = _web;
        if (focusedPage && self.window.isKeyWindow) [self.window makeFirstResponder:_web];
    }
    _committedURL = web.URL;
    _committedItem = web.backForwardList.currentItem;
    _pageCommitted = YES;
    _pendingWeb = nil;
    _clientRequestURL = nil;
    _transitioning = NO;
    _authorizedLocalNavigation = nil;
    _handoffCount = 0;
    _localActivationTime = 0;
    [self recordHistoryItem:_committedItem enteringView:enteringView];
    if (!_address.currentEditor) [self restoreAddress];
    [self finishPick:@""];
}

- (void)beginLoadInView:(WKWebView *)web {
    WKWebView *previous = _pendingWeb;
    _pendingWeb = web;
    _navigation = nil;
    _authorizedLocalNavigation = nil;
    _policyCancelled = NO;
    if (previous && previous != web) [previous stopLoading];
}

- (void)loadRequest:(NSURLRequest *)request inWebView:(WKWebView *)web {
    [self beginLoadInView:web];
    _clientRequestURL = request.URL;
    _transitioning = web != _web || !PreviewFragmentChange(_committedURL, request.URL);
    [self finishPick:@""];
    _navigation = [web loadRequest:request];
    if (web == _localWeb) _authorizedLocalNavigation = _navigation;
}

- (void)loadAddress:(NSURL *)url method:(NSString *)method inWebView:(WKWebView *)web {
    // A fresh API load does not open universal links. Across the local
    // boundary never carry headers or a body from the privileged request.
    NSMutableURLRequest *request = [NSMutableURLRequest requestWithURL:url];
    if (![method isEqualToString:@"GET"]) request.HTTPMethod = method;
    [self loadRequest:request inWebView:web];
}

- (BOOL)control:(NSControl *)control textView:(NSTextView *)textView doCommandBySelector:(SEL)command {
    if (command == @selector(cancelOperation:)) {
        [self endAddressEdit];
        return YES;
    }
    if (command != @selector(insertNewline:) || textView.hasMarkedText) return NO;
    NSURL *url = PreviewAddressURL(textView.string);
    if (!url) {
        [self finishPick:@"Enter a valid HTTP or HTTPS address."];
    } else if (_rulesReady) {
        [self endAddressEdit];
        _historyTargetIndex = -1;
        _handoffCount = 0;
        // Only this explicit native submission or native history can enter the
        // trusted store. The general view may visit the URL without that authority.
        WKWebView *web = [self isStartupOrigin:url] ? _localWeb : [self generalWebView];
        [self loadAddress:url method:@"GET" inWebView:web];
    }
    return YES;
}

- (void)traverseHistoryBy:(NSInteger)offset {
    [self endAddressEdit];
    NSInteger index = _historyIndex + offset;
    while (index >= 0 && index < (NSInteger)_history.count) {
        MiloPreviewHistoryEntry *entry = _history[(NSUInteger)index];
        WKBackForwardListItem *item = entry.item;
        WKWebView *web = entry.web;
        WKBackForwardList *list = web.backForwardList;
        BOOL alreadyThere = item == list.currentItem;
        if (alreadyThere || [list.backList containsObject:item] || [list.forwardList containsObject:item]) {
            [self beginLoadInView:web];
            _historyTargetIndex = index;
            BOOL live = web == _localWeb ? _localDocumentLive : _generalDocumentLive;
            if (alreadyThere && live && [item.URL isEqual:web.URL]) {
                // Switching to a retained view is still a transition. No
                // picker session may be reused when the view comes back.
                [self displayCommittedPageInView:web];
            } else {
                _transitioning = web != _web || alreadyThere;
                [self finishPick:@""];
                _navigation = alreadyThere ? [web reload] : [web goToBackForwardListItem:item];
            }
            return;
        }
        // WebKit can evict old items or discard a forward branch in either view.
        [_history removeObjectAtIndex:(NSUInteger)index];
        if (offset < 0) { --_historyIndex; --index; }
    }
    [self updateNavigationControls];
}

- (void)goBack:(id)sender {
    if (_back.isEnabled) [self traverseHistoryBy:-1];
}

- (void)goForward:(id)sender {
    if (_forward.isEnabled) [self traverseHistoryBy:1];
}

- (void)reload:(id)sender {
    if (!_rulesReady) return;
    [self endAddressEdit];
    _handoffCount = 0;
    _historyTargetIndex = -1;
    [self beginLoadInView:_web];
    _transitioning = YES;
    [self finishPick:@""];
    _navigation = _committedURL ? [_web reload] : nil;
    if (_web == _localWeb) _authorizedLocalNavigation = _navigation;
    if (!_navigation) [self loadAddress:_committedURL ?: _initialURL method:@"GET" inWebView:_web];
}

- (BOOL)hasCommittedLocalPage {
    return _pageCommitted && _pickerDocument && !_transitioning && _localWeb && _web == _localWeb &&
        [_committedURL isEqual:_localWeb.URL] && [self isStartupOrigin:_committedURL];
}

- (void)cancelPick:(id)sender {
    if (_pickState != PickIdle) [self finishPick:@""];
}

- (BOOL)canPick {
    return [self hasCommittedLocalPage] && PreviewPickerFits(self.bounds.size) &&
        self.window.isKeyWindow && self.window.isVisible && _acceptsPanelInput();
}

- (void)runPicker:(NSString *)mode point:(NSPoint)point expected:(PreviewPickState)state completion:(void (^)(MiloPreviewBrowser *, id))completion {
    if (_pickState != state || ![_pickURL isEqual:_web.URL] || ![self canPick]) {
        [self finishPick:@""];
        return;
    }
    NSUInteger identifier = _pickID;
    NSDictionary *arguments = @{@"origin": _localOrigin, @"mode": mode, @"token": @(identifier).stringValue,
        @"x": @(point.x), @"y": @(point.y), @"tags": PreviewTags(),
        @"maxDepth": @(PreviewMaxDepth), @"maxPosition": @(PreviewMaxPosition)};
    __weak typeof(self) weakSelf = self;
    // Only the origin-locked WebView may ever receive this IPC, even if the
    // pane switches to its general WebView before WebKit runs the request.
    [_localWeb callAsyncJavaScript:PreviewPickerScript arguments:arguments inFrame:nil inContentWorld:_pickerWorld
           completionHandler:^(id result, NSError *error) {
        typeof(self) self = weakSelf;
        if (!self || self->_pickID != identifier || self->_pickState != state) return;
        // The anchor can revoke panel input while WebKit is resolving the request.
        if (![self->_pickURL isEqual:self->_web.URL] || ![self canPick]) {
            [self finishPick:@""];
            return;
        }
        completion(self, error ? nil : result);
    }];
}

- (void)armPick:(id)sender {
    if (_pickState != PickIdle || ![self canPick]) return;
    ++_pickID;
    _pickURL = _web.URL;
    _pickState = PickArming;
    [self updateChrome:@"Preparing picker…"];
    [self runPicker:@"arm" point:NSZeroPoint expected:PickArming completion:^(MiloPreviewBrowser *browser, id result) {
        if (![result isEqual:@YES]) {
            [browser finishPick:@"Element selection is unavailable."];
            return;
        }
        browser->_pickState = PickArmed;
        [browser updateChrome:@"Click an element."];
        [browser.window makeFirstResponder:browser->_picker];
    }];
}

- (void)pickAt:(NSPoint)point {
    if (_pickState != PickArmed) return;
    if (![_pickURL isEqual:_web.URL] || ![self canPick]) {
        [self finishPick:@""];
        return;
    }
    _pickState = PickReading;
    [self updateChrome:@"Reading path…"];
    [self runPicker:@"hit" point:point expected:PickReading completion:^(MiloPreviewBrowser *browser, id result) {
        NSString *path = PreviewPath(result);
        if (!path) {
            [browser finishPick:@"No element path available at that point."];
            return;
        }
        browser->_path.string = path;
        browser->_pickState = PickReview;
        [browser updateChrome:@"Review element path"];
        [browser layoutSubtreeIfNeeded];
        [browser->_path scrollRangeToVisible:NSMakeRange(0, 0)];
        // Keep keyboard focus on Cancel, not on the clipboard action.
        [browser.window makeFirstResponder:browser->_cancel];
    }];
}

- (void)copyPick:(id)sender {
    if (_pickState != PickReview) return;
    if (![_pickURL isEqual:_web.URL] || ![self canPick]) {
        [self finishPick:@""];
        return;
    }
    _pickState = PickCopying;
    [self updateChrome:@"Review element path"];
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
    if (object == _localWeb || object == _generalWeb) {
        if (object != _web) return;
        if ([keyPath isEqualToString:@"URL"] && _pickState != PickIdle) [self finishPick:@""];
        NSURL *url = _web.URL;
        WKBackForwardListItem *item = _web.backForwardList.currentItem;
        // Same-document history has no commit callback. Never establish
        // authority here; only track a document already committed and settled.
        if (!_transitioning && !_web.isLoading && _pageCommitted && PreviewWebURL(url) &&
            [item.URL isEqual:url] && (item != _committedItem || ![_committedURL isEqual:url])) {
            _committedItem = item;
            _committedURL = url;
            if (_web == _localWeb && _pickerDocument && item) [_pickerItems addObject:item];
            _pendingWeb = nil;
            _navigation = nil;
            _clientRequestURL = nil;
            _authorizedLocalNavigation = nil;
            [self recordHistoryItem:item enteringView:NO];
            if (!_address.currentEditor) [self restoreAddress];
        }
        [self updateNavigationControls];
        [self updateSelectAvailability];
        return;
    }
    [super observeValueForKeyPath:keyPath ofObject:object change:change context:context];
}

- (void)webView:(WKWebView *)webView decidePolicyForNavigationAction:(WKNavigationAction *)action
    decisionHandler:(void (^)(WKNavigationActionPolicy))decisionHandler {
    NSURL *url = action.request.URL;
    BOOL mainFrame = action.targetFrame.isMainFrame;
    BOOL visibleOrPending = webView == (_pendingWeb ?: _web);
    BOOL localView = webView == _localWeb;
    BOOL localTarget = [self isStartupOrigin:url];
    // Public WK actions do not distinguish a synthetic link click from an
    // explicit target=_blank activation. Do not create or repurpose a window.
    BOOL allowed = _rulesReady && action.targetFrame && !action.shouldPerformDownload &&
        (mainFrame ? visibleOrPending && PreviewWebURL(url) != nil :
            localView ? localTarget : PreviewSubframeURL(url));
    if (!allowed) {
        if (visibleOrPending && (mainFrame || !action.targetFrame)) {
            if (mainFrame) _policyCancelled = YES;
            NSString *status = !action.targetFrame ? @"Pop-up windows are disabled." :
                action.shouldPerformDownload ? @"Downloads are disabled." : @"Only HTTP and HTTPS addresses can be opened.";
            [self finishPick:status];
        }
        decisionHandler(WKNavigationActionPolicyCancel);
        return;
    }
    if (!mainFrame) {
        decisionHandler(WKNavigationActionPolicyAllow);
        return;
    }

    BOOL clientLoad = [_clientRequestURL isEqual:url];
    WKNavigation *actionNavigation = nil;
#if __MAC_OS_X_VERSION_MAX_ALLOWED >= 270000
    if (@available(macOS 27.0, *)) {
        actionNavigation = action.mainFrameNavigation;
        if (_clientRequestURL && _navigation && actionNavigation)
            clientLoad = actionNavigation == _navigation;
    }
#endif
    if (clientLoad) _clientRequestURL = nil;
    BOOL history = action.navigationType == WKNavigationTypeBackForward;
    BOOL clicked = action.navigationType == WKNavigationTypeLinkActivated && action.sourceFrame.isMainFrame;
    BOOL leavesLocal = localView && !localTarget;
    NSURL *current = webView.backForwardList.currentItem.URL;
    BOOL changesHost = current && [current.host caseInsensitiveCompare:url.host] != NSOrderedSame;
    BOOL activated = NO;
    if (localView && clicked) {
        // LinkActivated also includes programmatic clicks. Consume one recent
        // native interaction, and bind redirect consent to this navigation ID.
        activated = _localActivationTime > 0 && NSProcessInfo.processInfo.systemUptime - _localActivationTime <= 1;
        if (activated) _authorizedLocalNavigation = actionNavigation;
        _localActivationTime = 0;
    }
    if (leavesLocal || (!clientLoad && !history && changesHost)) {
        NSURLRequest *request = action.request;
        NSString *method = request.HTTPMethod ?: @"GET";
        BOOL safeMethod = [method isEqualToString:@"GET"] || [method isEqualToString:@"HEAD"];
        BOOL authorizedDeparture = activated || (actionNavigation && actionNavigation == _authorizedLocalNavigation);
        // A public navigation ID is required to follow a local redirect without
        // a new native gesture. Earlier WebKit leaves the local document in place.
        if ((leavesLocal && (!safeMethod || !authorizedDeparture)) ||
            (!leavesLocal && !safeMethod && !request.HTTPBody) || _handoffCount >= 16) {
            _policyCancelled = YES;
            NSString *status = _handoffCount >= 16 ? @"Too many cross-site redirects." :
                !safeMethod ? @"This cross-site form cannot be sent from preview." :
                @"Open a link or enter an address to leave the local preview.";
            [self finishPick:status];
            decisionHandler(WKNavigationActionPolicyCancel);
            return;
        }
        ++_handoffCount;
        decisionHandler(WKNavigationActionPolicyCancel);
        if (leavesLocal) {
            [self loadAddress:url method:method inWebView:[self generalWebView]];
        } else {
            // Normal general-view forms keep the request body supplied by
            // WebKit, without admitting content to the local cookie store.
            [self loadRequest:request inWebView:webView];
        }
        return;
    }

    _pendingWeb = webView;
    if (localView) {
        _pickerResponseURL = nil;
        _pickerRestoring = action.navigationType == WKNavigationTypeBackForward;
    }
    if (!history && !PreviewFragmentChange(_committedURL, url)) _transitioning = YES;
    if (_pickState != PickIdle) [self finishPick:@""];
    else {
        [self updateNavigationControls];
        [self updateSelectAvailability];
    }
    decisionHandler(WKNavigationActionPolicyAllow);
}

// A URL alone does not establish document authority: CSP can assign an HTTP
// page an opaque origin. Keep sandboxed and non-HTML documents browsing-only.
static BOOL PreviewPickerResponse(NSURLResponse *resource) {
    if (![resource isKindOfClass:NSHTTPURLResponse.class]) return NO;
    NSString *mime = resource.MIMEType.lowercaseString;
    if (![mime isEqual:@"text/html"] && ![mime isEqual:@"application/xhtml+xml"]) return NO;
    NSHTTPURLResponse *http = (NSHTTPURLResponse *)resource;
    for (NSString *key in @[@"Content-Security-Policy", @"X-WebKit-CSP"]) {
        NSString *policy = [http valueForHTTPHeaderField:key];
        if ([policy rangeOfString:@"sandbox" options:NSCaseInsensitiveSearch].length) return NO;
    }
    return YES;
}

- (void)webView:(WKWebView *)webView decidePolicyForNavigationResponse:(WKNavigationResponse *)response
    decisionHandler:(void (^)(WKNavigationResponsePolicy))decisionHandler {
    NSURLResponse *resource = response.response;
    NSString *disposition = [resource isKindOfClass:NSHTTPURLResponse.class] ?
        [(NSHTTPURLResponse *)resource valueForHTTPHeaderField:@"Content-Disposition"] : nil;
    NSString *dispositionType = [[disposition componentsSeparatedByString:@";"] firstObject];
    BOOL attachment = dispositionType && [[dispositionType stringByTrimmingCharactersInSet:NSCharacterSet.whitespaceCharacterSet]
        caseInsensitiveCompare:@"attachment"] == NSOrderedSame;
    BOOL localView = webView == _localWeb;
    BOOL localTarget = [self isStartupOrigin:resource.URL];
    BOOL allowed = _rulesReady && !attachment && response.canShowMIMEType &&
        (response.isForMainFrame ? webView == _pendingWeb && (!localView || localTarget) && PreviewWebURL(resource.URL) != nil :
            localView ? localTarget : PreviewSubframeURL(resource.URL));
    if (response.isForMainFrame && localView && webView == _pendingWeb) {
        _pickerRestoring = NO;
        _pickerResponseURL = allowed && PreviewPickerResponse(resource) ? resource.URL : nil;
    }
    if (!allowed && response.isForMainFrame && webView == _pendingWeb) {
        _policyCancelled = YES;
        [self finishPick:attachment ? @"Downloads are disabled." : @"This response cannot be displayed in preview."];
    }
    decisionHandler(allowed ? WKNavigationResponsePolicyAllow : WKNavigationResponsePolicyCancel);
}

- (void)webView:(WKWebView *)webView didStartProvisionalNavigation:(WKNavigation *)navigation {
    if (webView != (_pendingWeb ?: _web)) return;
    _pendingWeb = webView;
    _navigation = navigation;
    _transitioning = YES;
    _clientRequestURL = nil;
    [self finishPick:@""];
}

- (void)webView:(WKWebView *)webView didReceiveServerRedirectForProvisionalNavigation:(WKNavigation *)navigation {
    if (webView == _pendingWeb && navigation == _navigation) {
        _transitioning = YES;
        if (_pickState != PickIdle) [self finishPick:@""];
    }
}

#if __MAC_OS_X_VERSION_MAX_ALLOWED >= 260000
- (void)webView:(WKWebView *)webView shouldGoToBackForwardListItem:(WKBackForwardListItem *)item
    willUseInstantBack:(BOOL)instant completionHandler:(void (^)(BOOL))completionHandler {
    BOOL allow = webView == (_pendingWeb ?: _web) && PreviewWebURL(item.URL) &&
        (webView != _localWeb || [self isStartupOrigin:item.URL]);
    if (allow && _pickState != PickIdle) [self finishPick:@""];
    completionHandler(allow);
}
#endif

- (void)webView:(WKWebView *)webView didCommitNavigation:(WKNavigation *)navigation {
    if (webView != (_pendingWeb ?: _web) || (_navigation && navigation != _navigation)) return;
    BOOL local = webView == _localWeb;
    if (!PreviewWebURL(webView.URL) || (local && ![self isStartupOrigin:webView.URL])) {
        // The two policy callbacks prevent this; do not display or authorize
        // a document whose origin does not match the view's role.
        [webView stopLoading];
        _pendingWeb = nil;
        _navigation = nil;
        _pageCommitted = NO;
        [self finishPick:@"Blocked a navigation outside the preview policy."];
        return;
    }
    if (local) {
        WKBackForwardListItem *item = webView.backForwardList.currentItem;
        _pickerDocument = [_pickerResponseURL isEqual:webView.URL] ||
            (_pickerResponseURL && PreviewFragmentChange(_pickerResponseURL, webView.URL)) ||
            (_pickerRestoring && item && [_pickerItems containsObject:item]);
        if (item) {
            if (_pickerDocument) [_pickerItems addObject:item];
            else [_pickerItems removeObject:item];
        }
        _pickerResponseURL = nil;
        _pickerRestoring = NO;
        _localDocumentLive = YES;
    } else _generalDocumentLive = YES;
    [self displayCommittedPageInView:webView];
}

- (void)webView:(WKWebView *)webView didFinishNavigation:(WKNavigation *)navigation {
    if (webView == _web && !_pendingWeb && navigation == _navigation) _navigation = nil;
}

- (void)webView:(WKWebView *)webView didFailProvisionalNavigation:(WKNavigation *)navigation withError:(NSError *)error {
    if (webView != _pendingWeb || navigation != _navigation) return;
    _pendingWeb = nil;
    _navigation = nil;
    _clientRequestURL = nil;
    _transitioning = NO;
    _authorizedLocalNavigation = nil;
    _historyTargetIndex = -1;
    _handoffCount = 0;
    // A failed provisional load never grants authority to a new document.
    _pageCommitted = _pageCommitted && _committedItem && _committedItem == _web.backForwardList.currentItem &&
        [_committedURL isEqual:_web.URL];
    BOOL stopped = [error.domain isEqualToString:NSURLErrorDomain] && error.code == NSURLErrorCancelled;
    if (!_address.currentEditor) [self restoreAddress];
    [self finishPick:_policyCancelled || stopped ? _status.stringValue : @"Could not load the page."];
}

- (void)webView:(WKWebView *)webView didFailNavigation:(WKNavigation *)navigation withError:(NSError *)error {
    if (webView != _web || navigation != _navigation) return;
    if (webView == _localWeb) _localDocumentLive = NO;
    else _generalDocumentLive = NO;
    _navigation = nil;
    _pageCommitted = _transitioning = NO;
    _authorizedLocalNavigation = nil;
    _committedItem = nil;
    if (!_address.currentEditor) [self restoreAddress];
    [self finishPick:@"Could not load the page."];
}

- (void)webViewWebContentProcessDidTerminate:(WKWebView *)webView {
    if (webView == _localWeb) {
        _localDocumentLive = _pickerDocument = _pickerRestoring = NO;
        _pickerResponseURL = nil;
        [_pickerItems removeAllObjects];
    } else _generalDocumentLive = NO;
    if (webView != _web && webView != _pendingWeb) return;
    if (webView == _pendingWeb) _pendingWeb = nil;
    if (webView == _web) {
        _pageCommitted = NO;
        _committedItem = nil;
    }
    _navigation = nil;
    _clientRequestURL = nil;
    _transitioning = NO;
    _authorizedLocalNavigation = nil;
    _historyTargetIndex = -1;
    _handoffCount = 0;
    if (!_address.currentEditor) [self restoreAddress];
    [self finishPick:@"Preview stopped. Use Reload to restart it."];
}

- (void)webView:(WKWebView *)webView didReceiveAuthenticationChallenge:(NSURLAuthenticationChallenge *)challenge
    completionHandler:(void (^)(NSURLSessionAuthChallengeDisposition, NSURLCredential *))completionHandler {
    BOOL tls = [challenge.protectionSpace.authenticationMethod isEqualToString:NSURLAuthenticationMethodServerTrust];
    completionHandler(tls ? NSURLSessionAuthChallengePerformDefaultHandling : NSURLSessionAuthChallengeCancelAuthenticationChallenge, nil);
}

- (void)webView:(WKWebView *)webView navigationAction:(WKNavigationAction *)action didBecomeDownload:(WKDownload *)download {
    [download cancel:nil];
}

- (void)webView:(WKWebView *)webView navigationResponse:(WKNavigationResponse *)response didBecomeDownload:(WKDownload *)download {
    [download cancel:nil];
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

NSView *MiloPreviewContent(NSURL *url, BOOL (^acceptsPanelInput)(void), void (^returnFocus)(void)) {
    NSURLComponents *parts = PreviewWebURL(url);
    if (!parts || parts.percentEncodedQuery || parts.percentEncodedFragment) return nil;
    return [[MiloPreviewBrowser alloc] initWithURL:url localOrigin:PreviewLoopbackOrigin(url)
        acceptsPanelInput:acceptsPanelInput returnFocus:returnFocus];
}

void MiloPreviewFocusRefused(NSView *content) {
    [(MiloPreviewBrowser *)content finishPick:@"Rex couldn’t return focus. Click the working terminal."];
}
