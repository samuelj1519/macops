#define _DARWIN_C_SOURCE
#include <dlfcn.h>
#include <fcntl.h>
#include <stdarg.h>
#include <stdbool.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

static int log_fd = -1;
static struct stat canary;
static bool armed = false;

__attribute__((constructor)) static void initialize(void) {
    const char *target = getenv("MACOPS_UV_CANARY");
    const char *log = getenv("MACOPS_UV_OPEN_LOG");
    if (target && log && stat(target, &canary) == 0) {
        log_fd = open(log, O_WRONLY | O_CREAT | O_APPEND | O_CLOEXEC, 0600);
        armed = log_fd >= 0;
    }
}
static void record(int fd) {
    struct stat found;
    if (armed && fd >= 0 && fstat(fd, &found) == 0 && found.st_dev == canary.st_dev && found.st_ino == canary.st_ino) {
        const char text[] = "OPENED-CANARY\n";
        (void)write(log_fd, text, sizeof(text) - 1);
    }
}
static int tracked_open(const char *path, int flags, ...) {
    mode_t mode = 0;
    if (flags & O_CREAT) { va_list args; va_start(args, flags); mode = (mode_t)va_arg(args, int); va_end(args); }
    int fd = open(path, flags, mode);
    record(fd);
    return fd;
}
static int tracked_openat(int parent, const char *path, int flags, ...) {
    mode_t mode = 0;
    if (flags & O_CREAT) { va_list args; va_start(args, flags); mode = (mode_t)va_arg(args, int); va_end(args); }
    int fd = openat(parent, path, flags, mode);
    record(fd);
    return fd;
}
#define INTERPOSE(replacement, original) \
    __attribute__((used)) static struct { const void *replacement; const void *original; } pair_##original \
    __attribute__((section("__DATA,__interpose"))) = { (const void *)(replacement), (const void *)(original) }
INTERPOSE(tracked_open, open);
INTERPOSE(tracked_openat, openat);
