/* raylib_shim.c — primitive-typed wrappers around raylib for Cato FFI.
 *
 * Cato's FFI calls C symbols directly with no marshalling, and Cato has no
 * first-class struct type, so raylib functions that take a `Color` (a small
 * struct) are re-exposed here with plain int components. Compile this file
 * alongside the Cato program:
 *
 *   cato compile examples/snake_raylib.cato -- examples/raylib_shim.c \
 *       -L/opt/homebrew/lib -lraylib -o snake
 */
#include <raylib.h>

int rl_init_window(int w, int h, const char *title) {
    InitWindow(w, h, title);
    return 0;
}

int rl_close_window(void) {
    CloseWindow();
    return 0;
}

int rl_window_should_close(void) {
    return WindowShouldClose() ? 1 : 0;
}

int rl_begin_drawing(void) {
    BeginDrawing();
    return 0;
}

int rl_end_drawing(void) {
    EndDrawing();
    return 0;
}

int rl_clear_background(int r, int g, int b) {
    ClearBackground((Color){ (unsigned char)r, (unsigned char)g, (unsigned char)b, 255 });
    return 0;
}

int rl_draw_rect(int x, int y, int w, int h, int r, int g, int b) {
    DrawRectangle(x, y, w, h, (Color){ (unsigned char)r, (unsigned char)g, (unsigned char)b, 255 });
    return 0;
}

int rl_draw_text(const char *text, int x, int y, int size, int r, int g, int b) {
    DrawText(text, x, y, size, (Color){ (unsigned char)r, (unsigned char)g, (unsigned char)b, 255 });
    return 0;
}

int rl_is_key_pressed(int key) {
    return IsKeyPressed(key) ? 1 : 0;
}

int rl_get_random_value(int min, int max) {
    return GetRandomValue(min, max);
}

int rl_set_target_fps(int fps) {
    SetTargetFPS(fps);
    return 0;
}
