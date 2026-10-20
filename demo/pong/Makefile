CATO ?= $(shell command -v cato 2>/dev/null || echo ../../target/release/cato)
SDL_CFLAGS := $(shell pkg-config --cflags sdl3)
SDL_LIBS := $(shell pkg-config --libs sdl3)

.PHONY: all clean run
all: pong

pong: pong.cato sdl3_shim.c
	$(CATO) compile pong.cato -o pong -- sdl3_shim.c $(SDL_CFLAGS) $(SDL_LIBS)

run: pong
	./pong

clean:
	rm -f pong