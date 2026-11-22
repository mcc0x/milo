#ifndef MILO_PREVIEW_ANCHOR_H
#define MILO_PREVIEW_ANCHOR_H

#include <CoreGraphics/CoreGraphics.h>
#include <sys/types.h>

// Returns the unique visible normal window of the frontmost local Rex process
// if its display and chrome support placement, or zero.
CGWindowID milo_preview_pin_window(pid_t pid);

// Main thread. state_fd delivers "show PID tile X Y W H\n" (integer millionths)
// or "hide\n". EOF exits; an explicit return-to-terminal writes "focus\n".
int milo_preview_run(const char *url, CGWindowID pinned_window, int state_fd);

#endif
