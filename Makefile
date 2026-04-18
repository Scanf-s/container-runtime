.PHONY: dev-image dev-shell

dev-image: ## Build development environment (Linux/Debian with Rust)
	@docker build -f Dockerfile -t dev-env .

dev-shell: ## Open interactive terminal in the development container
	@docker run --rm -it --privileged -v "$$PWD:/app" dev-env bash -c '/app/scripts/fetch-rootfs.sh && ls rootfs'