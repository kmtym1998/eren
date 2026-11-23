.PHONY: build release

build:
	cargo build --locked

release:
	cargo build --release --locked
