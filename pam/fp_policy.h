/*
 * fp_policy.h - which PAM services the immurok module must stay out of.
 *
 * Header-only pure logic so it can be unit-tested (test_fp_policy.c) without
 * pulling in PAM headers or the daemon socket.
 */
#ifndef IMMUROK_FP_POLICY_H
#define IMMUROK_FP_POLICY_H

#include <string.h>

/*
 * Returns 1 if the module must NOT block on a fingerprint for `service` and
 * should instead return PAM_IGNORE immediately (letting pam_unix handle a
 * typed password), 0 otherwise.
 *
 * The one such service is the GNOME screen-unlock service `gdm-password`.
 * Rationale: the graphical unlock has no controlling /dev/tty, so the module's
 * keypress-cancel fallback is dead — blocking here stalls password entry for
 * up to the timeout (40s) or 3 denied scans. Fingerprint unlock for this
 * service is handled out-of-band by the daemon (loginctl unlock-session on a
 * proactive FP match), so skipping the PAM fingerprint wait loses nothing.
 *
 * Explicitly NOT sudo / polkit-1: those have no daemon bypass, so fingerprint
 * authorization there runs only through this module and must keep blocking
 * (touch-to-auth). Keying off service name (not tty presence) avoids
 * regressing polkit-1, which is also tty-less on the graphical path.
 */
static inline int immurok_should_skip_fp(const char *service) {
    if (service == NULL) {
        return 0;
    }
    return strcmp(service, "gdm-password") == 0;
}

#endif /* IMMUROK_FP_POLICY_H */
