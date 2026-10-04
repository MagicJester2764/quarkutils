/* Where a new thread begins.
 *
 * Calls are to the PLT, which a static link makes direct and a shared
 * library — the C library as `libc.so`, with this in it — needs: a function
 * it might export is not one it may call directly.
 *
 * Entered with RDI holding the thread function's argument, and RSP pointing at
 * two words the creator planted: the thread pointer, then the function. The
 * word the kernel clears when the thread ends was registered by the creator.
 */
	.text
	.global __quark_thread_entry
	.type   __quark_thread_entry,@function
__quark_thread_entry:
	mov %rdi,%rbx           /* the argument, before anything clobbers it */
	pop %rdi                /* the thread pointer */
	pop %r12                /* the function */
	and $-16,%rsp           /* a call needs RSP aligned; below here is ours */
	test %rdi,%rdi
	jz 1f
	call __quark_set_fs@PLT /* FS base, which is where thread-locals hang */
1:
	mov %rbx,%rdi
	call *%r12
	mov %eax,%edi
	call __quark_thread_exit@PLT
	hlt
	.size __quark_thread_entry,.-__quark_thread_entry

/* Where a detached thread ends.
 *
 * It gives back its own stack, which it is standing on, and exits. musl does
 * that with two system calls and nothing between them; here unmapping is the
 * layer's bookkeeping as well as the kernel's, and that is C, and C wants a
 * stack. So the thread steps onto one kept for this and does the rest there.
 *
 * One is enough. musl takes its thread-list lock before it gets here and
 * never lets it go: the kernel does, when the thread has exited
 * (`SYS_SET_CLEAR_TID`). So the next thread to end cannot get this far until
 * this one is no longer on the stack at all.
 *
 * Entered with RDI = the mapping's base and RSI = its size, which is where
 * `__quark_unmap_and_exit` wants them.
 */
	.global __quark_unmapself
	.type   __quark_unmapself,@function
__quark_unmapself:
	lea __quark_last_stack_top(%rip),%rsp
	call __quark_unmap_and_exit@PLT
	hlt
	.size __quark_unmapself,.-__quark_unmapself

	.bss
	.balign 16
__quark_last_stack:
	.space 16384
__quark_last_stack_top:
	.text
	.section .note.GNU-stack,"",@progbits
