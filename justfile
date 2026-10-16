run *ARGS:
  cargo run {{ ARGS }}

watch *ARGS:
  cargo watch -x run {{ ARGS }}

build:
  cargo build --release

test:
  cargo test --all-targets

watch-test:
  cargo watch -x test

coverage:
  cargo tarpaulin

fmt:
  cargo fmt

lint:
  cargo clippy --all-targets -- -D warnings

man:
  scdoc < mm.1.scd > mm.1

install prefix=(env_var('HOME') + '/.local'): build man
  install -Dm755 target/release/mm {{ prefix }}/bin/mm
  install -Dm644 mm.1 {{ prefix }}/share/man/man1/mm.1

demo:
  cargo build --release --bin mm --example demo
  target/release/examples/demo

vhs:
  cargo build --release --bin mm --example demo
  vhs demo.tape
