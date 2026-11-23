#pragma once

#include <errno.h>
#include <fcntl.h>
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <string.h>
#include <sys/ioctl.h>
#include <termios.h>
#include <time.h>
#include <unistd.h>

typedef enum {
    ResizeInputWaiting, ResizeInputMotion, ResizeInputCode, ResizeInputDismiss, ResizeInputFailed,
} ResizeInputResult;

typedef enum { ResizeOff, ResizeProbe, ResizeMouse, ResizeCanonical } ResizeInputMode;
typedef enum { ResizeGround, ResizeEscape, ResizeCSI, ResizeString, ResizeStringEscape, ResizePaste } ResizeParseState;
typedef enum { ResizeLineEmpty, ResizeLineDigits, ResizeLineHide, ResizeLineRejected } ResizeLine;

typedef struct {
    struct termios saved;
    uint64_t probeEnd;
    double motionX, motionY; // Valid only on ResizeInputMotion; normalized cell center, y down.
    uint32_t code;
    ResizeInputMode mode;
    ResizeParseState escape;
    bool restoreModes, restoreTTY, flushInput;
    bool osc, codeMatches;
    ResizeLine line;
    unsigned replies, enabled, length, lineLength, incorrectCodes;
    char csi[21];
} ResizeInput;

static inline bool ResizeWrite(int tty, const char *text, size_t count) {
    while (count) {
        ssize_t written = write(tty, text, count);
        if (written < 0 && errno == EINTR) continue;
        if (written <= 0) return false;
        text += written;
        count -= (size_t)written;
    }
    return true;
}

static inline struct termios ResizeTTYMode(struct termios mode, bool canonical) {
    // Keep output and Ctrl+C. Disable terminal-generated stop/quit so the
    // shell cannot take over before the managed restoration path runs.
    mode.c_iflag &= ~(INLCR | IGNCR | ISTRIP | IXON | IXOFF | PARMRK);
    mode.c_iflag |= ICRNL;
    mode.c_lflag &= ~(ICANON | ECHO | ECHONL | IEXTEN | EXTPROC | PENDIN);
    mode.c_cc[VMIN] = mode.c_cc[VTIME] = 0;
    mode.c_cc[VSUSP] = mode.c_cc[VDSUSP] = mode.c_cc[VQUIT] = _POSIX_VDISABLE;
    if (canonical) {
        mode.c_lflag |= ICANON;
        mode.c_cc[VEOF] = mode.c_cc[VEOL] = mode.c_cc[VEOL2] = _POSIX_VDISABLE;
        mode.c_cc[VERASE] = mode.c_cc[VKILL] = _POSIX_VDISABLE;
    }
    return mode;
}

// Called even on revocation and partial startup. Retain failed restoration
// work so shutdown can retry without ever accepting input from this epoch.
static inline bool EndResizeInput(ResizeInput *input, int tty) {
    static const char restore[] = "\033[?1003;1006;2004r";
    input->mode = ResizeOff;
    input->code = 0;
    if (input->restoreModes && ResizeWrite(tty, restore, sizeof(restore) - 1)) input->restoreModes = false;
    if (input->restoreTTY) {
        // Darwin reprocesses queued raw input when switching to canonical
        // (PENDIN). Make that transition with echo off and flush before
        // restoring echo. TCSAFLUSH would wait for the terminal to drain.
        struct termios silent = ResizeTTYMode(input->saved, true);
        if (tcsetattr(tty, TCSANOW, &silent) == 0 && tcflush(tty, TCIFLUSH) == 0 &&
            tcsetattr(tty, TCSANOW, &input->saved) == 0) input->restoreTTY = false;
    }
    if (input->flushInput && tcflush(tty, TCIFLUSH) == 0 && !input->restoreModes && !input->restoreTTY)
        input->flushInput = false;
    return !input->restoreModes && !input->restoreTTY && !input->flushInput;
}

