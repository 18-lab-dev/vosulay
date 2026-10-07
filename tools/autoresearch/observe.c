#define _POSIX_C_SOURCE 200809L
#include <X11/Xlib.h>
#include <X11/Xutil.h>
#include <X11/extensions/XShm.h>
#include <errno.h>
#include <inttypes.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/ipc.h>
#include <sys/shm.h>
#include <time.h>

/* counter.html protocol: 0xa7, uint32 LE ID, CRC8(poly 0x07); 48x8. */
#define WIDTH 48
#define HEIGHT 8
#define SAMPLE_HZ 20000

static int x_error = 0;
static int handle_x_error(Display *display, XErrorEvent *event) {
    char text[128];
    XGetErrorText(display, event->error_code, text, sizeof(text));
    fprintf(stderr, "pixel observer X11 error: %s\n", text);
    x_error = 1;
    return 0;
}

static uint64_t now_ns(void) {
    struct timespec value;
    if (clock_gettime(CLOCK_MONOTONIC, &value) != 0) {
        perror("clock_gettime");
        exit(1);
    }
    return (uint64_t)value.tv_sec * UINT64_C(1000000000) + (uint64_t)value.tv_nsec;
}

static int decode(const XImage *image, uint32_t *id) {
    uint8_t bytes[6] = {0};
    const uint32_t *first = (const uint32_t *)image->data;
    for (int x = 0; x < WIDTH; ++x) {
        const uint32_t pixel = first[x] & UINT32_C(0xffffff);
        if (pixel != 0 && pixel != UINT32_C(0xffffff)) return 0;
        bytes[x / 8] = (uint8_t)((bytes[x / 8] << 1) | (pixel != 0));
        for (int y = 1; y < HEIGHT; ++y) {
            const uint32_t *row = (const uint32_t *)(image->data + y * image->bytes_per_line);
            if ((row[x] & UINT32_C(0xffffff)) != pixel) return 0;
        }
    }
    if (bytes[0] != 0xa7) return 0;
    uint8_t crc = 0;
    for (int i = 1; i <= 4; ++i) {
        crc ^= bytes[i];
        for (int bit = 0; bit < 8; ++bit)
            crc = (uint8_t)((crc << 1) ^ ((crc & 0x80) ? 0x07 : 0));
    }
    if (crc != bytes[5]) return 0;
    *id = (uint32_t)bytes[1] | ((uint32_t)bytes[2] << 8) |
          ((uint32_t)bytes[3] << 16) | ((uint32_t)bytes[4] << 24);
    return 1;
}

