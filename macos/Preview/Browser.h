#import <AppKit/AppKit.h>

NS_ASSUME_NONNULL_BEGIN

// Main-thread only. The input predicate must check the anchor's current authorization.
// The initial URL must be HTTP(S), without a query, fragment or credentials.
// Only a literal loopback startup origin can enable the opt-in element picker.
NSView *_Nullable MiloPreviewContent(NSURL *url, BOOL (^acceptsPanelInput)(void), void (^returnFocus)(void));
// Sets feedback in the browser controls only. Does not reveal a concealed pane.
void MiloPreviewFocusRefused(NSView *content);

NS_ASSUME_NONNULL_END