static inline bool ResizeTTYMatches(const ResizeInput *input, int tty) {
    struct termios actual, expected = ResizeTTYMode(input->saved, input->mode == ResizeCanonical);
    return tcgetpgrp(tty) == getpgrp() && tcgetattr(tty, &actual) == 0 &&
        // Darwin sets PENDIN when entering canonical mode and clears it on read.
        actual.c_iflag == expected.c_iflag &&
        ((actual.c_lflag ^ expected.c_lflag) & ~(input->mode == ResizeCanonical ? PENDIN : 0)) == 0 &&
        !memcmp(actual.c_cc, expected.c_cc, sizeof(actual.c_cc));
}

// The caller owns a dedicated foreground Rex PTY, calls read only while the
// current geometry/ownership proof holds, and calls end on every revocation.
// Input is routed terminal data, not a hardware or client identity assertion.
static inline bool BeginResizeInput(ResizeInput *input, int tty, uint32_t code) {
    // Nonblocking reads/writes are required even while a terminal is stalled.
    static const char save[] = "\033[?1003;1006;2004s";
    static const char enableAndCheck[] = "\033[?2004h\033[?1006h\033[?1003h"
        "\033[?2004$p\033[?1006$p\033[?1003$p";
    struct termios saved;
    if (code < 100000 || code > 999999) return false;
    int flags = fcntl(tty, F_GETFL);
    if (input->mode != ResizeOff || !EndResizeInput(input, tty) || flags < 0 || !(flags & O_NONBLOCK) ||
        tcgetpgrp(tty) != getpgrp() || tcgetattr(tty, &saved) < 0 || !(saved.c_lflag & ICANON) ||
        (saved.c_iflag & (ICRNL | IGNCR | INLCR)) != ICRNL) return false;
    *input = (ResizeInput){.saved = saved, .mode = ResizeProbe, .flushInput = true, .code = code};
    struct termios raw = ResizeTTYMode(saved, false);
    if (tcflush(tty, TCIFLUSH) < 0 || tcsetattr(tty, TCSANOW, &raw) < 0) goto failed;
    input->restoreTTY = true;
    if (!ResizeTTYMatches(input, tty)) goto failed;
    input->restoreModes = true;
    if (!ResizeWrite(tty, save, sizeof(save) - 1) ||
        !ResizeWrite(tty, enableAndCheck, sizeof(enableAndCheck) - 1)) goto failed;
    input->probeEnd = clock_gettime_nsec_np(CLOCK_MONOTONIC_RAW) + UINT64_C(500000000);
    return true;
failed:
    EndResizeInput(input, tty);
    return false;
}

static inline bool ResizeInputReady(const ResizeInput *input) {
    return input->mode == ResizeMouse || input->mode == ResizeCanonical;
}

static inline bool ResizeNumber(const char **cursor, const char *end, unsigned *number) {
    const char *p = *cursor;
    if (p == end || *p < '0' || *p > '9') return false;
    unsigned value = 0;
    do {
        value = value * 10 + (unsigned)(*p++ - '0');
        if (value > UINT16_MAX) return false;
    } while (p != end && *p >= '0' && *p <= '9');
    *cursor = p;
    *number = value;
    return true;
}

