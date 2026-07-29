#!/usr/bin/env bash
#
# examples/ のプラグイン・ドライバをビルドして、デーモンが読むディレクトリへ
# インストールする。
#
#   ./scripts/install-examples.sh                    # インストール可能な全件
#   ./scripts/install-examples.sh state-reader       # 指定したものだけ
#   ./scripts/install-examples.sh -n                 # 何をするかだけ表示
#   ./scripts/install-examples.sh --list             # 対象一覧
#
# インストール先は既定で `$XDG_CONFIG_HOME/edlr/{plugins,drivers}`
# (`XDG_CONFIG_HOME` 未設定なら `~/.config/edlr/...`)。`--prefix` で変えられる
# ので、`--plugins-dir`/`--drivers-dir` を付けて起動しているデーモンにも使える。
#
# **設定値と承認状態は消えない**: それらは settings-dir / grants-dir 側にあり、
# このスクリプトが触るのはプラグインディレクトリ(wasm と manifest)だけ。
#
# **インストール後はデーモンの再起動が要る**: プラグインのロードは起動時に
# 一度だけ行われる。

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# ビルド可能な対象。`名前|種別|ソースディレクトリ|ビルド成果物|配置後の名前`
#
# 種別:
#   rust-plugin / rust-driver -- cargo で wasm32-wasip2 向けにビルドする
#   go-plugin / go-driver     -- 同梱の build.sh(TinyGo)を使う
#
# ここに載っていない `examples/plugins/*`(busy-loop / init-trap / memory-hog /
# http-caller / hello-logger)は manifest.toml を持たないテスト用フィクスチャで、
# 単体ではインストールできない。
COMPONENTS=(
  "state-reader|rust-plugin|examples/plugins/state-reader|state_reader.wasm|plugin.wasm"
  "inara-uploader|go-plugin|examples/plugins/inara-uploader|plugin.wasm|plugin.wasm"
  "ed-state|rust-driver|examples/drivers/ed-state|ed_state.wasm|driver.wasm"
  "tutorial-jump-log-rs|rust-plugin|examples/plugins/tutorial-jump-log-rs|tutorial_jump_log.wasm|plugin.wasm"
  "tutorial-tracker-rs|rust-driver|examples/drivers/tutorial-tracker-rs|tutorial_tracker.wasm|driver.wasm"
  "tutorial-jump-log-go|go-plugin|examples/plugins/tutorial-jump-log-go|plugin.wasm|plugin.wasm"
  "tutorial-tracker-go|go-driver|examples/drivers/tutorial-tracker-go|driver.wasm|driver.wasm"
)

config_home="${XDG_CONFIG_HOME:-$HOME/.config}"
prefix="$config_home/edlr"
dry_run=0

die() {
  echo "error: $*" >&2
  exit 1
}

say() {
  echo "==> $*"
}

# 実行するか、`-n` なら表示するだけ。
run() {
  if [[ $dry_run -eq 1 ]]; then
    echo "    (dry-run) $*"
  else
    "$@"
  fi
}

usage() {
  sed -n '3,20p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
  echo
  echo "options:"
  echo "  --prefix DIR   インストール先の親(既定: $config_home/edlr)"
  echo "  -n, --dry-run  実際には何もせず、行う操作を表示する"
  echo "  -l, --list     インストール可能な対象を一覧する"
  echo "  -h, --help     このヘルプ"
}

list_components() {
  printf '%-22s %-12s %s\n' NAME KIND SOURCE
  local entry name kind dir
  for entry in "${COMPONENTS[@]}"; do
    IFS='|' read -r name kind dir _ _ <<<"$entry"
    printf '%-22s %-12s %s\n' "$name" "$kind" "$dir"
  done
}

# 指定名のエントリを返す(見つからなければ非ゼロ)。
find_component() {
  local wanted="$1" entry name
  for entry in "${COMPONENTS[@]}"; do
    IFS='|' read -r name _ _ _ _ <<<"$entry"
    if [[ "$name" == "$wanted" ]]; then
      echo "$entry"
      return 0
    fi
  done
  return 1
}

require_cmd() {
  command -v "$1" >/dev/null 2>&1 || die "$1 が PATH に無い ($2)"
}

