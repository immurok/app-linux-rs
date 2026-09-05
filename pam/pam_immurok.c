/*
 * pam_immurok.c - PAM module for immurok fingerprint authentication (Linux)
 *
 * Communicates with immurok-daemon via Unix socket at /run/immurok/pam.sock.
 * The daemon runs as a dedicated system user and serves the whole machine, so
 * the path is fixed; the socket directory's ownership is the trust boundary
 * (see socket_trust.h).
 * Protocol: "AUTH:username:service" -> "OK", "DENY", or "TIMEOUT"
 *
 * Shows an animated braille spinner on the terminal while waiting.
 */

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <pwd.h>
#include <sys/socket.h>
#include <sys/un.h>
#include <sys/select.h>
#include <fcntl.h>
#include <errno.h>
#include <syslog.h>
#include <time.h>
#include <signal.h>
#include <sys/signalfd.h>

#define PAM_SM_AUTH
#define PAM_SM_ACCOUNT
#define PAM_SM_SESSION
#define PAM_SM_PASSWORD

#include <security/pam_modules.h>
#include <security/pam_ext.h>

#include "fp_policy.h"
#include "socket_trust.h"

#define SOCKET_DIR  "/run/immurok"
#define SOCKET_NAME "pam.sock"
#define SOCKET_PATH SOCKET_DIR "/" SOCKET_NAME
#define DAEMON_USER "immurok"
#define CONNECT_TIMEOUT_MS 2000
#define DEFAULT_TIMEOUT_SEC 40
#define BUFFER_SIZE 256

/* Braille spinner frames (matching macOS TerminalSpinner) */
static const char *spinner_frames[] = {
    "⠖", "⠲", "⢲", "⢰", "⣰", "⣠", "⣄", "⣆", "⡆", "⡖"
};
#define SPINNER_COUNT 10

/* ANSI escape sequences */
#define ANSI_ERASE_LINE "\r\033[K"
#define ANSI_YELLOW     "\033[33m"
#define ANSI_GREEN      "\033[32m"
#define ANSI_RED        "\033[31m"
#define ANSI_RESET      "\033[0m"

/* Try to open the controlling terminal for writing */
static FILE *open_tty(void) {
    FILE *f = fopen("/dev/tty", "w");
    if (f) setbuf(f, NULL); /* unbuffered */
    return f;
}

/* Write spinner frame to terminal */
static void spinner_write(FILE *tty, int frame, const char *message) {
    if (!tty) return;
    fprintf(tty, ANSI_ERASE_LINE ANSI_YELLOW "%s %s" ANSI_RESET,
            spinner_frames[frame % SPINNER_COUNT], message);
}

/* Show result and clear */
static void spinner_result(FILE *tty, int success) {
    if (!tty) return;
    if (success) {
        fprintf(tty, ANSI_ERASE_LINE ANSI_GREEN "✓ Approved!" ANSI_RESET);
    } else {
        fprintf(tty, ANSI_ERASE_LINE ANSI_RED "✗ Denied" ANSI_RESET);
    }
    usleep(500000); /* show result for 0.5s */
    fprintf(tty, ANSI_ERASE_LINE);
}

/* Parse timeout=N from PAM module arguments */
static int parse_timeout(int argc, const char **argv) {
    for (int i = 0; i < argc; i++) {
        if (strncmp(argv[i], "timeout=", 8) == 0) {
            int val = atoi(argv[i] + 8);
            if (val > 0) return val;
        }
    }
    return DEFAULT_TIMEOUT_SEC;
}

/* uid of the dedicated daemon user, or (uid_t)-1 when it does not exist. */
static uid_t daemon_uid(void) {
    struct passwd *pw = getpwnam(DAEMON_USER);
    return pw ? pw->pw_uid : (uid_t)-1;
}

/* Connect with a hard upper bound.
 *
 * A blocking connect() on AF_UNIX is unbounded: SO_SNDTIMEO does not cover
 * connect(), so a daemon that is alive but wedged with a full accept backlog
 * would park us here forever — before the spinner loop below, which is what
 * gives the user the keypress/Ctrl+C escape, ever starts. Fail closed instead:
 * the module is `sufficient`, so giving up just falls through to the password.
 *
 * Returns 0 on success, -1 on error/timeout (errno set). The socket is left in
 * blocking mode either way; the rest of the module expects that. */
