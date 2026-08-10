default:
    @just --list

run:
    just fmt
    cargo run

build:
    just fmt
    cargo build

test:
    just fmt
    cargo test

check:
    cargo fmt --check
    cargo clippy --all-targets -- -D warnings
    cargo test

release:
    just fmt
    cargo build --release

install: release
    install -Dm755 target/release/syncerting-tray ~/.local/bin/syncerting-tray
    install -Dm644 resources/syncerting-tray.desktop ~/.config/autostart/syncerting-tray.desktop

fmt:
    cargo fmt

fix:
    just fmt
    cargo clippy --fix --allow-dirty
