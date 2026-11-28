/* Narrow NSS fixture: exercise the production getpwuid_r caller without touching passwd.
 * The external-account tripwire makes final AF subprocess access (including SQLite WAL/SHM)
 * fail the test. All supplied directories belong to the test, never the operator. */
#define _GNU_SOURCE
#include <pwd.h>
#include <errno.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <dlfcn.h>
#include <fcntl.h>
#include <stdarg.h>
#include <dirent.h>

int getpwuid_r(uid_t uid, struct passwd *entry, char *buf, size_t len, struct passwd **result) {
    (void)buf; (void)len;
    *result = NULL;
    const char *mode = getenv("WAKE_FIXTURE_ACCOUNT_MODE");
    if (mode && !strcmp(mode, "passthrough")) {
        int (*next)(uid_t, struct passwd *, char *, size_t, struct passwd **) =
            dlsym(RTLD_NEXT, "getpwuid_r");
        return next(uid, entry, buf, len, result);
    }
    if (!mode || !strcmp(mode, "absent")) return 0;
    if (!strcmp(mode, "io_error")) return EIO;
    entry->pw_uid = !strcmp(mode, "uid_mismatch") ? uid ^ 1 : uid;
    entry->pw_gid = getegid();
    entry->pw_name = "fixture-account";
    entry->pw_passwd = "x";
    entry->pw_gecos = "fixture";
    entry->pw_dir = getenv("WAKE_FIXTURE_ACCOUNT_HOME");
    entry->pw_shell = "/bin/false";
    if (!entry->pw_dir) return EINVAL;
    *result = entry;
    return 0;
}

static void check(const char *path) {
    const char *external = getenv("WAKE_FIXTURE_FORBID_OPEN");
    if (external && !strncmp(path, external, strlen(external)) &&
        (path[strlen(external)] == '/' || !path[strlen(external)])) _exit(94);
}

#define OPEN_HOOK(name) \
int name(const char *path, int flags, ...) { \
    check(path); \
    mode_t mode = 0; \
    if (flags & O_CREAT) { va_list ap; va_start(ap, flags); mode = va_arg(ap, int); va_end(ap); } \
    int (*next)(const char *, int, ...) = dlsym(RTLD_NEXT, #name); \
    return next(path, flags, mode); \
}
OPEN_HOOK(open)
OPEN_HOOK(open64)

DIR *opendir(const char *path) {
    check(path);
    DIR *(*next)(const char *) = dlsym(RTLD_NEXT, "opendir");
    return next(path);
}
