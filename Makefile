.PHONY: play play-beat export export-beat check test clean

play:
	cargo run --release -- examples/groove.cym

play-beat:
	cargo run --release -- examples/beat.cym

export:
	cargo run --release -- render examples/groove.cym groove.wav 120

export-beat:
	cargo run --release -- render examples/beat.cym beat.wav 120

check:
	cargo fmt --all -- --check
	cargo clippy --workspace --all-targets -- -D warnings
	cargo test --workspace

test:
	cargo test --workspace

clean:
	rm -f groove.wav beat.wav out.wav