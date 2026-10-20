/* sdl3_shim.c — primitive-typed wrappers around SDL3 for Cato FFI. */
#include <SDL3/SDL.h>

static SDL_Window *g_window = NULL;
static SDL_Renderer *g_renderer = NULL;
static SDL_Event g_event;
static int g_quit = 0;
static int g_w_down = 0, g_s_down = 0, g_up_down = 0, g_down_down = 0;

int sdl_init(void) {
    return SDL_Init(SDL_INIT_VIDEO) ? 0 : -1;
}

int sdl_create_window(int w, int h, const char *title) {
    g_window = SDL_CreateWindow(title, w, h, 0);
    return g_window ? 0 : -1;
}

int sdl_create_renderer(void) {
    g_renderer = SDL_CreateRenderer(g_window, NULL);
    return g_renderer ? 0 : -1;
}

int sdl_poll_event(void) {
    if (SDL_PollEvent(&g_event)) {
        if (g_event.type == SDL_EVENT_QUIT) { g_quit = 1; return 1; }
        if (g_event.type == SDL_EVENT_KEY_DOWN) {
            switch (g_event.key.key) {
                case SDLK_W: g_w_down = 1; break;
                case SDLK_S: g_s_down = 1; break;
                case SDLK_UP: g_up_down = 1; break;
                case SDLK_DOWN: g_down_down = 1; break;
                default: break;
            }
            return 2;
        }
        if (g_event.type == SDL_EVENT_KEY_UP) {
            switch (g_event.key.key) {
                case SDLK_W: g_w_down = 0; break;
                case SDLK_S: g_s_down = 0; break;
                case SDLK_UP: g_up_down = 0; break;
                case SDLK_DOWN: g_down_down = 0; break;
                default: break;
            }
            return 3;
        }
        return 4;
    }
    return 0;
}

int sdl_quit_requested(void) { return g_quit; }
int sdl_w_down(void) { return g_w_down; }
int sdl_s_down(void) { return g_s_down; }
int sdl_up_down(void) { return g_up_down; }
int sdl_down_down(void) { return g_down_down; }

int sdl_set_title(const char *title) {
    if (g_window) SDL_SetWindowTitle(g_window, title);
    return 0;
}

int sdl_render_clear(int r, int g, int b) {
    SDL_SetRenderDrawColor(g_renderer, (Uint8)r, (Uint8)g, (Uint8)b, 255);
    SDL_RenderClear(g_renderer);
    return 0;
}

int sdl_render_fill_rect(int x, int y, int w, int h, int r, int g, int b) {
    SDL_FRect rect = { (float)x, (float)y, (float)w, (float)h };
    SDL_SetRenderDrawColor(g_renderer, (Uint8)r, (Uint8)g, (Uint8)b, 255);
    SDL_RenderFillRect(g_renderer, &rect);
    return 0;
}

int sdl_render_fill_circle(int cx, int cy, int radius, int r, int g, int b) {
    SDL_FPoint pts[2048];
    int n = 0;
    for (int dy = -radius; dy <= radius; dy++) {
        for (int dx = -radius; dx <= radius; dx++) {
            if (dx*dx + dy*dy <= radius*radius) {
                if (n < 2048) {
                    pts[n].x = (float)(cx + dx);
                    pts[n].y = (float)(cy + dy);
                    n++;
                }
            }
        }
    }
    SDL_SetRenderDrawColor(g_renderer, (Uint8)r, (Uint8)g, (Uint8)b, 255);
    SDL_RenderPoints(g_renderer, pts, n);
    return 0;
}

int sdl_render_present(void) {
    SDL_RenderPresent(g_renderer);
    return 0;
}

int sdl_delay(int ms) {
    SDL_Delay((Uint32)ms);
    return 0;
}

int sdl_destroy(void) {
    if (g_renderer) SDL_DestroyRenderer(g_renderer);
    if (g_window) SDL_DestroyWindow(g_window);
    SDL_Quit();
    return 0;
}