# cargo で wasm32-wasip2 向けにビルドし、成果物のパスを stdout へ返す。
build_rust() {
  local dir="$1" artifact="$2"
  require_cmd cargo "https://rustup.rs"
  ( cd "$repo_root/$dir" && cargo build --release --target wasm32-wasip2 >&2 )
  echo "$repo_root/$dir/target/wasm32-wasip2/release/$artifact"
}

# 同梱の build.sh(TinyGo)でビルドし、成果物のパスを stdout へ返す。
build_go() {
  local dir="$1" artifact="$2"
  require_cmd tinygo "https://tinygo.org/getting-started/install/"
  ( cd "$repo_root/$dir" && ./build.sh >&2 )
  echo "$repo_root/$dir/$artifact"
}

install_component() {
  local entry="$1"
  local name kind dir artifact installed_as
  IFS='|' read -r name kind dir artifact installed_as <<<"$entry"

  local src="$repo_root/$dir"
  [[ -d "$src" ]] || die "$dir が無い"

  say "$name をビルド中 ($kind)"
  local built
  case "$kind" in
    rust-plugin|rust-driver)
      if [[ $dry_run -eq 1 ]]; then
        echo "    (dry-run) cargo build --release --target wasm32-wasip2 (in $dir)"
        built="$src/target/wasm32-wasip2/release/$artifact"
      else
        built="$(build_rust "$dir" "$artifact")"
      fi
      ;;
    go-plugin|go-driver)
      if [[ $dry_run -eq 1 ]]; then
        echo "    (dry-run) ./build.sh (in $dir)"
        built="$src/$artifact"
      else
        built="$(build_go "$dir" "$artifact")"
      fi
      ;;
    *)
      die "未知の種別: $kind"
      ;;
  esac

  if [[ $dry_run -eq 0 && ! -f "$built" ]]; then
    die "$name のビルド成果物が見つからない: $built"
  fi

  # ドライバは drivers/、プラグインは plugins/ 配下。
  local dest_parent
  case "$kind" in
    rust-driver|go-driver) dest_parent="$prefix/drivers" ;;
    *)                     dest_parent="$prefix/plugins" ;;
  esac
  local dest="$dest_parent/$name"

  say "$name を $dest へインストール中"
  run mkdir -p "$dest"
  run cp "$built" "$dest/$installed_as"

  # マニフェスト(プラグインは manifest.toml、ドライバは driver.toml)。
  local descriptor
  case "$kind" in
    rust-driver|go-driver) descriptor="driver.toml" ;;
    *)                     descriptor="manifest.toml" ;;
  esac
  [[ -f "$src/$descriptor" || $dry_run -eq 1 ]] || die "$dir/$descriptor が無い"
  run cp "$src/$descriptor" "$dest/$descriptor"

  # ダッシュボードウィジェットの静的ファイル。**先に消してから入れる**:
  # 上書きコピーだけだと、名前を変えたり消したりしたファイルが配置先に
  # 残り続ける(manifest から参照されていない古い entry が生き残る)。
  # ここで消すのはこのスクリプトが置いたものだけなので安全。
  if [[ -d "$src/ui" ]]; then
    run rm -rf "$dest/ui"
    run cp -r "$src/ui" "$dest/ui"
  fi
}

targets=()
while [[ $# -gt 0 ]]; do
  case "$1" in
    -h|--help)    usage; exit 0 ;;
    -l|--list)    list_components; exit 0 ;;
    -n|--dry-run) dry_run=1; shift ;;
    --prefix)     [[ $# -ge 2 ]] || die "--prefix には引数が要る"; prefix="$2"; shift 2 ;;
    --prefix=*)   prefix="${1#*=}"; shift ;;
    -*)           die "未知のオプション: $1 (--help を参照)" ;;
    *)            targets+=("$1"); shift ;;
  esac
done

# 引数が無ければ全件。
if [[ ${#targets[@]} -eq 0 ]]; then
  for entry in "${COMPONENTS[@]}"; do
    IFS='|' read -r name _ _ _ _ <<<"$entry"
    targets+=("$name")
  done
fi

# ビルドを始める前に名前を全部検証する(3 件目で typo に気づくのは遅い)。
selected=()
for target in "${targets[@]}"; do
  entry="$(find_component "$target")" || die "未知の対象: $target (--list を参照)"
  selected+=("$entry")
done

for entry in "${selected[@]}"; do
  install_component "$entry"
done

say "完了。デーモンを再起動すると反映される(ロードは起動時に一度だけ)。"
