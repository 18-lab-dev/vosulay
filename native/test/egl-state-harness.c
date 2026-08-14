// Tiny native EGL smoke target. It mutates audited state and checks the
// before/after state hash around eglSwapBuffers.
#define EGL_EGL_PROTOTYPES 1
#define GL_GLEXT_PROTOTYPES
#include <EGL/egl.h>
#include <GLES2/gl2.h>
#include <stdio.h>
#include <dlfcn.h>
#include <string.h>
#include <unistd.h>

static unsigned state_hash(void) {
    GLint p = 0, tex = 0, fb = 0, ab = 0, eb = 0, unpack = 0, v[4] = {0};
    GLboolean blend = GL_FALSE, depth = GL_FALSE, scissor = GL_FALSE;
    glGetIntegerv(GL_CURRENT_PROGRAM, &p); glGetIntegerv(GL_TEXTURE_BINDING_2D, &tex);
    glGetIntegerv(GL_FRAMEBUFFER_BINDING, &fb); glGetIntegerv(GL_ARRAY_BUFFER_BINDING, &ab);
    glGetIntegerv(GL_ELEMENT_ARRAY_BUFFER_BINDING, &eb); glGetIntegerv(GL_UNPACK_ALIGNMENT, &unpack);
    glGetIntegerv(GL_VIEWPORT, v);
    blend = glIsEnabled(GL_BLEND); depth = glIsEnabled(GL_DEPTH_TEST); scissor = glIsEnabled(GL_SCISSOR_TEST);
    return (unsigned)(p * 31u ^ tex * 37u ^ fb * 41u ^ ab * 43u ^ eb * 47u ^ unpack * 53u ^ (unsigned)v[2] * 59u ^ (unsigned)v[3] * 61u ^ blend * 67u ^ depth * 71u ^ scissor * 73u);
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
    EGLDisplay dpy = eglGetDisplay(EGL_DEFAULT_DISPLAY); if (dpy == EGL_NO_DISPLAY) return 77;
    EGLint major = 0, minor = 0; if (!eglInitialize(dpy, &major, &minor)) return 77;
    EGLint cfg_attr[] = {EGL_SURFACE_TYPE, EGL_PBUFFER_BIT, EGL_RENDERABLE_TYPE, EGL_OPENGL_ES2_BIT, EGL_NONE}; EGLConfig cfg; EGLint n = 0;
    if (!eglChooseConfig(dpy, cfg_attr, &cfg, 1, &n) || n != 1) return 77;
    EGLint pb_attr[] = {EGL_WIDTH, 640, EGL_HEIGHT, 480, EGL_NONE}; EGLSurface surface = eglCreatePbufferSurface(dpy, cfg, pb_attr); if (surface == EGL_NO_SURFACE) return 77;
    EGLint ctx_attr[] = {EGL_CONTEXT_CLIENT_VERSION, 2, EGL_NONE}; eglBindAPI(EGL_OPENGL_ES_API); EGLContext ctx = eglCreateContext(dpy, cfg, EGL_NO_CONTEXT, ctx_attr); if (ctx == EGL_NO_CONTEXT || !eglMakeCurrent(dpy, surface, surface, ctx)) return 77;
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
        unsigned before = state_hash(); eglSwapBuffers(dpy, surface); unsigned after = state_hash();
        printf("%d %u %u\n", i, before, after); if (before != after) return 1;
        glBindFramebuffer(GL_FRAMEBUFFER, 0);
        unsigned char pixel[4] = {0}; glReadPixels(320, 240, 1, 1, GL_RGBA, GL_UNSIGNED_BYTE, pixel);
        if (pixel[0] > 50 || pixel[1] > 80 || pixel[2] > 100) overlay_seen = 1;
        usleep(10000);
    }
    eglMakeCurrent(dpy, EGL_NO_SURFACE, EGL_NO_SURFACE, EGL_NO_CONTEXT); eglDestroyContext(dpy, ctx); eglDestroySurface(dpy, surface); eglTerminate(dpy);
    return overlay_seen ? 0 : 5;
}
