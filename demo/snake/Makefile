# Build the Cato Snake demo (requires raylib + cato).
CATO ?= $(shell command -v cato 2>/dev/null || echo ../../target/release/cato)
CC ?= clang

RAYLIB_PREFIX ?= /opt/homebrew
CFLAGS_INC := -I$(RAYLIB_PREFIX)/include
LDFLAGS := -L$(RAYLIB_PREFIX)/lib -lraylib

.PHONY: all clean run
all: snake

snake: snake.cato raylib_shim.c
	$(CATO) compile snake.cato -o snake -- raylib_shim.c $(CFLAGS_INC) $(LDFLAGS)

run: snake
	./snake

clean:
	rm -f snake