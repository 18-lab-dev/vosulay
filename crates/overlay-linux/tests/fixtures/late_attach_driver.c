#define _GNU_SOURCE
#include <dlfcn.h>
#include <stdio.h>

typedef void *(*swap_fn)(void);
typedef int (*attach_fn)(const char *);

// Usage: late-attach-driver <importer.so> <overlay.so> <endpoint>
int main(int argc, char **argv) {
    if (argc != 4) return 2;
    void *importer = dlopen(argv[1], RTLD_NOW | RTLD_GLOBAL);
    if (!importer) { fprintf(stderr, "importer: %s\n", dlerror()); return 3; }
    swap_fn swap = (swap_fn)dlsym(importer, "vosulay_late_swap");
    if (!swap) return 4;
    void *native = swap();
    // Loaded the way the injector loads it: after the game, without preload.
    void *overlay = dlopen(argv[2], RTLD_NOW | RTLD_GLOBAL);
    if (!overlay) { fprintf(stderr, "overlay: %s\n", dlerror()); return 5; }
    attach_fn attach = (attach_fn)dlsym(overlay, "vosulay_attach_v1");
    void *hook = dlsym(overlay, "glXSwapBuffers");
    if (!attach || !hook || hook == native) return 6;
    if (attach(argv[3]) != 1) { fprintf(stderr, "attach refused\n"); return 7; }
    if (swap() != hook) { fprintf(stderr, "swap slot not rewritten\n"); return 8; }
    printf("late attach verified\n");
    return 0;
}
