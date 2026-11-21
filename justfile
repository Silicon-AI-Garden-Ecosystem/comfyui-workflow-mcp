build-dev *args:
    cargo build -- {{ args }}

build-nix *args:
    rom build .#default -- {{ args }}

build-release *args:
    cargo build --release -- {{ args }}

clean *args:
    cargo clean -- {{ args }}

format:
    just --fmt
    treefmt .
    cargo fmt

install *args:
    cargo fetch -- {{ args }}

run-dev *args:
    cargo run -- {{ args }}

run-nix *args:
    rom run .#default -- {{ args }}

run-release *args:
    cargo run --release -- {{ args }}

show:
    nix flake show

test:
    cargo check
    cargo test
    cargo clippy --all-targets -- --deny warnings

update:
    nix flake update
    cargo update
