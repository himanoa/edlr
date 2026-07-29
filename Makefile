FRONTEND_DIR := ui/frontend
TAURI_APP_DIR := ui
BUNDLE_DIR    := target/release/bundle
DIST_DIR      := dist

# make           → リリースバイナリをビルドするだけ(target/release/edlr-ui, target/release/edlr)
# make install   → バイナリ(edlr-ui / edlr)を PATH(~/.cargo/bin)に配置
# make packaging → 配布用パッケージ(.deb / .rpm / .AppImage)を dist/ に生成

.PHONY: all install packaging frontend tauri-cli clean

all: frontend
	cargo build --release -p edlr-ui -p edlr-core
	@echo "==> バイナリ: target/release/edlr-ui, target/release/edlr"

install: frontend
	cargo install --path ui/src-tauri
	cargo install --path core

packaging: frontend tauri-cli
	cargo fetch
	cd $(TAURI_APP_DIR) && cargo tauri build --config '{"build":{"beforeBuildCommand":""}}'
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
