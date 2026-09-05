.PHONY: fmt lint

fmt:
	@cargo fmt

lint:
	@cargo clippy --all-features
