#ifndef MILO_PREVIEW_ANCHOR_H
#define MILO_PREVIEW_ANCHOR_H

#include <CoreGraphics/CoreGraphics.h>
#include <sys/types.h>

// Returns the unique visible normal window of the frontmost local Rex process
// if its display and chrome support placement, or zero. reason receives a
// fixed non-identifying refusal on failure, NULL on success.
CGWindowID milo_preview_pin_window(pid_t pid, const char **reason);

// Main thread. The observer supplies show (PID and tile in integer
// millionths), renew, hide, and unclaimed records. Show and renew carry a
// trailing " @<unix-seconds>" for the start of that observation. EOF ends
// the preview; current eligibility and fresh placement proof are required.
int milo_preview_run(const char *url, CGWindowID pinned_window, int state_fd);

#endif
