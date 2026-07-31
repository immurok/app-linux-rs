/*
 * test_fp_policy.c - unit test for immurok_should_skip_fp()
 *
 * Reproduction/verification for the GDM screen-unlock blocking bug:
 * on gdm-password the PAM module must get out of the way (PAM_IGNORE) so a
 * typed password reaches pam_unix immediately, instead of blocking on the
 * fingerprint wait (which, with no controlling tty on the graphical unlock,
 * cannot be cancelled and stalls the password path for up to 40s / 3 scans).
 * Fingerprint unlock for that service is handled out-of-band by the daemon
 * (loginctl unlock-session on a proactive FP match), so skipping here is safe.
 *
 * Build/run: `make test` in this directory.
 */

#include <assert.h>
#include <stdio.h>
#include "fp_policy.h"

int main(void) {
    /* Screen-unlock service: must skip fingerprint here (return 1 → PAM_IGNORE)
     * so the typed password reaches pam_unix and is not blocked. This is the
     * regression the fix addresses. */
    assert(immurok_should_skip_fp("gdm-password") == 1);

    /* Services with NO daemon bypass: fingerprint auth runs ONLY through this
     * module, so it must keep blocking (touch-to-auth) — do NOT skip. Skipping
     * these would regress polkit/sudo fingerprint authorization. */
    assert(immurok_should_skip_fp("sudo") == 0);
    assert(immurok_should_skip_fp("polkit-1") == 0);
    assert(immurok_should_skip_fp("login") == 0);
    assert(immurok_should_skip_fp("unknown") == 0);

    /* NULL safety (pam_get_item may leave service NULL). */
    assert(immurok_should_skip_fp(NULL) == 0);

    printf("fp_policy: all assertions passed\n");
    return 0;
}
