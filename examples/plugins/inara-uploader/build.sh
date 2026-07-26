#!/usr/bin/env bash
# inara-uploader を wasm コンポーネントへビルドする。
#
# なぜスクリプトが要るのか:
#   TinyGo/Go の標準ライブラリは、何もしなくても WASI の一部
#   (wasi:cli/environment、wasi:clocks/wall-clock など)を import する。
#   一方 `core/wit/plugin.wit` の `world plugin` は edlr 独自の 4
#   インターフェースしか宣言していないため、`wasm-tools component new` が
#   「world に無い import がある」としてコンポーネント化を拒否する。
#
#   そこでビルド時だけ、`plugin.wit` に `include wasi:cli/imports@0.2.0;`
#   を差し込んだ**オーバーレイ**を組み立てて、そちらを --wit-package に渡す。
#   WASI の WIT 定義は TinyGo 同梱のものを使うので、リポジトリに WASI の
#   WIT を vendoring せずに済む(= 本体の plugin.wit が唯一の真実であり続ける)。
#
#   Rust プラグイン(examples/plugins/hello-logger)にこの手当てが要らないのは、
#   wasm32-wasip2 ターゲットのリンカが WASI import を自動で足してくれるため。
#   ホスト側は wasmtime_wasi の add_to_linker_sync で WASI を提供しているので、
#   出来上がったコンポーネントはそのままロードできる。
#
# 必要なもの:
#   - TinyGo 0.34 以降(https://tinygo.org)
#   - wasm-tools(TinyGo が内部で呼ぶ。PATH に無ければ TinyGo が同梱のものを使う)
#   - 生成済みバインディング(gen/。再生成は README の「バインディングの再生成」参照)

set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_wit="$here/../../../core/wit"
out="${1:-$here/plugin.wasm}"
overlay="$here/build/wit"

if ! command -v tinygo >/dev/null 2>&1; then
  echo "error: tinygo not found in PATH (https://tinygo.org/getting-started/install/)" >&2
  exit 1
fi

tinygoroot="$(tinygo env TINYGOROOT)"
wasi_wit="$tinygoroot/lib/wasi-cli/wit"
if [ ! -d "$wasi_wit" ]; then
  echo "error: WASI の WIT が見つかりません: $wasi_wit" >&2
  echo "       TinyGo のバージョンが想定と違う可能性があります" >&2
  exit 1
fi

rm -rf "$overlay"
mkdir -p "$overlay/deps/cli"

# world に WASI の import 一式を足したコピーを作る。plugin.wit 本体は触らない。
awk '
  /^world plugin \{/ {
    print
    print "  // build.sh が差し込む: TinyGo/Go の標準ライブラリが要求する WASI import 一式。"
    print "  include wasi:cli/imports@0.2.0;"
    print ""
    next
  }
  { print }
' "$repo_wit/plugin.wit" > "$overlay/plugin.wit"

cp "$wasi_wit"/*.wit "$overlay/deps/cli/"
cp -r "$wasi_wit"/deps/* "$overlay/deps/"

cd "$here"
tinygo build -target=wasip2 \
  --wit-package "$overlay" \
  --wit-world plugin \
  -o "$out" .

echo "built: $out"
