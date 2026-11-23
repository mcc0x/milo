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
    assert(CGRectEqualToRect(left, CGRectMake(-85, -32, 779, 726)));
    NSMutableDictionary *right = [vertical mutableCopy];
    right[@"Workspace.sidebarPosition"] = @"right";
    CGRect flipped = Body(right, Full);
    assert(CGRectGetMinX(flipped) < CGRectGetMinX(left));
    assert(CGRectGetMaxX(flipped) < CGRectGetMaxX(left));
    assert(flipped.size.width == left.size.width);

    right[@"Workspace.sidebarCollapsed"] = @YES;
    CGRect collapsed = Body(right, Full);
    assert(CGRectEqualToRect(collapsed, flipped));

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
            CGRect shared = Body(prefs, tile);
            prefs[collapse] = @YES;
            assert(CGRectEqualToRect(Body(prefs, tile), shared));
            // Check against physical workspaces with both states rather than
            // treating a second call into the estimator as an independent oracle.
            for (int open = 0; open <= 1; ++open) {
                CGFloat width = open * 205;
                CGRect workspace = CGRectMake(Window.origin.x + ([side isEqualToString:@"left"] ? width : 0),
                                              Window.origin.y, Window.size.width - width, Window.size.height);
                CGRect nominal = CGRectMake(workspace.origin.x + tile.origin.x * workspace.size.width,
                                            workspace.origin.y + tile.origin.y * workspace.size.height,
                                            tile.size.width * workspace.size.width, tile.size.height * workspace.size.height);
                assert(CGRectContainsRect(nominal, shared));
            }
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
}

static void TerminalPaddingIsInsideBody(void) {
    // Padding styles terminal text, not the chrome and divider edges that
    // separate one pane's body from the surrounding controls and siblings.
    const CGRect tiles[] = {
        CGRectMake(0, 0, 0.5, 1), CGRectMake(0.5, 0, 0.5, 1),
        CGRectMake(0, 0, 1, 0.5), CGRectMake(0, 0.5, 1, 0.5),
    };
    CGRect bounds = Body(@{}, Full), bodies[4];
    assert(CGRectEqualToRect(bounds, CGRectMake(-288, 6, 976, 682)));
    for (size_t i = 0; i < sizeof(tiles) / sizeof(*tiles); ++i) {
        bodies[i] = Body(@{}, tiles[i]);
        assert(CGRectContainsRect(bounds, bodies[i]));
        assert(CGRectEqualToRect(bodies[i], Body(@{@"Workspace.terminalPaddingX": @48,
                                                  @"Workspace.terminalPaddingY": @48}, tiles[i])));
        assert(CGRectEqualToRect(bodies[i], Body(@{@"Workspace.terminalPaddingX": @0,
                                                  @"Workspace.terminalPaddingY": @17.8}, tiles[i])));
    }
    assert(CGRectGetMaxX(bodies[0]) < CGRectGetMinX(bodies[1]));
    assert(CGRectGetMaxY(bodies[2]) < CGRectGetMinY(bodies[3]));
    assert(CGRectGetMinX(bodies[0]) == CGRectGetMinX(bounds));
    assert(CGRectGetMaxX(bodies[1]) == CGRectGetMaxX(bounds));
    assert(CGRectGetMinY(bodies[2]) == CGRectGetMinY(bounds));
    assert(CGRectGetMaxY(bodies[3]) == CGRectGetMaxY(bounds));
}

static void RejectInvalidInputs(void) {
    MiloRexChrome chrome;
    assert(!MiloDecodeRexChrome(@{@"Workspace.tabStyle": @"unsupported"}, &chrome));
    assert(!MiloDecodeRexChrome(@{@"Workspace.paneHeaderVisibility": @"hidden"}, &chrome));
    assert(!MiloDecodeRexChrome(@{@"Workspace.toolbarBehavior": @"always"}, &chrome));
    assert(!MiloDecodeRexChrome(@{@"Workspace.tabStyle": @"vertical", @"Workspace.sidebarWidth": @90}, &chrome));
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
        TerminalPaddingIsInsideBody();
        RejectInvalidInputs();
    }
    return 0;
}
