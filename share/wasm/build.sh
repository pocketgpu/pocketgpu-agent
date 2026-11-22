#!/usr/bin/env bash
# Builds the guest page's wasm (F48) into share/src/wasm/ for Vite to bundle.
# Needs: rustup target add wasm32-unknown-unknown; cargo install wasm-bindgen-cli --version 0.2.122
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
want="0.2.122"
have="$(wasm-bindgen --version 2>/dev/null | awk '{print $2}')"
if [ "$have" != "$want" ]; then
  echo "wasm-bindgen $want is needed (found: ${have:-none}): cargo install wasm-bindgen-cli --version $want --locked" >&2
  exit 1
fi
# macOS's ar can't index wasm objects (ring's C code); Rust's own llvm-ar can
# (rustup component add llvm-tools). Linux runners with llvm-ar need nothing.
if [ "$(uname)" = "Darwin" ] && [ -z "${AR_wasm32_unknown_unknown:-}" ]; then
  sysroot="$(rustc --print sysroot)"
  host="$(rustc -vV | awk '/^host/ {print $2}')"
  export AR_wasm32_unknown_unknown="$sysroot/lib/rustlib/$host/bin/llvm-ar"
fi
cargo build --manifest-path "$here/Cargo.toml" --target wasm32-unknown-unknown --release
wasm-bindgen "$here/target/wasm32-unknown-unknown/release/pocketgpu_share_wasm.wasm" \
  --out-dir "$here/../src/wasm" --target web --weak-refs
echo "built $(du -h "$here/../src/wasm/pocketgpu_share_wasm_bg.wasm" | cut -f1) into share/src/wasm/"
