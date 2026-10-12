.PHONY: web build dev test

# Build the frontend into web/dist (embedded into the binary by rust-embed).
web:
	cd web && pnpm install && pnpm build

build: web
	cargo build --release

dev:
	@echo "Run these in two terminals:"
	@echo "  1) cargo run -- --no-open        # backend on 127.0.0.1:7457; prints a one-time auth URL"
	@echo "  2) cd web && pnpm dev            # Vite on :5173, proxies /api and /__auth to :7457"
	@echo "Open the printed auth URL once (sets the dr_session_<port> cookie for localhost),"
	@echo "then use http://localhost:5173/. Run cargo run -- --no-open again for a fresh URL."

test:
	cargo test
	cd web && pnpm test