// Keep only a bounded line kind, length and match bit, not the typed digits.
static inline ResizeInputResult ResizeReadLineByte(ResizeInput *input, unsigned char byte) {
    if (byte == '\n') {
        ResizeLine line = input->line;
        unsigned length = input->lineLength;
        input->line = ResizeLineEmpty;
        if (!ResizeInputReady(input)) return ResizeInputWaiting;
        if (line == ResizeLineHide && length == 4) return ResizeInputDismiss;
        if (line == ResizeLineDigits && length == 6 && input->code) {
            if (input->codeMatches) return ResizeInputCode;
            if (++input->incorrectCodes == 3) return ResizeInputFailed;
        }
    } else if (!ResizeInputReady(input)) {
        // Never assemble a line from characters on opposite sides of arming.
        input->line = ResizeLineRejected;
    } else if (input->line == ResizeLineEmpty) {
        input->lineLength = 1;
        if (byte >= '0' && byte <= '9') {
            input->line = ResizeLineDigits;
            input->codeMatches = input->code / 100000 == (unsigned)(byte - '0');
        } else input->line = byte == 'h' ? ResizeLineHide : ResizeLineRejected;
    } else if (input->line == ResizeLineDigits && input->lineLength < 6 && byte >= '0' && byte <= '9') {
        static const unsigned divisor[] = {10000, 1000, 100, 10, 1};
        input->codeMatches = input->codeMatches &&
            input->code / divisor[input->lineLength - 1] % 10 == (unsigned)(byte - '0');
        ++input->lineLength;
    } else if (input->line == ResizeLineHide && input->lineLength < 4 &&
        byte == (unsigned char)"hide"[input->lineLength]) ++input->lineLength;
    else input->line = ResizeLineRejected;
    return ResizeInputWaiting;
}

static inline ResizeInputResult ResizeFinishCSI(ResizeInput *input, int tty) {
    const char *p = input->csi, *end = p + input->length;
    if (input->length == 4 && !memcmp(p, "200~", 4)) {
        input->line = ResizeLineRejected; // Includes text before the opener and after the trailer.
        input->escape = ResizePaste;
        input->length = 0;
        return ResizeInputWaiting;
    }
    if (input->mode == ResizeProbe && p != end && *p++ == '?') {
        unsigned mode, status;
        if (ResizeNumber(&p, end, &mode) && end - p == 4 && *p++ == ';' &&
            ResizeNumber(&p, end, &status) && end - p == 2 && !memcmp(p, "$y", 2) && status <= 4) {
            unsigned bit = mode == 1003 ? 1 : mode == 1006 ? 2 : mode == 2004 ? 4 : 0;
            input->replies |= bit;
            if (status == 1) input->enabled |= bit;
            else input->enabled &= ~bit;
            if (bit) return ResizeInputWaiting;
        }
        input->line = ResizeLineRejected;
        return ResizeInputWaiting;
    }
    if (input->mode == ResizeMouse && p != end && *p++ == '<') {
        unsigned button, x, y;
        if (!ResizeNumber(&p, end, &button) || p == end || *p++ != ';' ||
            !ResizeNumber(&p, end, &x) || p == end || *p++ != ';' ||
            !ResizeNumber(&p, end, &y) || end - p != 1 || (*p != 'M' && *p != 'm') || !x || !y) {
            input->line = ResizeLineRejected;
            return ResizeInputWaiting;
        }
        if (input->line != ResizeLineEmpty || *p != 'M' || button != 35) return ResizeInputWaiting;
        struct winsize size;
        if (ioctl(tty, TIOCGWINSZ, &size) < 0 || x > size.ws_col || y > size.ws_row) return ResizeInputWaiting;
        input->motionX = (x - 0.5) / size.ws_col;
        input->motionY = (y - 0.5) / size.ws_row;
        return ResizeInputMotion;
    }
    // Unrecognized control sequences cannot be part of an input line.
    input->line = ResizeLineRejected;
    return ResizeInputWaiting;
}

