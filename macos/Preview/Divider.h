#ifndef MILO_PREVIEW_DIVIDER_H
#define MILO_PREVIEW_DIVIDER_H

#import <AppKit/AppKit.h>
#import <math.h>

typedef NS_ENUM(NSUInteger, DividerEdge) { DividerNone, DividerLeft, DividerRight, DividerTop, DividerBottom };

// The caller restricts p to the pinned window. Outside the inset page an
// internal edge permits only a hidden wait, never placement or input.
static DividerEdge DividerApproach(NSPoint p, NSRect body, CGRect tile) {
    if (p.y > NSMinY(body) && p.y < NSMaxY(body)) {
        if (tile.origin.x > 0 && p.x <= NSMinX(body)) return DividerLeft;
        if (CGRectGetMaxX(tile) < 0.9999995 && p.x >= NSMaxX(body)) return DividerRight;
    }
    if (p.x > NSMinX(body) && p.x < NSMaxX(body)) {
        if (tile.origin.y > 0 && p.y >= NSMaxY(body)) return DividerTop;
        if (CGRectGetMaxY(tile) < 0.9999995 && p.y <= NSMinY(body)) return DividerBottom;
    }
    return DividerNone;
}

static DividerEdge ResizedDivider(CGRect old, CGRect newTile) {
    // Rex reports edges quantized in millionths; compare those integers,
    // not floating point sums of independently decoded origins and widths.
    BOOL left = llround(CGRectGetMinX(old) * 1e6) != llround(CGRectGetMinX(newTile) * 1e6);
    BOOL right = llround(CGRectGetMaxX(old) * 1e6) != llround(CGRectGetMaxX(newTile) * 1e6);
    BOOL top = llround(CGRectGetMinY(old) * 1e6) != llround(CGRectGetMinY(newTile) * 1e6);
    BOOL bottom = llround(CGRectGetMaxY(old) * 1e6) != llround(CGRectGetMaxY(newTile) * 1e6);
    if (left + right + top + bottom != 1) return DividerNone;
    return left ? DividerLeft : right ? DividerRight : top ? DividerTop : DividerBottom;
}

#endif
