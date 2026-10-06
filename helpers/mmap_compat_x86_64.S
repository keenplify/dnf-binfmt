/* SPDX-License-Identifier: MIT
 * Guest counterpart to mmap_compat.c. Preserve the x86-64 mmap ABI and errno.
 * Only discard non-fixed low-address hints that cross the 4 GiB boundary.
 */
.text
.globl mmap, mmap64
.type mmap,@function
.type mmap64,@function
mmap:
mmap64:
    test $0x100010,%ecx /* MAP_FIXED | MAP_FIXED_NOREPLACE */
    jnz .Lsyscall
    test %rdi,%rdi
    jz .Lsyscall
    movabs $0x100000000,%rax
    cmp %rax,%rdi
    jae .Lsyscall
    sub %rdi,%rax
    cmp %rax,%rsi
    jb .Lsyscall
    xor %edi,%edi
.Lsyscall:
    mov %rcx,%r10
    mov $9,%eax
    syscall
    cmp $-4095,%rax
    jb .Lreturn
    neg %eax
    push %rax
    call __errno_location@PLT
    pop %rdx
    mov %edx,(%rax)
    mov $-1,%rax
.Lreturn:
    ret
.size mmap,.-mmap
.size mmap64,.-mmap64
.section .note.GNU-stack,"",@progbits
