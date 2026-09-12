/* Linux test subprocess only. No production code or global fault settings. */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/syscall.h>
#include <unistd.h>

static int fault(int fd, int syncing) {
    const char *root = getenv("HOSHIKAGE_IO_ROOT");
    const char *mode = getenv("HOSHIKAGE_IO_MODE");
    char marker[4096], link[64], path[4096], target[4096];
    static unsigned directory_syncs;
    if (!root || !mode) return 0;
    snprintf(marker, sizeof marker, "%s/armed", root);
    if (access(marker, F_OK)) return 0;
    snprintf(link, sizeof link, "/proc/self/fd/%d", fd);
    ssize_t length = readlink(link, path, sizeof path - 1);
    if (length < 0) return 0;
    path[length] = 0;
    int hit = 0;
    snprintf(target, sizeof target, "%s/state/staging/", root);
    if (!strncmp(path, target, strlen(target)))
        hit = syncing ? !strcmp(mode, "sync_staging") : !strcmp(mode, "write_staging");
    snprintf(target, sizeof target, "%s/state/blobs/", root);
    if (syncing && !strncmp(path, target, strlen(target)) && strstr(path, ".manifest"))
        hit = !strcmp(mode, "sync_manifest");
    snprintf(target, sizeof target, "%s/state/blobs", root);
    if (syncing && !strcmp(path, target) && !strcmp(mode, "sync_directory"))
        hit = ++directory_syncs >= 2; /* fail after rename, and during recovery */
    snprintf(target, sizeof target, "%s/state/metadata.sqlite3-wal", root);
    if (!strcmp(path, target))
        hit = syncing ? !strcmp(mode, "sqlite_sync") : !strcmp(mode, "sqlite_write");
    if (!hit) return 0;
    snprintf(marker, sizeof marker, "%s/hit", root);
    int log = syscall(SYS_openat, AT_FDCWD, marker, O_WRONLY | O_CREAT | O_APPEND, 0600);
    if (log >= 0) { syscall(SYS_write, log, "x", 1); syscall(SYS_close, log); }
    errno = syncing || !strncmp(mode, "sqlite_", 7) ? EIO : ENOSPC;
    return 1;
}
ssize_t write(int fd, const void *data, size_t size) {
    if (fault(fd, 0)) return -1;
    return syscall(SYS_write, fd, data, size);
}
int fsync(int fd) {
    if (fault(fd, 1)) return -1;
    return syscall(SYS_fsync, fd);
}

ssize_t pwrite(int fd, const void *data, size_t size, off_t offset) {
    if (fault(fd, 0)) return -1;
    return syscall(SYS_pwrite64, fd, data, size, offset);
}
ssize_t pwrite64(int fd, const void *data, size_t size, off64_t offset) {
    if (fault(fd, 0)) return -1;
    return syscall(SYS_pwrite64, fd, data, size, offset);
}
int fdatasync(int fd) {
    if (fault(fd, 1)) return -1;
    return syscall(SYS_fdatasync, fd);
}
