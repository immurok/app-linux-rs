/*
 * auth_reply.h - reading the daemon's answer to an AUTH request.
 *
 * Header-only pure logic so it can be unit-tested (test_auth_reply.c) without
 * pulling in PAM headers or a socket.
 *
 * The answer used to be a single message: "OK..." or anything else. It may
 * now be preceded by progress lines while the fingerprint gate is open:
 *
 *     RETRY:<n>\n      the finger did not match; n tries are left
 *
 * and the module asks for them by appending ":hints" to its request
 * ("AUTH:<user>:<service>:hints"). A daemon that does not know the field
 * ignores it and sends only the final answer, which this parser handles the
 * way the module always did — so an old daemon with a new module, and a new
 * daemon with an old module (which never asks), both keep working.
 *
 * Only "OK" at the start of the final answer authenticates. Every doubt —
 * overflow, a closed socket, garbage — resolves to DENY.
 */
#ifndef IMMUROK_AUTH_REPLY_H
#define IMMUROK_AUTH_REPLY_H

#include <stddef.h>
#include <string.h>

#define IMMUROK_REPLY_BUF 256
#define IMMUROK_RETRY_PREFIX "RETRY:"
#define IMMUROK_RETRY_PREFIX_LEN 6

enum immurok_reply {
    IMMUROK_REPLY_PENDING = 0, /* keep waiting (and redraw: retry_remaining may have changed) */
    IMMUROK_REPLY_OK,
    IMMUROK_REPLY_DENY
};

struct immurok_reply_state {
    char buf[IMMUROK_REPLY_BUF];
    size_t len;
    /* Tries left after the latest mismatch, or -1 if there has been none. */
    int retry_remaining;
};

static inline void immurok_reply_init(struct immurok_reply_state *st) {
    st->len = 0;
    st->retry_remaining = -1;
}

/* "RETRY:" followed by 1-2 digits and nothing else. Returns the count, or -1. */
static inline int immurok_parse_retry(const char *line, size_t len) {
    size_t i;
    int n = 0;
    if (len <= IMMUROK_RETRY_PREFIX_LEN || len > IMMUROK_RETRY_PREFIX_LEN + 2)
        return -1;
    if (memcmp(line, IMMUROK_RETRY_PREFIX, IMMUROK_RETRY_PREFIX_LEN) != 0)
        return -1;
    for (i = IMMUROK_RETRY_PREFIX_LEN; i < len; i++) {
        if (line[i] < '0' || line[i] > '9')
            return -1;
        n = n * 10 + (line[i] - '0');
    }
    return n;
}

/* Could `s` (no newline seen yet) still grow into a RETRY line? */
static inline int immurok_maybe_retry(const char *s, size_t len) {
    size_t cmp = len < IMMUROK_RETRY_PREFIX_LEN ? len : IMMUROK_RETRY_PREFIX_LEN;
    return memcmp(s, IMMUROK_RETRY_PREFIX, cmp) == 0;
}

/*
 * Feed `n` bytes just read from the socket (n == 0 means the peer closed).
 * Returns PENDING while only progress has arrived, otherwise the verdict.
 */
static inline enum immurok_reply immurok_reply_feed(struct immurok_reply_state *st,
                                                    const char *data, size_t n) {
    if (n == 0)
        return IMMUROK_REPLY_DENY;
    if (n > sizeof(st->buf) - st->len)
        return IMMUROK_REPLY_DENY;
    memcpy(st->buf + st->len, data, n);
    st->len += n;

    for (;;) {
        char *nl = memchr(st->buf, '\n', st->len);
        size_t line_len, used;
        int retry;
        if (nl == NULL)
            break;
        line_len = (size_t)(nl - st->buf);
        used = line_len + 1;
        if (line_len > 0 && st->buf[line_len - 1] == '\r')
            line_len--;
        retry = immurok_parse_retry(st->buf, line_len);
        if (retry < 0) {
            /* Not progress: this line is the answer. */
            return (line_len >= 2 && memcmp(st->buf, "OK", 2) == 0)
                       ? IMMUROK_REPLY_OK : IMMUROK_REPLY_DENY;
        }
        st->retry_remaining = retry;
        memmove(st->buf, st->buf + used, st->len - used);
        st->len -= used;
    }

    if (st->len == 0)
        return IMMUROK_REPLY_PENDING;
    /* No newline yet. The final answer is not required to end with one (the
     * native daemon's does not), so it is taken as soon as it cannot be the
     * beginning of a RETRY line. */
    if (immurok_maybe_retry(st->buf, st->len))
        return IMMUROK_REPLY_PENDING;
    if (st->len < 2)
        /* One byte: "O" may still become "OK"; anything else already is not. */
        return st->buf[0] == 'O' ? IMMUROK_REPLY_PENDING : IMMUROK_REPLY_DENY;
    return memcmp(st->buf, "OK", 2) == 0 ? IMMUROK_REPLY_OK : IMMUROK_REPLY_DENY;
}

#endif /* IMMUROK_AUTH_REPLY_H */