static inline ResizeInputResult ResizeReadByte(ResizeInput *input, int tty, unsigned char byte) {
    switch (input->escape) {
    case ResizePaste: {
        static const char close[] = "\033[201~";
        if (byte == (unsigned char)close[input->length]) {
            if (++input->length == sizeof(close) - 1) {
                input->escape = ResizeGround;
                input->length = 0;
            }
        } else input->length = byte == '\033';
        break;
    }
    case ResizeString:
        if (byte == '\033') input->escape = ResizeStringEscape;
        else if (input->osc && byte == '\a') input->escape = ResizeGround;
        break;
    case ResizeStringEscape:
        input->escape = byte == '\\' || (input->osc && byte == '\a') ? ResizeGround :
            byte == '\033' ? ResizeStringEscape : ResizeString;
        break;
    case ResizeEscape:
        input->length = 0;
        if (byte == '[') input->escape = ResizeCSI;
        else {
            input->line = ResizeLineRejected;
            input->osc = byte == ']';
            input->escape = (byte == ']' || byte == 'P' || byte == '_' || byte == '^' || byte == 'X') ?
                ResizeString : byte == '\033' ? ResizeEscape : ResizeGround;
        }
        break;
    case ResizeCSI:
        if (byte >= 0x40 && byte <= 0x7e) {
            input->escape = ResizeGround;
            if (input->length < sizeof(input->csi)) {
                input->csi[input->length++] = (char)byte;
                return ResizeFinishCSI(input, tty);
            }
            input->line = ResizeLineRejected;
        } else if (byte >= 0x20 && byte < 0x40) {
            if (input->length < sizeof(input->csi)) input->csi[input->length++] = (char)byte;
        } else {
            input->line = ResizeLineRejected;
            input->escape = byte == '\033' ? ResizeEscape : ResizeGround;
        }
        break;
    case ResizeGround:
        if (byte == '\033') {
            // Well-formed mouse frames may precede a line, but cannot be
            // interleaved with its digits (or with a dismissal).
            if (input->line != ResizeLineEmpty) input->line = ResizeLineRejected;
            input->escape = ResizeEscape;
        } else return ResizeReadLineByte(input, byte);
    }
    return ResizeInputWaiting;
}

static inline ResizeInputResult ReadResizeInput(ResizeInput *input, int tty) {
    if (input->mode == ResizeOff) return ResizeInputFailed;
    if (!ResizeTTYMatches(input, tty)) goto failed;
    ResizeInputResult result = ResizeInputWaiting;
    bool empty = false;
    for (unsigned i = 0; i < 8; ++i) {
        unsigned char bytes[128];
        ssize_t count = read(tty, bytes, sizeof(bytes));
        if (count < 0 && errno == EINTR) continue;
        if (count < 0 && errno != EAGAIN && errno != EWOULDBLOCK) goto failed;
        if (count <= 0) { empty = true; break; }
        for (ssize_t j = 0; j < count; ++j) {
            ResizeInputResult next = ResizeReadByte(input, tty, bytes[j]);
            if (next == ResizeInputFailed) goto failed;
            if (next > result) result = next;
        }
    }
    if (!ResizeTTYMatches(input, tty)) goto failed;
    if (input->mode != ResizeProbe) {
        // A positive receipt must not outrun a dismissal queued behind it.
        if (result != ResizeInputDismiss && (!empty || input->escape != ResizeGround)) return ResizeInputWaiting;
        if (result == ResizeInputCode || result == ResizeInputDismiss) input->code = 0;
        return result;
    }
    bool expired = clock_gettime_nsec_np(CLOCK_MONOTONIC_RAW) >= input->probeEnd;
    // Drain through the reply and any queued input without exposing a
    // receipt. Do not split a paste or control string at the arming boundary.
    if (!empty || input->escape != ResizeGround) {
        if (expired) goto failed;
        return ResizeInputWaiting;
    }
    if (input->replies != 7 && !expired) return ResizeInputWaiting;
    // Verified bracketed paste separates ordinary input from pasted data.
    if (!(input->enabled & 4)) goto failed;
    if ((input->enabled & 3) == 3) input->mode = ResizeMouse;
    else {
        static const char restoreMouse[] = "\033[?1003;1006r";
        struct termios canonical = ResizeTTYMode(input->saved, true);
        if (!ResizeWrite(tty, restoreMouse, sizeof(restoreMouse) - 1) || tcsetattr(tty, TCSANOW, &canonical) < 0)
            goto failed;
        input->mode = ResizeCanonical;
    }
    if (!ResizeTTYMatches(input, tty)) goto failed;
    return ResizeInputWaiting;
failed:
    EndResizeInput(input, tty);
    return ResizeInputFailed;
}
