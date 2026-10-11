// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
#define _GNU_SOURCE
#include <dlfcn.h>
#include <errno.h>
#include <signal.h>
#include <stdlib.h>
#include <unistd.h>

/* Loopback/synthetic login regression only. The second fsync is post-rename. */
int fsync(int fd) {
    static int count = 0;
    int (*real_fsync)(int) = dlsym(RTLD_NEXT, "fsync");
    int result = real_fsync(fd);
    const char *selected = getenv("CATABOLIC_TEST_INTERRUPT_SYNC");
    int target = selected ? atoi(selected) : 1;
    if (++count == target) {
        raise(SIGINT);
        /* No sleep: a pre-commit signal must be observed synchronously. */
        if (getenv("CATABOLIC_TEST_EINTR")) {
            errno = EINTR;
            return -1;
        }
    }
    return result;
}
