#define _GNU_SOURCE
#include <dlfcn.h>

typedef int (*marker_fn)(void);
int vosulay_loader_marker(void) { return 23; }
int vosulay_loader_local_marker(void) { return 41; }

int vosulay_loader_next(void) {
    marker_fn marker = (marker_fn)dlsym(RTLD_NEXT, "vosulay_loader_marker");
    return marker ? marker() : -1;
}

int vosulay_loader_default(void) {
    marker_fn marker = (marker_fn)dlsym(RTLD_DEFAULT, "vosulay_loader_local_marker");
    return marker ? marker() : -1;
}
