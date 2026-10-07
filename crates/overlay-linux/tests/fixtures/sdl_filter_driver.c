#ifdef VOSULAY_TEST_SDL3
#include <SDL3/SDL.h>
#else
#include <SDL.h>
#endif
#include <stdbool.h>
#include <stdio.h>
#include <string.h>

static int first_calls, second_calls;
static int first_data, second_data;
#if SDL_MAJOR_VERSION >= 3
static bool first_filter(void *userdata, SDL_Event *event) {
#else
static int first_filter(void *userdata, SDL_Event *event) {
#endif
    (void)event;
    if (userdata != &first_data) return 0;
    first_calls++;
    return 1;
}
#if SDL_MAJOR_VERSION >= 3
static bool second_filter(void *userdata, SDL_Event *event) {
#else
static int second_filter(void *userdata, SDL_Event *event) {
#endif
    (void)event;
    if (userdata != &second_data) return 0;
    second_calls++;
    return 0;
}

int main(void) {
    int initialized = SDL_Init(SDL_INIT_EVENTS);
#if SDL_MAJOR_VERSION >= 3
    if (!initialized) {
#else
    if (initialized != 0) {
#endif
        fprintf(stderr, "SDL_Init: %s\n", SDL_GetError());
        return 2;
    }
    SDL_SetEventFilter(first_filter, &first_data);
    SDL_EventFilter got = NULL;
    void *userdata = NULL;
    if (!SDL_GetEventFilter(&got, &userdata) || got != first_filter || userdata != &first_data) return 3;
    SDL_Event event;
    memset(&event, 0, sizeof(event));
#if SDL_MAJOR_VERSION >= 3
    event.type = SDL_EVENT_USER;
#else
    event.type = SDL_USEREVENT;
#endif
    if (!SDL_PushEvent(&event) || first_calls != 1 || second_calls != 0) return 4;
    SDL_SetEventFilter(second_filter, &second_data);
    if (!SDL_GetEventFilter(&got, &userdata) || got != second_filter || userdata != &second_data) return 5;
#if SDL_MAJOR_VERSION >= 3
    if (second_calls != 1) return 9;
    if (SDL_PushEvent(&event) || first_calls != 1 || second_calls != 2) return 6;
#else
    if (second_calls != 0) return 9;
    if (SDL_PushEvent(&event) || first_calls != 1 || second_calls != 1) return 6;
#endif
    SDL_SetEventFilter(NULL, NULL);
    if (SDL_GetEventFilter(&got, &userdata) || got != NULL || userdata != NULL) return 7;
    if (!SDL_PushEvent(&event) || first_calls != 1
#if SDL_MAJOR_VERSION >= 3
        || second_calls != 2
#else
        || second_calls != 1
#endif
    ) return 8;
    SDL_Quit();
    puts("native SDL filter callback/userdata lifecycle verified");
    return 0;
}