static int connect_timeout(int sock, const struct sockaddr_un *addr, int timeout_ms) {
    int flags = fcntl(sock, F_GETFL, 0);
    if (flags < 0 || fcntl(sock, F_SETFL, flags | O_NONBLOCK) < 0)
        return -1;

    int rc = connect(sock, (const struct sockaddr *)addr, sizeof(*addr));
    if (rc < 0 && errno == EINPROGRESS) {
        fd_set wfds;
        FD_ZERO(&wfds);
        FD_SET(sock, &wfds);
        struct timeval ctv;
        ctv.tv_sec = timeout_ms / 1000;
        ctv.tv_usec = (timeout_ms % 1000) * 1000;

        int ready = select(sock + 1, NULL, &wfds, NULL, &ctv);
        if (ready == 0) {
            errno = ETIMEDOUT;
            rc = -1;
        } else if (ready > 0) {
            int err = 0;
            socklen_t len = sizeof(err);
            if (getsockopt(sock, SOL_SOCKET, SO_ERROR, &err, &len) < 0) {
                rc = -1;
            } else if (err != 0) {
                errno = err;
                rc = -1;
            } else {
                rc = 0;
            }
        } else {
            rc = -1;
        }
    }

    int saved = errno;
    (void)fcntl(sock, F_SETFL, flags);
    errno = saved;
    return rc < 0 ? -1 : 0;
}

/* Send authentication request to immurok-daemon and wait for response
 * with animated spinner on the terminal */
static int authenticate_via_socket(pam_handle_t *pamh, const char *user,
                                   const char *service, int timeout_sec) {
    int sock;
    struct sockaddr_un addr;
    char request[BUFFER_SIZE];
    char response[BUFFER_SIZE];
    ssize_t n;
    struct stat dir_st;
    struct timeval tv;

    /* The socket directory is the entire trust boundary: if an ordinary user
     * owns it (or can write it), they could have unlinked the daemon's socket
     * and bound one that answers "OK" to everything. lstat, not stat — a
     * symlink planted at the path must not be followed into somewhere
     * writable. */
    if (lstat(SOCKET_DIR, &dir_st) < 0) {
        pam_syslog(pamh, LOG_ERR, "Cannot stat %s: %s", SOCKET_DIR, strerror(errno));
        return PAM_AUTH_ERR;
    }
    if (!immurok_dir_trusted(dir_st.st_mode, dir_st.st_uid, daemon_uid())) {
        pam_syslog(pamh, LOG_ERR,
                   "Refusing %s: untrusted socket directory (uid=%u mode=%04o)",
                   SOCKET_DIR, (unsigned)dir_st.st_uid,
                   (unsigned)(dir_st.st_mode & 07777));
        return PAM_AUTH_ERR;
    }

    sock = socket(AF_UNIX, SOCK_STREAM, 0);
    if (sock < 0)
        return PAM_AUTH_ERR;

    tv.tv_sec = 5;
    tv.tv_usec = 0;
    setsockopt(sock, SOL_SOCKET, SO_SNDTIMEO, &tv, sizeof(tv));

    memset(&addr, 0, sizeof(addr));
    addr.sun_family = AF_UNIX;
    strncpy(addr.sun_path, SOCKET_PATH, sizeof(addr.sun_path) - 1);

    if (connect_timeout(sock, &addr, CONNECT_TIMEOUT_MS) < 0) {
        pam_syslog(pamh, LOG_ERR, "Failed to connect to %s: %s",
                   SOCKET_PATH, strerror(errno));
        close(sock);
        return PAM_AUTH_ERR;
    }

    snprintf(request, sizeof(request), "AUTH:%s:%s", user, service);
    if (send(sock, request, strlen(request), 0) < 0) {
        close(sock);
        return PAM_AUTH_ERR;
    }

    /* Animate spinner while waiting for response. Also monitor the
     * controlling terminal for any keypress (fall back to password) and a
     * signalfd for SIGINT (Ctrl+C — same "cancel FP, fall to password"
     * behaviour). */
    FILE *tty = open_tty();
    int tty_rd = open("/dev/tty", O_RDONLY | O_NONBLOCK);
    int frame = 0;
    time_t start = time(NULL);
    int result = PAM_AUTH_ERR;
    int nfds = sock + 1;
    if (tty_rd >= 0 && tty_rd >= nfds)
        nfds = tty_rd + 1;

    /* Catch Ctrl+C via signalfd rather than a sigaction() handler. A handler
     * would be a function pointer INTO this module; libpam dlclose()s the
     * module in pam_end(), and a lingering reference into the unmapped module
     * crashes the host (observed: SIGSEGV in dlclose). signalfd installs no
     * such pointer — SIGINT is delivered to a file descriptor we poll and
     * close before returning. Block SIGINT so it is queued to the fd instead
     * of run through the host's disposition; signalfd receives it even if the
     * host (e.g. sudo) already had SIGINT blocked. Mask is restored on exit. */
    sigset_t sigint_set, old_mask;
    sigemptyset(&sigint_set);
    sigaddset(&sigint_set, SIGINT);
    int have_mask = (sigprocmask(SIG_BLOCK, &sigint_set, &old_mask) == 0);
    int sfd = signalfd(-1, &sigint_set, SFD_NONBLOCK | SFD_CLOEXEC);
    if (sfd >= 0 && sfd >= nfds)
        nfds = sfd + 1;

    while (1) {
        /* Check timeout */
        if (time(NULL) - start >= timeout_sec) {
            if (tty) {
                fprintf(tty, ANSI_ERASE_LINE ANSI_RED "✗ Timeout" ANSI_RESET);
                usleep(500000);
                fprintf(tty, ANSI_ERASE_LINE);
            }
            break;
        }

        /* Show spinner */
        spinner_write(tty, frame++, "Please verify your fingerprint...");

        /* Poll socket, tty and signalfd with an 80ms timeout */
        fd_set fds;
        FD_ZERO(&fds);
        FD_SET(sock, &fds);
        if (tty_rd >= 0)
            FD_SET(tty_rd, &fds);
        if (sfd >= 0)
            FD_SET(sfd, &fds);
        tv.tv_sec = 0;
        tv.tv_usec = 80000; /* 80ms per frame */

        int ready = select(nfds, &fds, NULL, NULL, &tv);
        if (ready < 0) {
            /* Interrupted syscall — treat as cancel (fall back to password) */
            if (tty) {
                fprintf(tty, ANSI_ERASE_LINE);
            }
            break;
        }
        if (ready > 0 && sfd >= 0 && FD_ISSET(sfd, &fds)) {
            /* Ctrl+C (SIGINT) — cancel the fingerprint wait and fall through
             * to the password prompt (pam_unix). Drain the fd. Closing the
             * socket below lets the daemon send GATE_CANCEL to stop the LED. */
            struct signalfd_siginfo si;
            (void)read(sfd, &si, sizeof(si));
            pam_syslog(pamh, LOG_INFO, "FP wait cancelled (Ctrl+C) for %s", user);
            if (tty) {
                fprintf(tty, ANSI_ERASE_LINE);
            }
            break;
        }
        if (ready > 0 && FD_ISSET(sock, &fds)) {
            memset(response, 0, sizeof(response));
            n = recv(sock, response, sizeof(response) - 1, 0);
            if (n > 0 && strncmp(response, "OK", 2) == 0) {
                pam_syslog(pamh, LOG_INFO, "Approved for user %s", user);
                result = PAM_SUCCESS;
            }
            spinner_result(tty, result == PAM_SUCCESS);
            break;
        }
        if (ready > 0 && tty_rd >= 0 && FD_ISSET(tty_rd, &fds)) {
            /* Any keypress on tty — cancel and fall back to password */
            char discard[64];
            (void)read(tty_rd, discard, sizeof(discard));
            if (tty) {
                fprintf(tty, ANSI_ERASE_LINE);
            }
            break;
        }
        /* ready == 0: timeout, continue animation */
    }

    /* Tear down the signalfd and restore the host's original signal mask.
     * No handler was installed, so nothing points into this module after we
     * return — safe for libpam to dlclose() us in pam_end(). */
    if (sfd >= 0) close(sfd);
    if (have_mask) sigprocmask(SIG_SETMASK, &old_mask, NULL);

    if (tty) fclose(tty);
    if (tty_rd >= 0) close(tty_rd);
    close(sock);
    return result;
}

