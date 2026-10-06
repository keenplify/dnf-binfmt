// SPDX-License-Identifier: MIT
#define _GNU_SOURCE
#include <stdint.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <unistd.h>

// FEX 2604 can accept a non-fixed hint below 4 GiB whose mapping crosses
// that boundary, but then refuses to unmap it (EOVERFLOW). V8 checks the
// failed cleanup and traps. Hints are optional: let the kernel choose a
// normal high address instead. Fixed mappings retain their exact semantics.
void *mmap(void *address, size_t length, int protection, int flags,
           int descriptor, off_t offset) {
    const uintptr_t boundary = UINT64_C(1) << 32;
    const uintptr_t hint = (uintptr_t)address;
    if (!(flags & (MAP_FIXED | MAP_FIXED_NOREPLACE)) && hint &&
        hint < boundary && length >= boundary - hint) {
        address = NULL;
    }
    return (void *)syscall(SYS_mmap, address, length, protection, flags,
                          descriptor, offset);
}

void *mmap64(void *address, size_t length, int protection, int flags,
             int descriptor, off64_t offset) __attribute__((alias("mmap")));
