/*
 * test_socket_trust.c - unit test for immurok_dir_trusted()
 *
 * The predicate is the PAM side of the privilege-separation boundary: it is
 * what stops the module from talking to a socket an ordinary user could have
 * replaced. Both directions matter — too strict and every auth breaks (the
 * "must be root-owned" reading rejects systemd's own RuntimeDirectory, which
 * is owned by the service user), too loose and the boundary is decorative.
 *
 * Build/run: `make test` in this directory.
 */

#include <assert.h>
#include <stdio.h>
#include <sys/stat.h>
#include "socket_trust.h"

#define DAEMON_UID ((uid_t)978)
#define OTHER_UID  ((uid_t)1000)
#define NO_USER    ((uid_t)-1)

int main(void) {
    /* The two shapes that actually occur in the field. */
    assert(immurok_dir_trusted(S_IFDIR | 0755, 0, DAEMON_UID) == 1);          /* root-owned */
    assert(immurok_dir_trusted(S_IFDIR | 0755, DAEMON_UID, DAEMON_UID) == 1); /* RuntimeDirectory= */

    /* Owned by an ordinary user: they could have rebound the socket. */
    assert(immurok_dir_trusted(S_IFDIR | 0755, OTHER_UID, DAEMON_UID) == 0);

    /* Right owner, but writable by someone else — same attack, one step out. */
    assert(immurok_dir_trusted(S_IFDIR | 0775, 0, DAEMON_UID) == 0);
    assert(immurok_dir_trusted(S_IFDIR | 0757, 0, DAEMON_UID) == 0);
    assert(immurok_dir_trusted(S_IFDIR | 0777, DAEMON_UID, DAEMON_UID) == 0);

    /* Sticky world-writable (/tmp style) is still world-writable: an attacker
     * cannot unlink the daemon's socket there, but they can create the path
     * first and win the race before the daemon starts. Reject. */
    assert(immurok_dir_trusted(S_IFDIR | 01777, 0, DAEMON_UID) == 0);

    /* Not a directory: a symlink or a plain file planted at the path. */
    assert(immurok_dir_trusted(S_IFLNK | 0777, 0, DAEMON_UID) == 0);
    assert(immurok_dir_trusted(S_IFREG | 0644, 0, DAEMON_UID) == 0);

    /* Daemon user does not exist (uninstalled, or install half-done): only a
     * root-owned directory is acceptable. */
    assert(immurok_dir_trusted(S_IFDIR | 0755, 0, NO_USER) == 1);
    assert(immurok_dir_trusted(S_IFDIR | 0755, DAEMON_UID, NO_USER) == 0);

    printf("socket_trust: all assertions passed\n");
    return 0;
}
