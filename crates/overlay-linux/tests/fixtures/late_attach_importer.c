// A loaded module the GOT installer must walk without failing: built with
// VOSULAY_LONG_IMPORT it imports an over-long symbol, and with
// VOSULAY_MANY_RELOCATIONS it carries more than 65536 dynamic relocations, as
// NVIDIA's libnvidia-eglcore does.
#include "late_attach.h"

#ifdef VOSULAY_LONG_IMPORT
int vosulay_late_long(void) { return VOSULAY_LONG_NAME(); }
#endif

#ifdef VOSULAY_MANY_RELOCATIONS
#define R1 &vosulay_late_target,
#define R10 R1 R1 R1 R1 R1 R1 R1 R1 R1 R1
#define R100 R10 R10 R10 R10 R10 R10 R10 R10 R10 R10
#define R1000 R100 R100 R100 R100 R100 R100 R100 R100 R100 R100
#define R10000 R1000 R1000 R1000 R1000 R1000 R1000 R1000 R1000 R1000 R1000
int *vosulay_late_table[] = {R10000 R10000 R10000 R10000 R10000 R10000 R10000};
#endif

// The address is read through this module's GOT slot, which attach rewrites.
void *vosulay_late_swap(void) { return (void *)&glXSwapBuffers; }
