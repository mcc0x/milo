#import <Foundation/Foundation.h>
#import "Anchor.h"
#include <errno.h>
#include <limits.h>
#include <stdio.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <arpa/inet.h>

enum { PinRefused = 2 }; // The Rust startup handoff recognizes this exit status.

static unsigned long Decimal(const char *argument, unsigned long limit) {
    if (!*argument || argument[strspn(argument, "0123456789")]) return 0;
    errno = 0;
    unsigned long value = strtoul(argument, NULL, 10);
    return errno || value > limit ? 0 : value;
}

static BOOL ReadExactly(void *buffer, size_t remaining) {
    char *cursor = buffer;
    while (remaining) {
        ssize_t count = read(STDIN_FILENO, cursor, remaining);
        if (count == -1 && errno == EINTR) continue;
        if (count <= 0) return NO;
        cursor += count;
        remaining -= (size_t)count;
    }
    return YES;
}

int main(int argc, const char *argv[]) {
    if (argc != 3) return EXIT_FAILURE;
    @autoreleasepool {
        if (strcmp(argv[1], "--pin-window") == 0) {
            pid_t pid = (pid_t)Decimal(argv[2], INT_MAX);
            const char *reason = NULL;
            CGWindowID pinned = milo_preview_pin_window(pid, &reason);
            if (!pinned) {
                return reason && printf("refused: %s\n", reason) >= 0 && fflush(stdout) == 0
                    ? PinRefused : EXIT_FAILURE;
            }
            if (printf("%u\n", pinned) < 0 || fflush(stdout) != 0) return EXIT_FAILURE;
            return EXIT_SUCCESS;
        }
        CGWindowID pinned = (CGWindowID)Decimal(argv[2], UINT32_MAX);
        if (!pinned || strcmp(argv[1], "--address-stdin") != 0) return EXIT_FAILURE;
        // Read only the launch frame: the observer uses the rest of the same pipe.
        char address[4097]; // src/preview.rs: URL_MAX plus the C terminator
        uint32_t size;
        if (!ReadExactly(&size, sizeof size)) return EXIT_FAILURE;
        size = ntohl(size);
        if (!size || size >= sizeof address || !ReadExactly(address, size) || memchr(address, 0, size)) return EXIT_FAILURE;
        address[size] = '\0';
        return milo_preview_run(address, pinned, STDIN_FILENO);
    }
}
