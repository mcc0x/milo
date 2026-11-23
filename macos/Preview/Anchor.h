#ifndef MILO_PREVIEW_ANCHOR_H
#define MILO_PREVIEW_ANCHOR_H

#include <CoreGraphics/CoreGraphics.h>
#include <stdbool.h>
#include <sys/types.h>

// Returns the unique visible normal window of the frontmost local Rex process
// if its display and chrome support placement, or zero. reason receives a
// fixed non-identifying refusal on failure, NULL on success.
CGWindowID milo_preview_pin_window(pid_t pid, const char **reason);

// Main thread. state_fd carries the observer's show/guard (PID and tile in
// integer millionths), renew, hide, guard-hide and unclaimed records. EOF
// ends the preview. confirm, guard or guard-hide permanently requires local
// TTY consent. Readiness is independent of window sampling; a confirmation
// must pass a fresh placement check and expires within 12 seconds of receipt.
// A focus request on stdout needs a separate parent acknowledgment.
int milo_preview_run(const char *url, CGWindowID pinned_window, int state_fd, bool confirm);

#endif
