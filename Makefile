.DEFAULT_GOAL := help

.PHONY: help install run watch build release test smoke fmt fmt-check lint check ci clean css css-check css-watch

# Config ---------------------------------------------------------------------
# cwd defaults to $REPO_RECALL_CWD if exported, else $(CURDIR). Lets callers
# do `REPO_RECALL_CWD=$(pwd) make -C repo-recall run` from a parent dir.
cwd        ?= $(or $(REPO_RECALL_CWD),$(CURDIR))
port       ?= $(or $(REPO_RECALL_PORT),7780)
depth      ?= $(or $(REPO_RECALL_DEPTH),4)
https_host ?= repo-recall.localhost
https_port ?= 7443

help: ## Show this help
	@perl -nle'print $& if m{^[a-zA-Z_-]+:.*?## .*$$}' $(MAKEFILE_LIST) | sort | awk 'BEGIN {FS = ":.*?## "}; {printf "\033[36m%-18s\033[0m %s\n", $$1, $$2}'

install: ## Install dev tooling (cargo-watch, pre-commit hooks, tailwindcss)
	cargo install cargo-watch --locked
	@command -v pre-commit >/dev/null || pip install --user pre-commit
	@command -v tailwindcss >/dev/null || brew install tailwindcss
	pre-commit install

css: ## Compile static/tailwind.css from static/tailwind.input.css
	tailwindcss -i static/tailwind.input.css -o static/tailwind.css --minify

css-check: css ## Rebuild CSS and warn if it drifted from the committed copy
	@if ! git diff --exit-code -- static/tailwind.css >/dev/null; then \
		echo 'note: static/tailwind.css drifted on regen.'; \
		echo '      tailwindcss v4 standalone output is not byte-identical across'; \
		echo '      platforms, so this is informational. Commit it if you intended'; \
		echo '      to refresh the bundle; otherwise `git checkout -- static/tailwind.css`.'; \
	fi

css-watch: ## Rebuild CSS on every input/source change (run alongside `make watch`)
	tailwindcss -i static/tailwind.input.css -o static/tailwind.css --watch

run: ## Run the server (cargo + caddy https proxy at https://$(https_host):$(https_port))
	@caddy reverse-proxy --from https://$(https_host):$(https_port) --to 127.0.0.1:$(port) --internal-certs > /tmp/repo-recall-caddy.log 2>&1 & \
		CADDY_PID=$$!; \
		trap "kill $$CADDY_PID 2>/dev/null" EXIT INT TERM; \
		echo "caddy: https://$(https_host):$(https_port) -> 127.0.0.1:$(port) (log: /tmp/repo-recall-caddy.log)"; \
		REPO_RECALL_CWD=$(cwd) REPO_RECALL_PORT=$(port) REPO_RECALL_DEPTH=$(depth) cargo run

watch: ## cargo-watch + caddy https proxy at https://$(https_host):$(https_port)
	@caddy reverse-proxy --from https://$(https_host):$(https_port) --to 127.0.0.1:$(port) --internal-certs > /tmp/repo-recall-caddy.log 2>&1 & \
		CADDY_PID=$$!; \
		trap "kill $$CADDY_PID 2>/dev/null" EXIT INT TERM; \
		echo "caddy: https://$(https_host):$(https_port) -> 127.0.0.1:$(port) (log: /tmp/repo-recall-caddy.log)"; \
		REPO_RECALL_CWD=$(cwd) REPO_RECALL_PORT=$(port) REPO_RECALL_DEPTH=$(depth) \
			cargo watch -w src -w Cargo.toml -w static -x run

build: ## Compile the binary in debug mode. Forward extras (`--release`, `-p`) verbatim.
	cargo build

release: ## cargo build --release
	cargo build --release

test: ## Run the integration smoke suite (tests/smoke.rs, tests/mcp_smoke.rs) plus unit tests. Forward extras (`--test`, `-- <filter>`) verbatim.
	cargo test --color always

smoke: ## MCP-protocol integration smoke (spawns the binary, talks JSON-RPC over stdio)
	cargo test --color always --test mcp_smoke

fmt: ## Apply rustfmt fixes in place.
	cargo fmt --all

fmt-check: ## Verify rustfmt is clean. Mirrors the CI step.
	cargo fmt --all --check

lint: ## Lint with clippy, treating warnings as errors. Mirrors the CI step.
	cargo clippy --all-targets --all-features -- -D warnings

check: ## Type-check without producing artifacts. Faster than build.
	cargo check --all-targets

ci: fmt-check lint check test ## Full pre-merge gate (fmt-check + clippy + check + test). Matches GitHub Actions.

clean: ## Remove target/ and the redb cache
	cargo clean
	rm -rf $${TMPDIR:-/tmp}/repo-recall-* $${TMPDIR:-/tmp}/repo-recall.sqlite $${TMPDIR:-/tmp}/repo-recall.sqlite-wal $${TMPDIR:-/tmp}/repo-recall.sqlite-shm
