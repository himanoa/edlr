# inara-uploader Live 版ゲート 実装計画

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Live(gameversion メジャー >= 4 かつ beta でない)以外のセッションのイベントを Inara へ送らない。

**Architecture:** `mapping.State` が `LoadGame.gameversion` を学習し、`Convert` の出口でゲート(非 Live / 未学習なら `Events` を空にする)。判定は純関数 `isLiveVersion`。

**Tech Stack:** Go(TinyGo/wasm32-wasip2 でビルド)。

**Spec:** `docs/superpowers/specs/2026-07-28-inara-uploader-live-version-gate-design.md`

## Global Constraints

- 未学習(LoadGame 前)は送らない。学習(identity/version)はゲートより先に行う。
- ブランチ `inara-live-gate`。TDD。cargo/go テストは並走させない。

---

### Task 1: `isLiveVersion` 純関数

**Files:** Create: `examples/plugins/inara-uploader/mapping/version.go` / Test: `version_test.go`

**Interfaces:** Produces `func isLiveVersion(gameversion string) bool`

- [ ] Step 1: 表形式の失敗するテスト(`"4.0.0.1904"`/`"4.1.2"` true、`"3.8.0.404"`/`"4.0 beta"`/`"Beta"`/`""`/`"garbage"` false)
- [ ] Step 2: `cd examples/plugins/inara-uploader && go test ./mapping/` で失敗確認
- [ ] Step 3: 実装(先頭数字列をメジャーとしてパース >= 4、`strings.Contains(strings.ToLower(v), "beta")` で beta 除外)
- [ ] Step 4: パス確認
- [ ] Step 5: Commit `feat(inara-uploader): add live game version predicate`

### Task 2: State 学習 + Convert ゲート

**Files:** Modify: `mapping/state.go`(`gameVersion` フィールド + `learnGameVersion` + `liveAllowed()`)、`mapping/identity.go`(`loadGame` に `GameVersion string \`json:"gameversion"\``、convert で学習)、`mapping/mapping.go`(`Convert` の出口で `if !st.liveAllowed() { res.Events = nil }`)/ Test: `mapping_test.go` 追記

**Interfaces:** Consumes `isLiveVersion`。Produces: ゲート済み `Convert`(シグネチャ不変)

- [ ] Step 1: 失敗するテスト: (a) LoadGame 前の FSDJump は Events 空 (b) Legacy LoadGame(gameversion "3.8.0.404")後の FSDJump も空、LoadGame 自身の setCommanderCredits も空 (c) Live LoadGame("4.0.0.1904")後は従来どおり変換される (d) Legacy でも identity は学習される(State.CommanderName が入る)
- [ ] Step 2: 失敗確認
- [ ] Step 3: 実装
- [ ] Step 4: `go test ./...` 全パス確認
- [ ] Step 5: Commit `feat(inara-uploader): drop events from non-live game sessions`

### Task 3: wasm 再ビルドと配備

- [ ] Step 1: `build.sh` で plugin.wasm 再ビルド
- [ ] Step 2: `~/.config/edlr/plugins/inara-uploader/plugin.wasm` に配備(manifest 変更なし → grant 維持)
- [ ] Step 3: `go test ./...` + リポジトリ全テストに影響ないこと確認
- [ ] Step 4: Commit(plugin.wasm の扱いは既存リポジトリ方針に従う — コミット済みの実物があるなら更新)
