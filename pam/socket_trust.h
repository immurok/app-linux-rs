/*
 * socket_trust.h - is the directory holding the daemon socket trustworthy?
 *
 * Running the daemon as a dedicated system user only buys anything if an
 * ordinary user cannot unlink its socket and bind an impostor in its place —
 * and that property comes entirely from the *directory*. A directory owned by
 * an unprivileged user, or writable by group/other, means "OK"-returning
 * impostor could be sitting there. Check before connecting, refuse otherwise.
 *
 * Header-only pure logic so it can be unit-tested (test_socket_trust.c)
 * without PAM headers or a live daemon.
 */
#ifndef IMMUROK_SOCKET_TRUST_H
#define IMMUROK_SOCKET_TRUST_H

#include <sys/stat.h>
#include <sys/types.h>

/*
 * Returns 1 if a directory with these stat() attributes may hold the daemon
 * socket, 0 otherwise.
 *
 * `daemon_uid` is the uid of the dedicated daemon user (getpwnam("immurok")),
 * or (uid_t)-1 when that user does not exist — then only root-owned passes.
 *
 * Note "root-owned" alone is NOT the right check: systemd's RuntimeDirectory=
 * chowns /run/immurok to the service user, so the daemon uid must be accepted
 * too. Anything else is rejected, as is a non-directory (a symlink planted at
 * the path lstat()s as a link, not a directory).
 */
static inline int immurok_dir_trusted(mode_t st_mode, uid_t st_uid, uid_t daemon_uid) {
    if (!S_ISDIR(st_mode)) {
        return 0;
    }
    if (st_mode & (S_IWGRP | S_IWOTH)) {
        return 0;
    }
    if (st_uid == 0) {
        return 1;
    }
    if (daemon_uid != (uid_t)-1 && st_uid == daemon_uid) {
        return 1;
    }
    return 0;
}

#endif /* IMMUROK_SOCKET_TRUST_H */
