/*
 * test_auth_reply.c - unit test for immurok_reply_feed()
 *
 * The module's whole security decision is "did the daemon say OK". Teaching
 * it to read progress lines first (RETRY:<n>) must not open any path where
 * something other than a final "OK..." authenticates, and must not change
 * what an answer from a daemon that sends no progress lines means.
 *
 * Build/run: `make test` in this directory.
 */

#include <assert.h>
#include <stdio.h>
#include <string.h>
#include "auth_reply.h"

/* Feed `s` in pieces of `step` bytes; return the first non-PENDING verdict
 * (or PENDING if none), and the retry count seen via *retry. */
static enum immurok_reply feed_in_steps(const char *s, size_t step, int *retry) {
    struct immurok_reply_state st;
    size_t len = strlen(s), off = 0;
    enum immurok_reply r = IMMUROK_REPLY_PENDING;
    immurok_reply_init(&st);
    while (off < len && r == IMMUROK_REPLY_PENDING) {
        size_t n = len - off < step ? len - off : step;
        r = immurok_reply_feed(&st, s + off, n);
        off += n;
    }
    if (retry) *retry = st.retry_remaining;
    return r;
}

static void expect(const char *s, enum immurok_reply want, int want_retry) {
    size_t step, len = strlen(s);
    for (step = 1; step <= len; step++) {
        int retry = -2;
        enum immurok_reply got = feed_in_steps(s, step, &retry);
        if (got != want || retry != want_retry) {
            fprintf(stderr, "FAIL %s (step %zu): got %d retry %d, want %d retry %d\n",
                    s, step, got, retry, want, want_retry);
            assert(0);
        }
    }
}

int main(void) {
    struct immurok_reply_state st;
    char big[IMMUROK_REPLY_BUF + 8];

    /* What daemons have always sent: one answer, with or without a newline. */
    expect("OK:AUTHENTICATED", IMMUROK_REPLY_OK, -1);
    expect("OK:AUTHENTICATED\n", IMMUROK_REPLY_OK, -1);
    expect("OK:PRE_AUTH\n", IMMUROK_REPLY_OK, -1);
    expect("OK", IMMUROK_REPLY_OK, -1);
    expect("DENY:FP_DENIED", IMMUROK_REPLY_DENY, -1);
    expect("DENY:NOT_CONNECTED\n", IMMUROK_REPLY_DENY, -1);
    expect("ERROR:BUSY", IMMUROK_REPLY_DENY, -1);
    expect("ERROR:BRIDGE_UNAVAILABLE\n", IMMUROK_REPLY_DENY, -1);
    expect("TIMEOUT", IMMUROK_REPLY_DENY, -1);

    /* Progress, then the answer. */
    expect("RETRY:2\nOK:AUTHENTICATED\n", IMMUROK_REPLY_OK, 2);
    expect("RETRY:2\nRETRY:1\nOK:AUTHENTICATED\n", IMMUROK_REPLY_OK, 1);
    expect("RETRY:2\nRETRY:1\nDENY:FP_DENIED\n", IMMUROK_REPLY_DENY, 1);
    expect("RETRY:2\r\nOK:AUTHENTICATED\r\n", IMMUROK_REPLY_OK, 2);
    expect("RETRY:0\nDENY:FP_DENIED", IMMUROK_REPLY_DENY, 0);
    expect("RETRY:12\nOK", IMMUROK_REPLY_OK, 12);

    /* Progress alone decides nothing. */
    expect("RETRY:2\n", IMMUROK_REPLY_PENDING, 2);
    expect("RETRY:2\nRETRY:1\n", IMMUROK_REPLY_PENDING, 1);
    expect("RETRY:2", IMMUROK_REPLY_PENDING, -1);  /* no newline yet: not a line */
    expect("RET", IMMUROK_REPLY_PENDING, -1);

    /* Nothing that merely contains OK, or looks like progress, authenticates. */
    expect("RETRY:OK\n", IMMUROK_REPLY_DENY, -1);
    expect("RETRY:\nOK\n", IMMUROK_REPLY_DENY, -1);
    expect("RETRY:123\nOK\n", IMMUROK_REPLY_DENY, -1);
    expect("RETRY:-1\nOK\n", IMMUROK_REPLY_DENY, -1);
    expect("RETRYOK\n", IMMUROK_REPLY_DENY, -1);
    expect("DENY\nOK\n", IMMUROK_REPLY_DENY, -1);
    expect("\nOK\n", IMMUROK_REPLY_DENY, -1);
    expect(" OK\n", IMMUROK_REPLY_DENY, -1);
    expect("ok\n", IMMUROK_REPLY_DENY, -1);
    expect("O\n", IMMUROK_REPLY_DENY, -1);
    expect("NOK", IMMUROK_REPLY_DENY, -1);

    /* A closed socket is a denial, whatever came before. */
    immurok_reply_init(&st);
    assert(immurok_reply_feed(&st, "", 0) == IMMUROK_REPLY_DENY);
    immurok_reply_init(&st);
    assert(immurok_reply_feed(&st, "RETRY:2\n", 8) == IMMUROK_REPLY_PENDING);
    assert(immurok_reply_feed(&st, "", 0) == IMMUROK_REPLY_DENY);
    immurok_reply_init(&st);
    assert(immurok_reply_feed(&st, "O", 1) == IMMUROK_REPLY_PENDING);
    assert(immurok_reply_feed(&st, "", 0) == IMMUROK_REPLY_DENY);

    /* Overflow is a denial, not a truncation that might start with OK. */
    memset(big, 'R', sizeof(big));
    immurok_reply_init(&st);
    assert(immurok_reply_feed(&st, big, sizeof(big)) == IMMUROK_REPLY_DENY);
    immurok_reply_init(&st);
    assert(immurok_reply_feed(&st, "RETRY", 5) == IMMUROK_REPLY_PENDING);
    assert(immurok_reply_feed(&st, big, IMMUROK_REPLY_BUF) == IMMUROK_REPLY_DENY);

    /* A long run of progress lines never fills the buffer. */
    {
        int i;
        immurok_reply_init(&st);
        for (i = 0; i < 1000; i++)
            assert(immurok_reply_feed(&st, "RETRY:1\n", 8) == IMMUROK_REPLY_PENDING);
        assert(st.len == 0 && st.retry_remaining == 1);
        assert(immurok_reply_feed(&st, "OK\n", 3) == IMMUROK_REPLY_OK);
    }

    printf("test_auth_reply: all assertions passed\n");
    return 0;
}
