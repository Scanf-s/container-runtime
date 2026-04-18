.PHONY: dev-image dev-shell rootfs

dev-image: ## Build development environment (Linux/Debian with Rust)
	@docker build -f Dockerfile -t dev-env .

rootfs: ## Fetch alpine rootfs into ./rootfs (one-shot)
	@docker run --rm --privileged -v "$$PWD:/app" dev-env ./scripts/fetch-rootfs.sh

dev-shell: ## Open interactive shell in the development container
	@docker run --rm -it --privileged -v "$$PWD:/app" dev-env bash