FRONTEND_DIR := ui/frontend
TAURI_APP_DIR := ui
BUNDLE_DIR    := target/release/bundle
DIST_DIR      := dist
TARGET_TRIPLE := $(shell rustc -vV | sed -n 's/^host: //p')

# make           → リリースバイナリをビルドするだけ(target/release/edlr-ui, target/release/edlr)
# make install   → バイナリ(edlr-ui / edlr)を PATH(~/.cargo/bin)に配置
# make packaging → 配布用パッケージ(.deb / .rpm / .AppImage)を dist/ に生成

.PHONY: all install packaging frontend tauri-cli clean

all: frontend
	cargo build --release -p edlr-ui -p edlr-core --features edlr-ui/custom-protocol
	@echo "==> バイナリ: target/release/edlr-ui, target/release/edlr"

install: frontend
	cargo install --path ui/src-tauri --features custom-protocol
	cargo install --path core

packaging: frontend tauri-cli
	cargo fetch
	# core の edlr を sidecar(externalBin)として同梱する。Tauri は
	# <name>-<target-triple> というファイル名を要求する(issue vlxe)。
	# externalBin を tauri.conf.json に書くと sidecar ファイルが無いだけで
	# 通常の cargo build/test まで失敗するため、packaging 時だけ --config で渡す。
	cargo build --release -p edlr-core
	mkdir -p $(TAURI_APP_DIR)/src-tauri/binaries
	cp target/release/edlr $(TAURI_APP_DIR)/src-tauri/binaries/edlr-daemon-$(TARGET_TRIPLE)
	cd $(TAURI_APP_DIR) && cargo tauri build --config '{"build":{"beforeBuildCommand":""},"bundle":{"externalBin":["binaries/edlr-daemon"]}}'
	mkdir -p $(DIST_DIR)
	find $(BUNDLE_DIR) -maxdepth 2 -type f \
		\( -name '*.deb' -o -name '*.rpm' -o -name '*.AppImage' \) \
		-exec cp -v {} $(DIST_DIR)/ \;
	@echo "==> 配布物: $(DIST_DIR)/"

frontend:
	pnpm --dir $(FRONTEND_DIR) install --frozen-lockfile
	pnpm --dir $(FRONTEND_DIR) build

tauri-cli:
	@command -v cargo-tauri >/dev/null 2>&1 || cargo install tauri-cli --locked

clean:
	rm -rf $(DIST_DIR) $(FRONTEND_DIR)/dist
	cargo clean --release
