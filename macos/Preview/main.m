#import <Foundation/Foundation.h>
#import "Anchor.h"
#include <errno.h>
#include <limits.h>
#include <stdio.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

static unsigned long Decimal(const char *argument, unsigned long limit) {
    if (!*argument || argument[strspn(argument, "0123456789")]) return 0;
    errno = 0;
    unsigned long value = strtoul(argument, NULL, 10);
    return errno || value > limit ? 0 : value;
}

int main(int argc, const char *argv[]) {
    if (argc != 3) return EXIT_FAILURE;
    @autoreleasepool {
        if (strcmp(argv[1], "--pin-window") == 0) {
            pid_t pid = (pid_t)Decimal(argv[2], INT_MAX);
            CGWindowID pinned = milo_preview_pin_window(pid);
            if (!pinned || printf("%u\n", pinned) < 0 || fflush(stdout) != 0) return EXIT_FAILURE;
            return EXIT_SUCCESS;
        }
        CGWindowID pinned = (CGWindowID)Decimal(argv[2], UINT32_MAX);
        return milo_preview_run(argv[1], pinned, STDIN_FILENO);
    }
}
