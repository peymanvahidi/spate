# Spate: build, test, install. Works from a plain terminal: the toolchain paths are set here.
#
#   make install                 build (release) and install for the current user, no sudo
#   make uninstall [PURGE=1]     remove it (PURGE=1 also deletes history, settings, logs, downloads)
#   make build                   release build only: Spate.app, spate, spate-host
#   make test                    cargo tests + extension and UI node tests
#   make dev                     run the app from source (debug)
#
# PREFIX_HOME=/some/dir redirects the whole install/uninstall there instead of $HOME (tests).

SHELL := /bin/bash

# Rust from Homebrew's rustup; cargo registry and the Tauri CLI in the project's CARGO_HOME
# (see PLAN.md). Override SPATE_CARGO_HOME to use another one.
SPATE_CARGO_HOME ?= /Users/heispv/Documents/spate/.cargo-home
ifneq ($(wildcard $(SPATE_CARGO_HOME)),)
export CARGO_HOME := $(SPATE_CARGO_HOME)
endif
export PATH := /opt/homebrew/opt/rustup/bin:$(if $(CARGO_HOME),$(CARGO_HOME)/bin:)$(HOME)/.cargo/bin:$(PATH)
# make runs simple recipe lines without a shell, looking programs up in its own startup PATH.
CARGO := $(or $(shell PATH='$(PATH)' command -v cargo),cargo)
NODE := $(or $(shell PATH='$(PATH)' command -v node),node)

PREFIX_HOME ?= $(HOME)
export PREFIX_HOME

ROOT := $(abspath $(dir $(lastword $(MAKEFILE_LIST))))

.PHONY: all build install uninstall test dev

all: build

build:
	$(CARGO) build --release --locked -p spate-cli -p spate-host
	cd "$(ROOT)/app/src-tauri" && $(CARGO) tauri build --bundles app

install: build
	"$(ROOT)/scripts/spate-install.sh" install

uninstall:
	"$(ROOT)/scripts/spate-install.sh" uninstall

test:
	$(CARGO) test --workspace --locked
	cd "$(ROOT)/extension" && $(NODE) --test
	cd "$(ROOT)/app/ui" && $(NODE) --test tests/*.test.js

dev:
	cd "$(ROOT)/app/src-tauri" && $(CARGO) tauri dev
