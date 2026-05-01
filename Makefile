.PHONY: tui

tui:
	docker-compose up -d qdrant
	cargo run -p symdex-cli -- tui
