# Uptime Medics developer tasks. Run `make help` for an overview.
#
# Cargo is used directly when it is installed. Otherwise every cargo command runs inside the
# official Rust image with cached registry/toolchain/target volumes, so no local toolchain is
# needed (only Docker).

.DEFAULT_GOAL := help
SHELL := /bin/bash

APP_PORT   ?= 8080
BASE_URL   ?= http://localhost:$(APP_PORT)
COMPOSE    ?= docker compose
RUST_IMAGE ?= rust:1-bookworm
TO         ?=

HAS_CARGO := $(shell command -v cargo 2>/dev/null)
DOCKER_RUST := docker run --rm -e CARGO_TERM_COLOR=always \
	-v "$(CURDIR)":/app \
	-v um-cargo-registry:/usr/local/cargo/registry \
	-v um-rustup:/usr/local/rustup \
	-v um-target:/app/target \
	-w /app $(RUST_IMAGE)

ifeq ($(HAS_CARGO),)
CARGO  := $(DOCKER_RUST) cargo
RUSTUP := $(DOCKER_RUST) rustup
else
CARGO  := cargo
RUSTUP := rustup
endif

.PHONY: help build release test test-unit test-integration fmt fmt-check clippy check run \
	docker-build up up-dev down down-clean logs ps smoke smtp-test clean

help: ## Show this help
	@grep -hE '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) | sort | awk 'BEGIN {FS = ":.*?## "}; {printf "  \033[36m%-18s\033[0m %s\n", $$1, $$2}'

# ---------------------------------------------------------------------------------------------
# Rust

build: ## Compile a debug binary
	$(CARGO) build

release: ## Compile an optimised binary (target/release/uptime-medics)
	$(CARGO) build --release

test: ## Run unit and integration tests
	$(CARGO) test

test-unit: ## Run only the unit tests inside src/
	$(CARGO) test --lib --bins

test-integration: ## Run only the integration tests in tests/
	$(CARGO) test --test '*'

fmt: ## Format the code
	@$(RUSTUP) component add rustfmt >/dev/null 2>&1 || true
	$(CARGO) fmt --all

fmt-check: ## Fail if the code is not formatted
	@$(RUSTUP) component add rustfmt >/dev/null 2>&1 || true
	$(CARGO) fmt --all -- --check

clippy: ## Lint with clippy (warnings are errors)
	@$(RUSTUP) component add clippy >/dev/null 2>&1 || true
	$(CARGO) clippy --all-targets -- -D warnings

check: fmt-check clippy test ## Formatting, lints and tests

run: ## Run from source on http://localhost:8080 (needs a local cargo; otherwise use `make up`)
ifeq ($(HAS_CARGO),)
	@echo "cargo is not installed. Use 'make up' to run with Docker, or install Rust from https://rustup.rs"; exit 1
else
	ALLOW_PRIVATE_TARGETS=true cargo run
endif

# ---------------------------------------------------------------------------------------------
# Docker

docker-build: ## Build the production image (uptime-medics:latest)
	$(COMPOSE) build

up: ## Build and start the stack (APP_PORT=8080 overrides the host port)
	APP_PORT=$(APP_PORT) $(COMPOSE) up -d --build

up-dev: ## Same as `up`, plus Mailpit on :8025 for testing alert emails
	APP_PORT=$(APP_PORT) $(COMPOSE) --profile dev up -d --build

down: ## Stop the stack (data is kept)
	$(COMPOSE) --profile dev down

down-clean: ## Stop the stack and delete all data
	$(COMPOSE) --profile dev down -v

logs: ## Follow application logs
	$(COMPOSE) logs -f uptime-medics

ps: ## Show container status and health
	$(COMPOSE) ps

smoke: ## Run the API smoke test against BASE_URL (default http://localhost:$(APP_PORT))
	scripts/smoke_test.sh $(BASE_URL)

smtp-test: ## Send a test email from the running container (TO=you@example.com)
	@test -n "$(TO)" || { echo "usage: make smtp-test TO=you@example.com"; exit 1; }
	$(COMPOSE) exec uptime-medics uptime-medics smtp-test $(TO)

clean: ## Remove build artifacts and the Docker cargo cache volumes
	rm -rf target
	-docker volume rm um-cargo-registry um-rustup um-target 2>/dev/null
