#!/usr/bin/env bash
# tutorial-tracker を wasm コンポーネントへビルドする。
#
# ビルド対象の world は `plugin` ではなく `driver-guest`(= `plugin` に WASI の
# import 一式を足したもの)。Go/TinyGo の標準ライブラリは、プラグインが何も
# 呼ばなくても WASI を import するため、`plugin` を直接対象にするとコンポーネント
# 化が失敗する。詳細は docs/plugin-tutorial-tinygo.md の 6 章を参照。
#
# 必要なもの:
#   - TinyGo 0.34 以降(https://tinygo.org、0.41.1 で確認)
#   - Go 1.23 以降(TinyGo が内部で使う)

set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
wit="$here/../../../core/wit"
out="${1:-$here/driver.wasm}"

if ! command -v tinygo >/dev/null 2>&1; then
  echo "error: tinygo not found in PATH (https://tinygo.org/getting-started/install/)" >&2
  exit 1
fi

cd "$here"
tinygo build -target=wasip2 \
  --wit-package "$wit" \
  --wit-world driver-guest \
  -o "$out" .

echo "built: $out"
