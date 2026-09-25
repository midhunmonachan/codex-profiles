.PHONY: coverage fmt clippy test check precommit hooks release-tag release-prep env-check clean-artifacts verify-artifacts print-release-notes

coverage:
	cargo llvm-cov --locked --workspace --all-features --ignore-filename-regex "tests/|target/"

fmt:
	cargo fmt --all

clippy:
	cargo clippy --all-targets --locked -- -D warnings

test:
	@if command -v cargo-nextest >/dev/null 2>&1; then \
		cargo nextest run --tests --locked; \
	else \
		cargo test --tests --locked; \
	fi

check:
	@./scripts/check.sh

precommit: check

hooks:
	@hooks_dir=$$(git rev-parse --git-path hooks); \
	repo_root=$$(git rev-parse --show-toplevel); \
	scripts_dir="$$repo_root/scripts"; \
	case "$$hooks_dir" in \
		/*) hooks_dir_abs="$$hooks_dir" ;; \
		*) hooks_dir_abs="$$repo_root/$$hooks_dir" ;; \
	esac; \
	hooks_dir_abs="$${hooks_dir_abs%/}"; \
	scripts_dir="$${scripts_dir%/}"; \
	chmod +x "$$scripts_dir/pre-commit" "$$scripts_dir/pre-push"; \
	if [ "$$hooks_dir_abs" = "$$scripts_dir" ]; then \
		exit 0; \
	fi; \
	mkdir -p "$$hooks_dir"; \
	{ \
		printf '%s\n' '#!/usr/bin/env bash' \
			'set -euo pipefail' \
			'repo_root=$$(git rev-parse --show-toplevel)' \
			'exec "$$repo_root/scripts/pre-commit"'; \
	} > "$$hooks_dir/pre-commit"; \
	chmod +x "$$hooks_dir/pre-commit"; \
	{ \
		printf '%s\n' '#!/usr/bin/env bash' \
			'set -euo pipefail' \
			'repo_root=$$(git rev-parse --show-toplevel)' \
			'exec "$$repo_root/scripts/pre-push" "$$@"'; \
	} > "$$hooks_dir/pre-push"; \
	chmod +x "$$hooks_dir/pre-push"

release-tag:
	@./scripts/release-tag $(ARGS)

release-prep:
	@./scripts/release-prep.sh $(ARGS)

env-check:
	@./scripts/env-check.sh

clean-artifacts:
	@./scripts/clean-artifacts.sh

verify-artifacts:
	@./scripts/verify-artifacts.sh $(ARGS)

print-release-notes:
	@./scripts/print-release-notes.sh $(ARGS)
