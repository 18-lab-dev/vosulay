#include "late_attach.h"

int vosulay_late_target;

// Never called: late attach only rewrites the importing module's GOT slot.
void glXSwapBuffers(void *display, unsigned long drawable) {
    (void)display;
    (void)drawable;
}

int VOSULAY_LONG_NAME(void) { return 1; }
