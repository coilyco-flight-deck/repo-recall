.DEFAULT_GOAL := help

.PHONY: help install run watch build release test fmt fmt-check lint check ci clean css css-check css-watch \
        docker-demo-build docker-demo-run docker-demo-smoke

# Demo image -----------------------------------------------------------------
demo_image ?= repo-recall-demo:local
demo_port  ?= 7777

# Config ---------------------------------------------------------------------
# cwd defaults to $REPO_RECALL_CWD if exported, else $(CURDIR). Lets callers
# do `REPO_RECALL_CWD=$(pwd) make -C repo-recall run` from a parent dir.
cwd   ?= $(or $(REPO_RECALL_CWD),$(CURDIR))
port  ?= $(or $(REPO_RECALL_PORT),7777)
depth ?= $(or $(REPO_RECALL_DEPTH),4)

help: ## Show this help
	@perl -nle'print $& if m{^[a-zA-Z_-]+:.*?## .*$$}' $(MAKEFILE_LIST) | sort | awk 'BEGIN {FS = ":.*?## "}; {printf "\033[36m%-18s\033[0m %s\n", $$1, $$2}'

install: ## Install dev tooling (cargo-watch, pre-commit hooks, tailwindcss)
	cargo install cargo-watch --locked
	@command -v pre-commit >/dev/null || pip install --user pre-commit
	@command -v tailwindcss >/dev/null || brew install tailwindcss
	pre-commit install

css: ## Compile static/tailwind.css from static/tailwind.input.css
	tailwindcss -i static/tailwind.input.css -o static/tailwind.css --minify

css-check: css ## Build CSS and fail if it differs from the committed copy
	@if ! git diff --exit-code -- static/tailwind.css >/dev/null; then \
		echo 'static/tailwind.css is stale — run `make css` and commit the result'; \
		echo '---- diff (first 80 lines) ----'; \
		git --no-pager diff -- static/tailwind.css | head -80; \
		exit 1; \
	fi

css-watch: ## Rebuild CSS on every input/source change (run alongside `make watch`)
	tailwindcss -i static/tailwind.input.css -o static/tailwind.css --watch

run: ## Run the server against the current directory
	REPO_RECALL_CWD=$(cwd) REPO_RECALL_PORT=$(port) REPO_RECALL_DEPTH=$(depth) cargo run

watch: ## Run under cargo-watch (rebuild + browser livereload on save)
	REPO_RECALL_CWD=$(cwd) REPO_RECALL_PORT=$(port) REPO_RECALL_DEPTH=$(depth) \
		cargo watch -w src -w Cargo.toml -w static -x run

build: ## cargo build (dev)
	cargo build

release: ## cargo build --release
	cargo build --release

test: ## Run cargo test (unit + integration)
	cargo test --color always

fmt: ## Format everything with rustfmt
	cargo fmt --all

fmt-check: ## Check formatting; non-zero exit if anything would change
	cargo fmt --all --check

lint: ## Run clippy with warnings-as-errors
	cargo clippy --all-targets --all-features -- -D warnings

check: ## Fast type-check
	cargo check --all-targets

ci: fmt-check lint check test css-check ## Everything CI runs, in order. Fail fast.

docker-demo-build: ## Build the public-demo container image (Dockerfile.demo)
	docker build -f Dockerfile.demo -t $(demo_image) .

docker-demo-run: docker-demo-build ## Run the demo image, port-forwarded to localhost
	docker run --rm -p $(demo_port):7777 --name repo-recall-demo $(demo_image)

docker-demo-smoke: docker-demo-build ## Build, boot in background, curl /, assert non-empty repo list, kill
	@bash scripts/docker-demo-smoke.sh $(demo_image) $(demo_port)

clean: ## Remove target/ and the SQLite cache
	cargo clean
	rm -f $${TMPDIR:-/tmp}/repo-recall.sqlite $${TMPDIR:-/tmp}/repo-recall.sqlite-wal $${TMPDIR:-/tmp}/repo-recall.sqlite-shm
