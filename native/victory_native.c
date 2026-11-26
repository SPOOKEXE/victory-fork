// The unix side of victory_native.dll: lets the layer make system calls of
// its own under Wine (crates/victory-gpu/src/native.rs).
#define _GNU_SOURCE
#include <errno.h>
#include <stdint.h>
#include <sys/syscall.h>
#include <unistd.h>

struct call {
    int64_t number, x[6], result;
};

static int32_t system_call(void *arg)
{
    struct call *c = arg;
    long r = syscall(c->number, c->x[0], c->x[1], c->x[2], c->x[3], c->x[4], c->x[5]);
    c->result = r == -1 ? -errno : r;
    return 0;
}

const void *__wine_unix_call_funcs[] = { system_call };
