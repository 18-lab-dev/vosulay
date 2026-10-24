/* Optional Linux/glibc benchmark instrumentation, never linked into vosulay.
 * Counts malloc-family calls/requested bytes in a task-owned shared mapping.
 * Inherited mappings also count forked CEF children. mmap/GPU allocations are
 * outside this metric. Keep the same probe enabled for every compared build.
 */
#define _GNU_SOURCE
#include <stdatomic.h>
#include <stdint.h>
#include <stdlib.h>
#include <stdio.h>
#include <sys/mman.h>
#include <fcntl.h>
#include <unistd.h>
extern void *__libc_malloc(size_t);
extern void *__libc_calloc(size_t,size_t);
extern void *__libc_realloc(void*,size_t);
extern void __libc_free(void*);
static _Atomic uint64_t *counts;
__attribute__((constructor)) static void start(void) {
    const char *directory=getenv("VOSULAY_PERF_ALLOC_DIR");
    if (!directory) return;
    char path[4096];
    snprintf(path,sizeof(path),"%s/%ld.bin",directory,(long)getpid());
    int fd=open(path,O_CREAT|O_TRUNC|O_RDWR,0600);
    if(fd<0) return;
    if(ftruncate(fd,24)) { close(fd); return; }
    void *p=mmap(0,24,PROT_READ|PROT_WRITE,MAP_SHARED,fd,0);
    close(fd);
    if(p!=MAP_FAILED) counts=p;
}
static void note(size_t n) { if(counts) { atomic_fetch_add_explicit(counts,1,memory_order_relaxed); atomic_fetch_add_explicit(counts+1,n,memory_order_relaxed); } }
void *malloc(size_t n) { note(n); return __libc_malloc(n); }
void *calloc(size_t n,size_t s) { note(n*s); return __libc_calloc(n,s); }
void *realloc(void *p,size_t n) { note(n); return __libc_realloc(p,n); }
void free(void *p) { if(counts && p) atomic_fetch_add_explicit(counts+2,1,memory_order_relaxed); __libc_free(p); }
