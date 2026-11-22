#ifndef MILO_PREVIEW_ANCHOR_H
#define MILO_PREVIEW_ANCHOR_H

// Runs on the main thread. state_fd delivers "show PID tile\n",
// "show PID float\n" (topmost float only), or "hide\n".
// EOF exits. An explicit return-to-terminal action writes "focus\n" to stdout.
// The marker is 32 lowercase hexadecimal digits from 128 random bits.
int milo_preview_run(const char *url, const char *marker, int state_fd);

#endif
