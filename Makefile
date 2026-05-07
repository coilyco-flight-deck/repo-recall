.DEFAULT_GOAL := help

.PHONY: help install run watch build release test smoke fmt fmt-check lint check ci clean css css-check css-watch \
        docker-demo-build docker-demo-run docker-demo-smoke

# Demo image -----------------------------------------------------------------
demo_image ?= repo-recall-demo:local
demo_port  ?= 7780

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

build: ## cargo build (dev)
	cargo build

release: ## cargo build --release
	cargo build --release

test: ## Run cargo test (unit + integration)
	cargo test --color always

smoke: ## MCP-protocol integration smoke (spawns the binary, talks JSON-RPC over stdio)
	cargo test --color always --test mcp_smoke

fmt: ## Format everything with rustfmt
	cargo fmt --all

fmt-check: ## Check formatting; non-zero exit if anything would change
	cargo fmt --all --check

lint: ## Run clippy with warnings-as-errors
	cargo clippy --all-targets --all-features -- -D warnings

check: ## Fast type-check
	cargo check --all-targets

ci: fmt-check lint check test ## Everything CI runs, in order. Fail fast.

docker-demo-build: ## Build the public-demo container image (Dockerfile.demo)
	docker build -f Dockerfile.demo -t $(demo_image) .

docker-demo-run: docker-demo-build ## Run the demo image, port-forwarded to localhost
	docker run --rm -p $(demo_port):7777 --name repo-recall-demo $(demo_image)

docker-demo-smoke: docker-demo-build ## Build, boot in background, curl /, assert non-empty repo list, kill
	@bash scripts/docker-demo-smoke.sh $(demo_image) $(demo_port)

clean: ## Remove target/ and the redb cache
	cargo clean
	rm -rf $${TMPDIR:-/tmp}/repo-recall-* $${TMPDIR:-/tmp}/repo-recall.sqlite $${TMPDIR:-/tmp}/repo-recall.sqlite-wal $${TMPDIR:-/tmp}/repo-recall.sqlite-shm