int main(int argc, char **argv) {
    if (argc != 6) {
        fprintf(stderr, "usage: observe WINDOW X Y SECONDS TRANSITIONS.csv\n");
        return 2;
    }
    char *end = NULL;
    const Window window = strtoul(argv[1], &end, 10);
    if (!window || !end || *end) return 2;
    const int x = (int)strtol(argv[2], &end, 10);
    if (!end || *end || x < 0) return 2;
    const int y = (int)strtol(argv[3], &end, 10);
    if (!end || *end || y < 0) return 2;
    const double seconds = strtod(argv[4], &end);
    if (!end || *end || !(seconds > 0 && seconds <= 3600)) return 2;
    int result = 1, attached = 0;
    Display *display = XOpenDisplay(NULL);
    XImage *image = NULL;
    FILE *output = NULL;
    XShmSegmentInfo shared = {.shmid = -1, .shmaddr = (char *)-1};
    if (!display) { fprintf(stderr, "observer cannot open private display\n"); return 1; }
    XSetErrorHandler(handle_x_error);
    XWindowAttributes attributes;
    if (!XShmQueryExtension(display) || !XGetWindowAttributes(display, window, &attributes) ||
        x + WIDTH > attributes.width || y + HEIGHT > attributes.height) {
        fprintf(stderr, "observer requires MIT-SHM and a valid marker window/region\n");
        goto cleanup;
    }
    image = XShmCreateImage(display, attributes.visual, (unsigned)attributes.depth, ZPixmap,
                            NULL, &shared, WIDTH, HEIGHT);
    if (!image || image->bits_per_pixel != 32 || image->byte_order != LSBFirst ||
        image->red_mask != 0xff0000 || image->green_mask != 0xff00 || image->blue_mask != 0xff) {
        fprintf(stderr, "unsupported observer XImage layout\n");
        goto cleanup;
    }
    shared.shmid = shmget(IPC_PRIVATE, (size_t)image->bytes_per_line * HEIGHT, IPC_CREAT | 0600);
    if (shared.shmid < 0) { perror("shmget"); goto cleanup; }
    shared.shmaddr = shmat(shared.shmid, NULL, 0);
    if (shared.shmaddr == (char *)-1) { perror("shmat"); goto cleanup; }
    image->data = shared.shmaddr;
    shared.readOnly = False;
    if (!XShmAttach(display, &shared)) { fprintf(stderr, "XShmAttach failed\n"); goto cleanup; }
    XSync(display, False);
    if (x_error) goto cleanup;
    attached = 1;
    if (shmctl(shared.shmid, IPC_RMID, NULL) != 0) { perror("shmctl"); goto cleanup; }
    output = fopen(argv[5], "w");
    if (!output) { perror("transitions"); goto cleanup; }
    char output_buffer[65536];
    if (setvbuf(output, output_buffer, _IOFBF, sizeof(output_buffer)) != 0) goto cleanup;
    fprintf(output, "elapsed_seconds,frame_id\n");
    uint32_t previous = 0;
    if (!XShmGetImage(display, window, image, x, y, AllPlanes) || x_error || !decode(image, &previous)) {
        fprintf(stderr, "initial composed marker invalid\n");
        goto cleanup;
    }
    const uint64_t started = now_ns();
    const uint64_t until = started + (uint64_t)(seconds * 1e9);
    uint64_t next = started, last_sample = started, maximum_gap = 0;
    uint64_t samples = 0, invalid = 0, changes = 0, skipped = 0;
    while (now_ns() < until) {
        if (!XShmGetImage(display, window, image, x, y, AllPlanes) || x_error) goto cleanup;
        const uint64_t now = now_ns();
        const uint64_t gap = now - last_sample;
        if (gap > maximum_gap) maximum_gap = gap;
        last_sample = now;
        ++samples;
        uint32_t id;
        if (!decode(image, &id)) {
            ++invalid;
        } else if (id != previous) {
            const uint32_t delta = id - previous;
            if (delta >= UINT32_C(0x80000000)) {
                fprintf(stderr, "displayed marker moved backwards\n");
                goto cleanup;
            }
            skipped += delta - 1;
            ++changes;
            fprintf(output, "%.9f,%" PRIu32 "\n", (double)(now - started) / 1e9, id);
            previous = id;
        }
        next += UINT64_C(1000000000) / SAMPLE_HZ;
        if (next > now) {
            const struct timespec deadline = {(time_t)(next / UINT64_C(1000000000)),
                                             (long)(next % UINT64_C(1000000000))};
            int error;
            do { error = clock_nanosleep(CLOCK_MONOTONIC, TIMER_ABSTIME, &deadline, NULL); }
            while (error == EINTR);
            if (error) { fprintf(stderr, "observer sleep error %d\n", error); goto cleanup; }
        } else {
            next = now; /* no catch-up bursts */
        }
    }
    const double elapsed = (double)(now_ns() - started) / 1e9;
    if (fflush(output) != 0 || ferror(output)) { perror("transitions write"); goto cleanup; }
    printf("{\"elapsed_seconds\":%.9f,\"visible_changes\":%" PRIu64
           ",\"updates_per_second\":%.9f,\"observer_hz\":%.9f,\"samples\":%" PRIu64
           ",\"invalid_samples\":%" PRIu64 ",\"skipped_ids\":%" PRIu64
           ",\"maximum_sample_gap_ms\":%.9f,\"requested_observer_hz\":%d}\n",
           elapsed, changes, changes / elapsed, samples / elapsed, samples, invalid, skipped,
           (double)maximum_gap / 1e6, SAMPLE_HZ);
    if (!samples || !changes || invalid > samples / 20) {
        fprintf(stderr, "invalid delivered pixel observation\n");
        goto cleanup;
    }
    if (changes > samples / 2) {
        fprintf(stderr, "observer saturation: more than half the samples change; throughput is undersampled\n");
        goto cleanup;
    }
    result = 0;
cleanup:
    if (output) fclose(output);
    if (attached) { XShmDetach(display, &shared); XSync(display, False); }
    if (image) { image->data = NULL; XDestroyImage(image); }
    if (shared.shmaddr != (char *)-1) shmdt(shared.shmaddr);
    if (shared.shmid >= 0) shmctl(shared.shmid, IPC_RMID, NULL);
    XCloseDisplay(display);
    return result;
}
