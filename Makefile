# The CLI's gate and shortcuts.
.PHONY: check fmt test install run

# make run signs in to HOST as U with key P; without P the key comes from ONTOLOGIC_KEY.
HOST ?= localhost:6969
U ?= admin
P ?=

# Everything that should pass before a change is done.
check:
	cargo fmt --check
	cargo clippy --all-targets -- -D warnings
	cargo test

fmt:
	cargo fmt

test:
	cargo test

# ontologic on your PATH, built from this checkout with the locked dependencies.
install:
	cargo install --path . --locked

run:
	@cargo run --quiet -- -h $(HOST) -u $(U) $(if $(P),-p $(P),)
