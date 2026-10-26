.PHONY: all help run seed build release test fmt fmt-check clippy audit md-lint lex-lint lex-validate a11y hooks check clean

all: check ## Default: run the local check suite

help: ## Show this help
	@grep -E '^[a-zA-Z_-]+:.*?## ' $(MAKEFILE_LIST) | awk 'BEGIN {FS = ":.*?## "}; {printf "  %-14s %s\n", $$1, $$2}'

run: ## Run the server on 127.0.0.1:8080
	cargo run -- serve

seed: ## Load demo tags and stories into an empty database
	cargo run -- seed

build: ## Debug build
	cargo build

release: ## Release build
	cargo build --release

test: ## Unit and integration tests
	cargo test

fmt: ## Format sources
	cargo fmt --all

fmt-check: ## Fail if sources are not formatted
	cargo fmt --all --check

clippy: ## Lint, warnings are errors
	cargo clippy --all-targets -- -D warnings

audit: ## Check dependencies against the RustSec advisory database (needs cargo-audit)
	cargo audit

md-lint: ## Lint markdown
	markdownlint-cli2

lex-lint: ## Lint lexicon schemas (needs goat)
	goat lex lint --lint-level info lexicons

lex-validate: ## Validate sample records against the lexicons (needs goat)
	rm -rf target/lex
	mkdir -p target/lex/catalog
	cp -R lexicons/. crates/auth/tests/lexicons/. target/lex/catalog/
	RECORD_SAMPLES=$(CURDIR)/target/lex/records cargo test -q -p threaded-auth --test samples
	for f in target/lex/records/*.json; do goat lex validate -c target/lex/catalog $$f || exit 1; done

a11y: ## Audit every page with axe-core in light and dark (needs node and Chrome)
	scripts/a11y/run.sh

hooks: ## Install pre-commit hooks
	pre-commit install

check: fmt-check clippy test audit md-lint lex-lint lex-validate ## Local suite, same as CI

clean: ## Remove build output
	cargo clean
