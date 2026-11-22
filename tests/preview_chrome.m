// macOS, no GUI or persistent preferences:
// xcrun clang -fobjc-arc tests/preview_chrome.m -framework AppKit -framework CoreGraphics -o /tmp/preview-chrome
// /tmp/preview-chrome
#import "../macos/Preview/Chrome.h"
#import <assert.h>

static const CGRect Window = {{-300, -100}, {1000, 800}};
static const CGRect Full = {{0, 0}, {1, 1}};

static CGRect Body(NSDictionary *prefs, CGRect normalized) {
    MiloRexChrome chrome;
    CGRect body;
    assert(MiloDecodeRexChrome(prefs, &chrome));
    assert(MiloPaneInRexChrome(Window, normalized, &chrome, &body));
    return body;
}

static void WorkspaceLayouts(void) {
    CGRect top = Body(@{}, Full);
    CGRect bottom = Body(@{@"Workspace.tabBarEdge": @"bottom"}, Full);
    assert(CGRectGetMinY(top) > CGRectGetMinY(bottom));
    assert(CGRectGetMaxY(top) > CGRectGetMaxY(bottom));

    NSDictionary *vertical = @{@"Workspace.tabStyle": @"vertical", @"Workspace.sidebarWidth": @205,
                               @"Workspace.paneDensity": @"compact", @"Workspace.interfaceStyle": @"modern",
                               @"Workspace.preservesWindowWidthOnSidebarToggle": @NO};
    CGRect left = Body(vertical, Full);
    assert(CGRectEqualToRect(left, CGRectMake(-75, -50, 759, 738)));
    NSMutableDictionary *right = [vertical mutableCopy];
    right[@"Workspace.sidebarPosition"] = @"right";
    CGRect flipped = Body(right, Full);
    assert(CGRectGetMinX(flipped) < CGRectGetMinX(left));
    assert(CGRectGetMaxX(flipped) < CGRectGetMaxX(left));
    assert(flipped.size.width == left.size.width);

    right[@"Workspace.sidebarCollapsed"] = @YES;
    CGRect collapsed = Body(right, Full);
    assert(CGRectGetMinX(collapsed) == CGRectGetMinX(flipped));
    assert(CGRectGetMaxX(collapsed) == CGRectGetMaxX(left));

    CGRect leftSplit = Body(vertical, CGRectMake(0, 0, 0.45, 1));
    CGRect rightSplit = Body(vertical, CGRectMake(0.45, 0, 0.55, 1));
    assert(CGRectGetMaxX(leftSplit) < CGRectGetMinX(rightSplit));
    CGRect aboveSplit = Body(vertical, CGRectMake(0, 0, 1, 0.375));
    CGRect belowSplit = Body(vertical, CGRectMake(0, 0.375, 1, 0.625));
    assert(CGRectGetMaxY(aboveSplit) < CGRectGetMinY(belowSplit));
    assert(CGRectContainsRect(left, leftSplit) && CGRectContainsRect(left, rightSplit));
    assert(CGRectContainsRect(left, aboveSplit) && CGRectContainsRect(left, belowSplit));
}

static void SidebarWithoutResize(void) {
    NSString *const preserve = @"Workspace.preservesWindowWidthOnSidebarToggle";
    NSString *const collapse = @"Workspace.sidebarCollapsed";
    NSMutableDictionary *prefs = [@{@"Workspace.tabStyle": @"vertical", @"Workspace.sidebarWidth": @205,
                                    @"Workspace.paneDensity": @"compact"} mutableCopy];
    CGRect tiles[] = {Full, CGRectMake(0, 0, 0.625, 1), CGRectMake(0.625, 0, 0.375, 1),
                      CGRectMake(0, 0, 1, 0.375), CGRectMake(0, 0.375, 1, 0.625)};
    for (NSString *side in @[@"left", @"right"]) {
        prefs[@"Workspace.sidebarPosition"] = side;
        for (size_t i = 0; i < sizeof(tiles) / sizeof(*tiles); ++i) {
            CGRect tile = tiles[i];
            prefs[preserve] = @NO;
            prefs[collapse] = @NO;
            CGRect expanded = Body(prefs, tile);
            prefs[collapse] = @YES;
            CGRect collapsed = Body(prefs, tile);
            CGRect shared = CGRectIntersection(expanded, collapsed);
            assert(!CGRectIsEmpty(shared));
            prefs[preserve] = @YES;
            assert(CGRectEqualToRect(Body(prefs, tile), shared));
            prefs[collapse] = @NO;
            assert(CGRectEqualToRect(Body(prefs, tile), shared));
            [prefs removeObjectForKey:preserve];
            assert(CGRectEqualToRect(Body(prefs, tile), shared));
            prefs[preserve] = @"unknown";
            assert(CGRectEqualToRect(Body(prefs, tile), shared));

            // A normal 37.5% right tile beside a left sidebar is still wide.
            if (i == 2 && [side isEqualToString:@"left"]) assert(shared.size.width > 250);
        }
    }
}

