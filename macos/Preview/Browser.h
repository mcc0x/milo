#import <AppKit/AppKit.h>

NS_ASSUME_NONNULL_BEGIN

// Main-thread only. The input predicate must check the anchor's current authorization.
// Returns nil if the initial URL is outside the preview policy.
NSView *_Nullable MiloPreviewContent(NSURL *url, BOOL (^acceptsPanelInput)(void), void (^returnFocus)(void));

NS_ASSUME_NONNULL_END
