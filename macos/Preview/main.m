#import <Foundation/Foundation.h>
#import <ApplicationServices/ApplicationServices.h>
#import "Anchor.h"
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

int main(int argc, const char *argv[]) {
    if (argc != 2) return EXIT_FAILURE;
    // Probe without prompting or changing system settings, before Rex opens a pane.
    if (strcmp(argv[1], "--check-access") == 0) return AXIsProcessTrusted() ? 0 : 77;
    @autoreleasepool {
        // The one-time proof is printed only by the anchor, never included in a
        // window title, persisted preference, URL, or page-accessible property.
        unsigned char bytes[16];
        arc4random_buf(bytes, sizeof(bytes));
        const char hex[] = "0123456789abcdef";
        char marker[33] = {0};
        for (size_t i = 0; i < sizeof(bytes); i++) {
            marker[2*i] = hex[bytes[i] >> 4];
            marker[2*i+1] = hex[bytes[i] & 15];
        }
        return milo_preview_run(argv[1], marker, STDIN_FILENO);
    }
}
