#!/usr/bin/env fish
source .venv/bin/activate.fish
set -e CARGO_TARGET_DIR
set -x LIBTORCH_USE_PYTORCH 1
set -x LIBTORCH_BYPASS_VERSION_CHECK 1
cargo run --release --example stable-diffusion-inpaint --features clap -- \
 --input-image input.png \
 --mask-image mask.png
# --input-image $XDG_RUNTIME_DIR/input.png \
# --mask-image $XDG_RUNTIME_DIR/mask.png
