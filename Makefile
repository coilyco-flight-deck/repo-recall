.DEFAULT_GOAL := help

.PHONY: help install run watch watch-fixtures watch-fixtures-errors build release test smoke fmt fmt-check lint check ci clean web-install web-dev web-build watch-all docker-build-web

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

install: ## Install dev tooling (cargo-watch, pre-commit hooks, web/ npm deps)
	cargo install cargo-watch --locked
	@command -v pre-commit >/dev/null || pip install --user pre-commit
	pre-commit install
	$(MAKE) web-install

web-install: ## Install npm deps for the web/ subtree
	cd web && npm ci

web-dev: ## Vite dev server (proxies /api, /openapi.json, /mcp to $(or $(VITE_API_TARGET),http://127.0.0.1:$(port)))
	cd web && VITE_API_TARGET=$(or $(VITE_API_TARGET),http://127.0.0.1:$(port)) npm run dev

web-build: ## Build the static SPA bundle at web/dist
	cd web && npm run build

watch-all: ## Run cargo-watch + Vite dev server concurrently (two-step orchestrator wrapped via npx concurrently)
	cd web && npx --yes concurrently --kill-others-on-fail \
		-n api,web -c blue,green \
		"$(MAKE) -C .. watch" \
		"$(MAKE) -C .. web-dev"

docker-build-web: ## Build the static React + Caddy image (Dockerfile.web)
	docker build -t repo-recall-web:dev -f Dockerfile.web .

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
			cargo watch -w src -w Cargo.toml -x run

watch-fixtures: ## Same as `watch`, but feeds GitHub responses from tests/fixtures/github/rest (no real API calls).
	REPO_RECALL_GITHUB_FIXTURES_DIR=$(CURDIR)/tests/fixtures/github/rest $(MAKE) watch

watch-fixtures-errors: ## Same as `watch`, but replays the failure-mode fixtures (401, 403 rate-limited, 502, etc.).
	REPO_RECALL_GITHUB_FIXTURES_DIR=$(CURDIR)/tests/fixtures/github/errors $(MAKE) watch

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
