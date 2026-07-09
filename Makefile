# Rebuild and reinstall local CLIs after code changes.
.PHONY: install install-cli install-trader build test help

help:
	@echo "Targets:"
	@echo "  make install        - release-build + install schwab and schwab-trader"
	@echo "  make install-cli    - release-build + install schwab only"
	@echo "  make install-trader - release-build + install schwab-trader only"
	@echo "  make build          - cargo build --workspace (dev)"
	@echo "  make test           - cargo test workspace crates"

install: install-cli install-trader

install-cli:
	cargo install --path crates/schwab-cli --force

install-trader:
	cargo install --path crates/schwab-trader --force

build:
	cargo build --workspace

test:
	cargo test -p schwab-api-cli --lib
	cargo test -p schwab-trader