PAM_EXTERN int pam_sm_authenticate(pam_handle_t *pamh, int flags,
                                    int argc, const char **argv) {
    const char *user = NULL;
    const char *service = NULL;

    if (pam_get_user(pamh, &user, NULL) != PAM_SUCCESS || user == NULL)
        return PAM_AUTH_ERR;

    if (pam_get_item(pamh, PAM_SERVICE, (const void **)&service) != PAM_SUCCESS || service == NULL)
        service = "unknown";

    /* Screen-unlock services (gdm-password) have no controlling /dev/tty, so
     * the keypress-cancel fallback below is dead and blocking here would stall
     * a typed password for the full timeout / 3 denied scans. Fingerprint
     * unlock for these is handled out-of-band by the daemon (loginctl
     * unlock-session on a proactive FP match), so return PAM_IGNORE and let
     * pam_unix handle the password. Not sudo/polkit-1 — see fp_policy.h. */
    if (immurok_should_skip_fp(service)) {
        pam_syslog(pamh, LOG_INFO,
                   "Service %s handled via daemon (loginctl) — PAM_IGNORE, deferring to password",
                   service);
        return PAM_IGNORE;
    }

    int timeout_sec = parse_timeout(argc, argv);
    pam_syslog(pamh, LOG_INFO, "Auth request: user=%s service=%s timeout=%d",
               user, service, timeout_sec);

    return authenticate_via_socket(pamh, user, service, timeout_sec);
}

/* Required PAM stubs */
PAM_EXTERN int pam_sm_setcred(pam_handle_t *pamh, int flags, int argc, const char **argv) { return PAM_SUCCESS; }
PAM_EXTERN int pam_sm_acct_mgmt(pam_handle_t *pamh, int flags, int argc, const char **argv) { return PAM_SUCCESS; }
PAM_EXTERN int pam_sm_open_session(pam_handle_t *pamh, int flags, int argc, const char **argv) { return PAM_SUCCESS; }
PAM_EXTERN int pam_sm_close_session(pam_handle_t *pamh, int flags, int argc, const char **argv) { return PAM_SUCCESS; }
PAM_EXTERN int pam_sm_chauthtok(pam_handle_t *pamh, int flags, int argc, const char **argv) { return PAM_SUCCESS; }
