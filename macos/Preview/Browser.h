#import <AppKit/AppKit.h>

NS_ASSUME_NONNULL_BEGIN

// Main-thread only. Returns nil if the initial URL is outside the preview policy.
NSView *_Nullable MiloPreviewContent(NSURL *url, void (^returnFocus)(void));

NS_ASSUME_NONNULL_END
