#define _DEFAULT_SOURCE
#define EGL_EGL_PROTOTYPES 1
#define GL_GLEXT_PROTOTYPES
#include <EGL/egl.h>
#include <GLES2/gl2.h>
#include <stdio.h>
#include <unistd.h>
static int near_byte(unsigned char actual, unsigned char expected) {
    int difference = (int)actual - (int)expected;
    return difference >= -4 && difference <= 4;
}


__attribute__((visibility("default")))
int vosulay_run_egl_target(const char *ready_path) {
    EGLDisplay display = eglGetDisplay(EGL_DEFAULT_DISPLAY);
    if (display == EGL_NO_DISPLAY || !eglInitialize(display, NULL, NULL)) return 77;
    const EGLint config_attributes[] = {
        EGL_SURFACE_TYPE, EGL_PBUFFER_BIT,
        EGL_RENDERABLE_TYPE, EGL_OPENGL_ES2_BIT,
        EGL_NONE
    };
    EGLConfig config = NULL;
    EGLint count = 0;
    if (!eglChooseConfig(display, config_attributes, &config, 1, &count) || count != 1) return 77;
    const EGLint surface_attributes[] = {EGL_WIDTH, 640, EGL_HEIGHT, 480, EGL_NONE};
    EGLSurface surface = eglCreatePbufferSurface(display, config, surface_attributes);
    const EGLint context_attributes[] = {EGL_CONTEXT_CLIENT_VERSION, 2, EGL_NONE};
    if (surface == EGL_NO_SURFACE || !eglBindAPI(EGL_OPENGL_ES_API)) return 77;
    EGLContext context = eglCreateContext(display, config, EGL_NO_CONTEXT, context_attributes);
    if (context == EGL_NO_CONTEXT || !eglMakeCurrent(display, surface, surface, context)) return 77;

    FILE *ready = fopen(ready_path, "wb");
    if (ready == NULL) return 6;
    fputs("ready\n", ready);
    fclose(ready);

    int overlay_seen = 0;
    for (int frame = 0; frame < 3000 && !overlay_seen; ++frame) {
        glBindFramebuffer(GL_FRAMEBUFFER, 0);
        glViewport(0, 0, 640, 480);
        glClearColor(0.1f, 0.2f, 0.3f, 1.0f);
        glClear(GL_COLOR_BUFFER_BIT);
        if (!eglSwapBuffers(display, surface)) return 7;
        unsigned char pixel[4] = {0};
        glReadPixels(320, 240, 1, 1, GL_RGBA, GL_UNSIGNED_BYTE, pixel);
        overlay_seen = near_byte(pixel[0], 0x4d)
            && near_byte(pixel[1], 0xc3)
            && near_byte(pixel[2], 0x7a);
        usleep(10000);
    }

    eglMakeCurrent(display, EGL_NO_SURFACE, EGL_NO_SURFACE, EGL_NO_CONTEXT);
    eglDestroyContext(display, context);
    eglDestroySurface(display, surface);
    eglTerminate(display);
    return overlay_seen ? 0 : 5;
}
