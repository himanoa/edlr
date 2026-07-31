#!/usr/bin/env bash
# tutorial-jump-log を wasm コンポーネントへビルドする。
#
# MoonBit の出力はコアモジュールなので、`wasm-tools component embed` +
# `component new` でコンポーネントへ包む。**`--encoding utf16` を忘れないこと**
# (MoonBit の文字列は UTF-16。utf8 のまま包むとロードは通るがログや
# payload がすべて文字化けする)。詳細は docs/plugin-tutorial-moonbit.md の
# 2 章を参照。
#
# 必要なもの:
#   - MoonBit toolchain(https://www.moonbitlang.com、moon 0.1.20260309 で確認)
#   - wasm-tools(cargo install wasm-tools、1.254.0 で確認)

set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
wit="$here/../../../core/wit"
out="${1:-$here/plugin.wasm}"

for tool in moon wasm-tools; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    echo "error: $tool not found in PATH" >&2
    exit 1
  fi
done

cd "$here"
moon build --target wasm --release

core_wasm="$here/_build/wasm/release/build/gen/gen.wasm"
embedded="$(mktemp)"
trap 'rm -f "$embedded"' EXIT
wasm-tools component embed "$wit" "$core_wasm" \
  -w plugin-guest --encoding utf16 -o "$embedded"
wasm-tools component new "$embedded" -o "$out"

echo "built: $out"
