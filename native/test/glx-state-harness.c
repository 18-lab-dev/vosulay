// Tiny native GLX smoke target. It deliberately mutates audited state before
// every swap; run with LD_PRELOAD=liboverlay_agent_gl.so and compare the
// state hash printed after each swap.
#define GL_GLEXT_PROTOTYPES
#include <GL/gl.h>
#include <GL/glx.h>
#include <X11/Xlib.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <dlfcn.h>
#include <unistd.h>

static unsigned state_hash(void) {
    GLint p = 0, vao = 0, ab = 0, eb = 0, tex = 0, fb = 0, unpack = 0, v[4] = {0};
    GLboolean blend = GL_FALSE, depth = GL_FALSE, scissor = GL_FALSE;
    glGetIntegerv(GL_CURRENT_PROGRAM, &p); glGetIntegerv(GL_VERTEX_ARRAY_BINDING, &vao);
    glGetIntegerv(GL_ARRAY_BUFFER_BINDING, &ab); glGetIntegerv(GL_ELEMENT_ARRAY_BUFFER_BINDING, &eb);
    glGetIntegerv(GL_TEXTURE_BINDING_2D, &tex); glGetIntegerv(GL_DRAW_FRAMEBUFFER_BINDING, &fb);
    glGetIntegerv(GL_UNPACK_ALIGNMENT, &unpack); glGetIntegerv(GL_VIEWPORT, v);
    blend = glIsEnabled(GL_BLEND); depth = glIsEnabled(GL_DEPTH_TEST); scissor = glIsEnabled(GL_SCISSOR_TEST);
    return (unsigned)(p * 31u ^ vao * 37u ^ ab * 41u ^ eb * 43u ^ tex * 47u ^ fb * 53u ^ unpack * 59u ^ (unsigned)v[2] * 61u ^ (unsigned)v[3] * 67u ^ blend * 71u ^ depth * 73u ^ scissor * 79u);
}
static int start_agent(const char *path) {
    if (path == NULL) return 0;
    typedef int (*start_fn)(const unsigned char *, size_t);
    void *symbol = dlsym(RTLD_DEFAULT, "vosulay_agent_start_with_payload");
    start_fn start = NULL;
    if (symbol == NULL || sizeof(start) != sizeof(symbol)) return 2;
    memcpy(&start, &symbol, sizeof(start));
    for (int attempt = 0; attempt < 500; ++attempt) {
        FILE *file = fopen(path, "rb");
        if (file != NULL) {
            unsigned char payload[256];
            size_t length = fread(payload, 1, sizeof(payload), file);
            fclose(file);
            return length > 0 && start(payload, length) > 0 ? 0 : 3;
        }
        usleep(10000);
    }
    return 4;
}
int main(int argc, char **argv) {
    Display *dpy = XOpenDisplay(NULL); if (!dpy) return 77;
    int attr[] = {GLX_RGBA, GLX_DOUBLEBUFFER, GLX_RED_SIZE, 8, GLX_GREEN_SIZE, 8, GLX_BLUE_SIZE, 8, None};
    XVisualInfo *vi = glXChooseVisual(dpy, DefaultScreen(dpy), attr); if (!vi) return 77;
    GLXContext ctx = glXCreateContext(dpy, vi, NULL, True); Window win = XCreateSimpleWindow(dpy, RootWindow(dpy, vi->screen), 0, 0, 640, 480, 0, 0, 0); XMapWindow(dpy, win); glXMakeCurrent(dpy, win, ctx);
    int started = start_agent(argc > 1 ? argv[1] : NULL); if (started != 0) return started;
    GLuint fbo = 0, array_buffer = 0, element_buffer = 0;
    glGenFramebuffers(1, &fbo); glGenBuffers(1, &array_buffer); glGenBuffers(1, &element_buffer);
    int overlay_seen = argc <= 1;
    for (int i = 0; i < 120; ++i) {
        glViewport(0, 0, 640 - i % 31, 480 - i % 17);
        glEnable(i & 1 ? GL_BLEND : GL_DEPTH_TEST); glDisable(i & 1 ? GL_DEPTH_TEST : GL_BLEND);
        glBindFramebuffer(GL_FRAMEBUFFER, 0);
        glClearColor(.1f, .2f, .3f, 1.f); glClear(GL_COLOR_BUFFER_BIT | GL_DEPTH_BUFFER_BIT);
        glBindFramebuffer(GL_FRAMEBUFFER, fbo); glBindBuffer(GL_ARRAY_BUFFER, array_buffer);
        glBindBuffer(GL_ELEMENT_ARRAY_BUFFER, element_buffer); glPixelStorei(GL_UNPACK_ALIGNMENT, i & 1 ? 2 : 8);
        unsigned before = state_hash(); glXSwapBuffers(dpy, win); unsigned after = state_hash();
        printf("%d %u %u\n", i, before, after); if (before != after) return 1;
        glBindFramebuffer(GL_FRAMEBUFFER, 0); glReadBuffer(GL_FRONT);
        unsigned char pixel[4] = {0}; glReadPixels(320, 240, 1, 1, GL_RGBA, GL_UNSIGNED_BYTE, pixel);
        if (pixel[0] > 50 || pixel[1] > 80 || pixel[2] > 100) overlay_seen = 1;
        usleep(10000);
    }
    glXMakeCurrent(dpy, None, NULL); glXDestroyContext(dpy, ctx); XDestroyWindow(dpy, win); XCloseDisplay(dpy);
    return overlay_seen ? 0 : 5;
}
