#include <SDL.h>
#include <SDL_opengl.h>
#include <stdint.h>
#include <stdio.h>
#include <signal.h>

static volatile sig_atomic_t stop_requested = 0;
static void request_stop(int signal_number) { (void)signal_number; stop_requested = 1; }

int main(void) {
    signal(SIGINT, request_stop);
    signal(SIGTERM, request_stop);
    if (SDL_Init(SDL_INIT_VIDEO) != 0) { fprintf(stderr, "%s\n", SDL_GetError()); return 1; }
    SDL_GL_SetAttribute(SDL_GL_CONTEXT_MAJOR_VERSION, 3);
    SDL_GL_SetAttribute(SDL_GL_CONTEXT_MINOR_VERSION, 3);
    SDL_GL_SetAttribute(SDL_GL_CONTEXT_PROFILE_MASK, SDL_GL_CONTEXT_PROFILE_CORE);
    SDL_Window *window = SDL_CreateWindow("Vosulay autoresearch game", SDL_WINDOWPOS_UNDEFINED, SDL_WINDOWPOS_UNDEFINED, 640, 480, SDL_WINDOW_OPENGL);
    if (!window) { fprintf(stderr, "%s\n", SDL_GetError()); SDL_Quit(); return 2; }
    SDL_GLContext context = SDL_GL_CreateContext(window);
    if (!context) { fprintf(stderr, "%s\n", SDL_GetError()); SDL_DestroyWindow(window); SDL_Quit(); return 3; }
    if (SDL_GL_SetSwapInterval(0) != 0 || SDL_GL_GetSwapInterval() != 0) {
        fprintf(stderr, "could not disable swap interval: %s\n", SDL_GetError());
        SDL_GL_DeleteContext(context); SDL_DestroyWindow(window); SDL_Quit(); return 4;
    }
    const GLubyte *renderer = glGetString(GL_RENDERER);
    printf("renderer=%s\n", renderer ? (const char *)renderer : "unknown");
    fflush(stdout);
    uint64_t presents = 0;
    int running = 1;
    while (running && !stop_requested) {
        SDL_Event event;
        while (SDL_PollEvent(&event)) if (event.type == SDL_QUIT || (event.type == SDL_WINDOWEVENT && event.window.event == SDL_WINDOWEVENT_CLOSE)) running = 0;
        if (!running || stop_requested) break;
        int width, height;
        SDL_GL_GetDrawableSize(window, &width, &height);
        glViewport(0, 0, width, height);
        glClearColor(0.035f, 0.12f, 0.22f, 1.0f);
        glClear(GL_COLOR_BUFFER_BIT);
        SDL_GL_SwapWindow(window);
        ++presents;
    }
    printf("total_presents=%llu\n", (unsigned long long)presents);
    fflush(stdout);
    SDL_GL_DeleteContext(context);
    SDL_DestroyWindow(window);
    SDL_Quit();
    return 0;
}
