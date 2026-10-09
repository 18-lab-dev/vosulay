#define _GNU_SOURCE
#include <dlfcn.h>
#include <stdio.h>
#include <string.h>

typedef int (*marker_fn)(void);

static marker_fn function(void *library, const char *name) {
    marker_fn result = (marker_fn)dlsym(library, name);
    if (!result) fprintf(stderr, "%s: %s\n", name, dlerror());
    return result;
}

int main(int argc, char **argv) {
    if (argc != 5) return 2;
    void *overlay = dlopen(argv[4], RTLD_NOW | RTLD_NOLOAD);
    if (!overlay) {
        fprintf(stderr, "overlay: %s\n", dlerror());
        return 10;
    }
    int local = strcmp(argv[1], "local") == 0;
    void *caller = dlopen(argv[2], RTLD_NOW | (local ? RTLD_LOCAL : RTLD_GLOBAL));
    if (!caller) { fprintf(stderr, "%s\n", dlerror()); return 3; }
    if (local) {
        marker_fn lookup = function(caller, "vosulay_loader_default");
        if (!lookup || lookup() != 41) return 4;
    } else {
        void *provider = dlopen(argv[3], RTLD_NOW | RTLD_GLOBAL);
        if (!provider) { fprintf(stderr, "%s\n", dlerror()); return 5; }
        if (strcmp(argv[1], "next") == 0) {
            marker_fn lookup = function(caller, "vosulay_loader_next");
            if (!lookup || lookup() != 37) return 6;
        } else if (strcmp(argv[1], "explicit") == 0) {
            marker_fn first = function(caller, "vosulay_loader_marker");
            marker_fn second = function(provider, "vosulay_loader_marker");
            if (!first || !second || first == second || first() != 23 || second() != 37) return 7;
            dlerror();
            if (dlsym(provider, "vosulay_loader_missing") != NULL || dlerror() == NULL) return 8;
        } else return 9;
        dlclose(provider);
    }
    dlclose(caller);
    dlclose(overlay);
    printf("loader_scope=%s verified\n", argv[1]);
    return 0;
}