static void ChromeBehaviors(void) {
    CGRect upper = CGRectMake(0, 0, 1, 0.5);
    CGRect always = Body(@{}, upper);
    CGRect hover = Body(@{@"Workspace.toolbarBehavior": @"onHover"}, upper);
    CGRect hidden = Body(@{@"Workspace.toolbarBehavior": @"hidden"}, upper);
    assert(CGRectGetMaxY(hover) < CGRectGetMaxY(always) && CGRectEqualToRect(hover, hidden));
    assert(CGRectEqualToRect(Body(@{@"Workspace.paneHeaderVisibility": @"onHover"}, upper), always));
    CGRect noHeader = Body(@{@"Workspace.paneHeaderVisibility": @"never"}, upper);
    assert(CGRectGetMinY(noHeader) < CGRectGetMinY(always));
    assert(CGRectEqualToRect(noHeader, Body(@{@"Workspace.showsPaneHeaders": @NO}, upper)));
    assert(CGRectEqualToRect(always, Body(@{@"Workspace.paneHeaderVisibility": @"always",
                                           @"Workspace.showsPaneHeaders": @NO}, upper)));

    CGRect padded = Body(@{@"Workspace.terminalPaddingX": @17.8, @"Workspace.terminalPaddingY": @0}, upper);
    assert(CGRectGetMinX(padded) == CGRectGetMinX(always) + 8);
    assert(CGRectGetMaxX(padded) == CGRectGetMaxX(always) - 8);
    assert(CGRectGetMinY(padded) == CGRectGetMinY(always) - 6);
}

static void RejectInvalidInputs(void) {
    MiloRexChrome chrome;
    assert(!MiloDecodeRexChrome(@{@"Workspace.tabStyle": @"unsupported"}, &chrome));
    assert(!MiloDecodeRexChrome(@{@"Workspace.paneHeaderVisibility": @"hidden"}, &chrome));
    assert(!MiloDecodeRexChrome(@{@"Workspace.toolbarBehavior": @"always"}, &chrome));
    assert(!MiloDecodeRexChrome(@{@"Workspace.terminalPaddingX": @"10"}, &chrome));
    assert(!MiloDecodeRexChrome(@{@"Workspace.terminalPaddingY": @(NAN)}, &chrome));
    assert(!MiloDecodeRexChrome(@{@"Workspace.terminalPaddingY": @49}, &chrome));
    assert(!MiloDecodeRexChrome(@{@"Workspace.tabStyle": @"vertical", @"Workspace.sidebarWidth": @90}, &chrome));
    assert(!MiloDecodeRexChrome(@{@"Workspace.tabStyle": @"vertical", @"Workspace.sidebarCollapsed": @1}, &chrome));
    assert(MiloDecodeRexChrome(@{}, &chrome));
    CGRect result = CGRectZero;
    assert(!MiloPaneInRexChrome(CGRectInfinite, Full, &chrome, &result));
    assert(!MiloPaneInRexChrome(CGRectMake(DBL_MAX, 0, 1000, 800), Full, &chrome, &result));
    assert(!MiloPaneInRexChrome(CGRectMake(0, 0, -100, 800), Full, &chrome, &result));
    assert(!MiloPaneInRexChrome(Window, CGRectMake(0, 0, INFINITY, 1), &chrome, &result));
    assert(!MiloPaneInRexChrome(Window, CGRectMake(-0.1, 0, 1, 1), &chrome, &result));
    assert(!MiloPaneInRexChrome(Window, CGRectMake(0.5, 0, 0.500001, 1), &chrome, &result));
    assert(!MiloPaneInRexChrome(Window, CGRectMake(0, 0, 1, 0.005), &chrome, &result));
    assert(!MiloPaneInRexChrome(Window, Full, &chrome, NULL));
    assert(CGRectEqualToRect(result, CGRectZero));
    assert(MiloPaneInRexChrome(Window, CGRectMake(0.5, 0, nextafter(0.5, 1), 1), &chrome, &result));
}

int main(void) {
    @autoreleasepool {
        (void)MiloApproximatePane;
        WorkspaceLayouts();
        SidebarWithoutResize();
        ChromeBehaviors();
        RejectInvalidInputs();
    }
    return 0;
}
