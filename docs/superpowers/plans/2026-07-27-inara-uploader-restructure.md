# inara-uploader 再構成 実装計画

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `examples/plugins/inara-uploader` を、replay / live のモード分離とリングバッファ化したキューを持つ、テスト可能な 4 パッケージ構成へ作り直す。

**Architecture:** 判断を `main` の外へ出す。`main` はホスト境界のアダプタ(設定を読む・バイト列を送る・結果をログへ整形する)に徹し、`uploader` がキューとフラッシュ判断、`mapping` が Journal→INARA 変換、`inara` がリクエスト組み立てと応答解釈を持つ。ロジック層はログを吐かず、何が起きたかを `Outcome` として値で返す。JSON は `map[string]any` ではなくイベントごとの構造体へデコードする。

**Tech Stack:** Go 1.23(TinyGo 0.34+ で wasip2 へビルド)、標準ライブラリのみ、`go.bytecodealliance.org/cm`(生成バインディング用)。

設計書: `docs/superpowers/specs/2026-07-27-inara-uploader-restructure-design.md`

## Global Constraints

- 作業ディレクトリはすべて `examples/plugins/inara-uploader/`。パスはそこからの相対。
- **`go.mod` に依存を追加しない**。現在の依存は `go.bytecodealliance.org/cm v0.3.0` のみ。テスト用の TOML パーサも追加しない。
- **`package main` にテストを書かない / 書けない**。`//go:wasmimport` を含むためネイティブでリンクできない。`main` の検証は `go vet ./...` の型チェックまで。
- 検証コマンドは 2 つ: `go test ./...`(ロジック層)と `go vet ./...`(`main` を含む全パッケージの型チェック、exit 0 が期待値)。
- **`go build ./...` は使わない**。`main` のリンクで `relocation target ... wasmimport_Send not defined` が出るのが正常。
- **TinyGo と `wasm-tools` は作業環境に無い**。`plugin.wasm` の再ビルドと実機確認はこの計画のスコープ外。
- コメントは日本語。既存コード(`settings/settings.go`)の粒度に合わせ、「何をするか」ではなく「なぜそうするか」を書く。
- 定数の実値: `appName = "edlr-inara-uploader"`、`appVersion = "0.1.0"`、`inaraEndpoint = "https://inara.cz/inapi/v1/"`、`MaxQueued = 200`、`ReplayBatchSize = 100`。
- 既存の `settings` パッケージは変更しない。

## File Structure

| ファイル | 責務 |
|---|---|
| `inara/inara.go` | INARA API v1 のリクエスト型・`Encode` |
| `inara/response.go` | 応答の解釈(`Interpret`、`Rejection`、`BatchError`) |
| `mapping/mapping.go` | ハンドラレジストリ、`Convert`、`Names`、`handlerFor` |
| `mapping/state.go` | イベントをまたぐ状態(`State`) |
| `mapping/identity.go` | `Commander` / `LoadGame` |
| `mapping/travel.go` | `FSDJump` / `CarrierJump` / `Docked` / `Location` |
| `mapping/ranks.go` | `Rank` / `Progress` / `Reputation` / `EngineerProgress` |
| `mapping/inventory.go` | `Materials` / `Statistics` |
| `mapping/combat.go` | `Died` |
| `uploader/queue.go` | 直近 N 件を保持するリングバッファ |
| `uploader/uploader.go` | モード遷移・フラッシュ判断・送信・`Outcome` |
| `main.go` | ホスト境界のアダプタ |
| 削除 | `mapping.go`(旧)、`inara.go`(旧) |

依存は一方向: `main` → `uploader` → `mapping` / `inara` / `settings`。

## 設計書からの逸脱(2 点)

### `Outcome` に 2 フィールド足す

設計書の `Outcome` に加えて `Pending int`(キューに残っている件数)と `Fatal bool`(`Err` が恒久的でキューを捨てたこと)を持つ。前者は `main` が「holding N event(s)」を出すために要る。後者はログレベルの決定に要り、`main` で `errors.As(err, &inara.BatchError{})` を書くと判断がテストできない場所に残るため。

### `handler` の関数を 1 つにする

設計書の `handler` は `learn` と `convert` の 2 つの関数を持つが、実装では **`convert` 1 つに統合する**。`LoadGame` のように学習と変換を両方行うイベントで同じペイロードを 2 回デコードすることになるため。State への書き込み(コマンダー名の学習など)は `convert` の中で行い、送信対象でないイベント(`Commander`)は `nil` を返す。設計書の意図(3 か所に散った関心を 1 つのレジストリに集める)は変わらない。

---

### Task 1: `inara` パッケージ

**Files:**
- Create: `inara/inara.go`
- Create: `inara/response.go`
- Test: `inara/response_test.go`, `inara/inara_test.go`

**Interfaces:**
- Consumes: なし
- Produces:
  - `inara.Event{Name, Timestamp string; Data any}` — `eventName` / `eventTimestamp` / `eventData` へマーシャルされる
  - `inara.New(name string, data any) Event` — `Timestamp` は空のまま(`mapping` が後で埋める)
  - `inara.Header{AppName, AppVersion string; IsBeingDeveloped bool; APIKey, CommanderName, FrontierID string}`
  - `inara.Encode(h Header, events []Event) ([]byte, error)`
  - `inara.Interpret(status int, body []byte) (Result, error)`
  - `inara.Result{Rejected []Rejection}`
  - `inara.Rejection{Index int; Status int; StatusText string}`
  - `inara.BatchError{Status int; StatusText string}` — `*BatchError` が `error` を実装

- [ ] **Step 1: `inara/inara.go` を書く**

```go
// Package inara は INARA API v1 のリクエスト組み立てと応答の解釈を持つ。
//
// ホストの import には依存しない。送信そのもの(driver-http の呼び出し)は
// main が行い、このパッケージは「何を送るか」と「返ってきたものをどう読むか」
// だけを扱う。
package inara

import "encoding/json"

// Event は INARA API v1 の 1 イベント。
//
// Timestamp は mapping が Journal イベントの timestamp で埋める。個々の
// マッパーに timestamp を配って回らずに済ませるため、New では設定しない。
type Event struct {
	Name      string `json:"eventName"`
	Timestamp string `json:"eventTimestamp"`
	Data      any    `json:"eventData"`
}

// New は timestamp 未設定の Event を作る。
func New(name string, data any) Event {
	return Event{Name: name, Data: data}
}

// Header は INARA がリクエストごとに要求する識別情報。
type Header struct {
	AppName          string `json:"appName"`
	AppVersion       string `json:"appVersion"`
	IsBeingDeveloped bool   `json:"isBeingDeveloped"`
	APIKey           string `json:"APIkey"`
	CommanderName    string `json:"commanderName"`
	FrontierID       string `json:"commanderFrontierID,omitempty"`
}

type request struct {
	Header Header  `json:"header"`
	Events []Event `json:"events"`
}

// Encode は送信する JSON を組み立てる。
func Encode(h Header, events []Event) ([]byte, error) {
	return json.Marshal(request{Header: h, Events: events})
}
```

- [ ] **Step 2: `inara/inara_test.go` を書く**

```go
package inara

import (
	"encoding/json"
	"testing"
)

func TestEncodeUsesInaraFieldNames(t *testing.T) {
	body, err := Encode(
		Header{AppName: "app", APIKey: "k", CommanderName: "cmdr"},
		[]Event{{Name: "setCommanderCredits", Timestamp: "2026-07-27T00:00:00Z", Data: map[string]int{"commanderCredits": 1}}},
	)
	if err != nil {
		t.Fatalf("Encode failed: %v", err)
	}

	var got map[string]any
	if err := json.Unmarshal(body, &got); err != nil {
		t.Fatalf("Encode produced invalid JSON: %v", err)
	}
	header := got["header"].(map[string]any)
	if header["APIkey"] != "k" {
		t.Errorf("api key must be marshalled as APIkey, got %v", header)
	}
	events := got["events"].([]any)
	first := events[0].(map[string]any)
	if first["eventName"] != "setCommanderCredits" || first["eventTimestamp"] != "2026-07-27T00:00:00Z" {
		t.Errorf("unexpected event shape: %v", first)
	}
}

// FrontierID は Journal から学習できていないことがあるので、空なら落とす。
func TestEncodeOmitsEmptyFrontierID(t *testing.T) {
	body, err := Encode(Header{CommanderName: "cmdr"}, nil)
	if err != nil {
		t.Fatalf("Encode failed: %v", err)
	}
	var got struct {
		Header map[string]any `json:"header"`
	}
	if err := json.Unmarshal(body, &got); err != nil {
		t.Fatal(err)
	}
	if _, ok := got.Header["commanderFrontierID"]; ok {
		t.Errorf("empty frontier id must be omitted, got %v", got.Header)
	}
}
```

- [ ] **Step 3: テストを走らせて通ることを確認する**

Run: `go test ./inara/ -v`
Expected: PASS(2 件)

- [ ] **Step 4: `inara/response_test.go` を書く(失敗する状態)**

```go
package inara

import (
	"errors"
	"testing"
)

const okBody = `{"header":{"eventStatus":200,"eventStatusText":"OK"},"events":[{"eventStatus":200}]}`

func TestInterpretAcceptsSuccessfulBatch(t *testing.T) {
	res, err := Interpret(200, []byte(okBody))
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if len(res.Rejected) != 0 {
		t.Errorf("expected no rejections, got %v", res.Rejected)
	}
}

// バッチ全体の拒否は恒久的(API キー不正など)。呼び出し側はキューを捨てる。
func TestInterpretReportsBatchRejectionAsBatchError(t *testing.T) {
	body := `{"header":{"eventStatus":400,"eventStatusText":"Invalid API key"},"events":[]}`
	_, err := Interpret(200, []byte(body))

	var batch *BatchError
	if !errors.As(err, &batch) {
		t.Fatalf("expected a *BatchError, got %v", err)
	}
	if batch.Status != 400 || batch.StatusText != "Invalid API key" {
		t.Errorf("unexpected batch error: %+v", batch)
	}
}

// 個別イベントの拒否はバッチ全体の失敗ではない。報告だけして再送はしない。
func TestInterpretReportsRejectedEvents(t *testing.T) {
	body := `{"header":{"eventStatus":200},"events":[{"eventStatus":200},{"eventStatus":400,"eventStatusText":"nope"}]}`
	res, err := Interpret(200, []byte(body))
	if err != nil {
		t.Fatalf("individual rejections must not fail the batch: %v", err)
	}
	if len(res.Rejected) != 1 {
		t.Fatalf("expected 1 rejection, got %v", res.Rejected)
	}
	if res.Rejected[0].Index != 1 || res.Rejected[0].Status != 400 || res.Rejected[0].StatusText != "nope" {
		t.Errorf("unexpected rejection: %+v", res.Rejected[0])
	}
}

// 非 2xx と壊れた JSON は一時的な失敗として扱う(BatchError ではないので再送される)。
func TestInterpretRejectsNon2xx(t *testing.T) {
	_, err := Interpret(503, []byte(okBody))
	if err == nil {
		t.Fatal("expected an error for HTTP 503")
	}
	var batch *BatchError
	if errors.As(err, &batch) {
		t.Error("an HTTP-level failure must be retryable, not a BatchError")
	}
}

func TestInterpretRejectsUnparsableBody(t *testing.T) {
	if _, err := Interpret(200, []byte("not json")); err == nil {
		t.Fatal("expected an error for an unparsable body")
	}
}
```

- [ ] **Step 5: テストが失敗することを確認する**

Run: `go test ./inara/ -run Interpret -v`
Expected: FAIL(`undefined: Interpret`, `undefined: BatchError`)

- [ ] **Step 6: `inara/response.go` を書く**

```go
package inara

import (
	"encoding/json"
	"fmt"
)

// response は INARA の応答のうち、このプラグインが解釈する部分。
type response struct {
	Header struct {
		EventStatus     int    `json:"eventStatus"`
		EventStatusText string `json:"eventStatusText"`
	} `json:"header"`
	Events []struct {
		EventStatus     int    `json:"eventStatus"`
		EventStatusText string `json:"eventStatusText"`
	} `json:"events"`
}

// Rejection は INARA が個別に拒否したイベント。Index はバッチ内の位置。
type Rejection struct {
	Index      int
	Status     int
	StatusText string
}

// Result は応答の解釈結果。
type Result struct {
	Rejected []Rejection
}

// BatchError はバッチ全体が拒否されたことを表す。API キー不正などの恒久的な
// 失敗なので、同じ内容を送り直しても通らない。呼び出し側はキューを捨てる。
type BatchError struct {
	Status     int
	StatusText string
}

func (e *BatchError) Error() string {
	return fmt.Sprintf("inara rejected the batch: %d %s", e.Status, e.StatusText)
}

// Interpret は HTTP ステータスと応答本文を解釈する。
//
// *BatchError 以外のエラーは一時的な失敗として扱ってよい(呼び出し側は
// キューを保持して再試行する)。
func Interpret(status int, body []byte) (Result, error) {
	if status < 200 || status >= 300 {
		return Result{}, fmt.Errorf("inara returned HTTP %d", status)
	}

	var parsed response
	if err := json.Unmarshal(body, &parsed); err != nil {
		return Result{}, fmt.Errorf("unparsable inara response: %w", err)
	}

	if parsed.Header.EventStatus != 200 {
		return Result{}, &BatchError{
			Status:     parsed.Header.EventStatus,
			StatusText: parsed.Header.EventStatusText,
		}
	}

	var res Result
	for i, ev := range parsed.Events {
		if ev.EventStatus != 200 {
			res.Rejected = append(res.Rejected, Rejection{
				Index:      i,
				Status:     ev.EventStatus,
				StatusText: ev.EventStatusText,
			})
		}
	}
	return res, nil
}
```

- [ ] **Step 7: テストが通ることを確認する**

Run: `go test ./inara/ -v`
Expected: PASS(7 件)

- [ ] **Step 8: コミット**

```bash
git add inara/
git commit -m "feat(examples/inara): add the inara package for requests and responses"
```

---

### Task 2: `mapping` の骨組みと識別子・移動イベント

**Files:**
- Create: `mapping/state.go`, `mapping/mapping.go`, `mapping/identity.go`, `mapping/travel.go`
- Test: `mapping/identity_test.go`, `mapping/travel_test.go`, `mapping/mapping_test.go`

**Interfaces:**
- Consumes: `inara.Event`, `inara.New`(Task 1)
- Produces:
  - `mapping.State{CommanderName, FrontierID, LastSystem string}` + `mapping.NewState() *State`
  - `mapping.Result{Events []inara.Event; FlushLive bool}`
  - `mapping.Convert(name, timestamp string, payload json.RawMessage, st *State) (Result, error)`
  - `mapping.Names() []string` — 購読すべき Journal イベント名(ソート済み)

- [ ] **Step 1: `mapping/state.go` を書く**

```go
package mapping

// State はイベントをまたいで覚えておく情報。プラグインのプロセス内にしか無く、
// 永続化はされない(デーモンを再起動すると Journal の replay で埋め直される)。
type State struct {
	CommanderName string
	FrontierID    string
	// LastSystem は直近に確認した星系。Died は星系名を含まないため、
	// 移動系イベントから覚えておいたものを添える。
	LastSystem string

	// ranks / progress は INARA の rankName をキーにした段位と進捗。
	// Journal では別イベントで来るため、揃うまで送れない(ranks.go 参照)。
	ranks    map[string]int
	progress map[string]float64
}

func NewState() *State {
	return &State{
		ranks:    map[string]int{},
		progress: map[string]float64{},
	}
}

// learnIdentity は空でない値だけを取り込む。Journal のイベントによっては
// 片方しか入っていないため、上書きで消さないようにする。
func (s *State) learnIdentity(name, frontierID string) {
	if name != "" {
		s.CommanderName = name
	}
	if frontierID != "" {
		s.FrontierID = frontierID
	}
}
```

- [ ] **Step 2: `mapping/mapping.go` を書く(識別子と移動イベントのみ登録)**

```go
// Package mapping は Journal イベントを INARA API v1 のイベントへ変換する。
//
// 対応するイベントは handlers がすべて。ここに無い Journal イベントは
// デコードすらせずに捨てる。manifest.toml の `events` と handlers のキーが
// 一致することは manifest_test.go が検証する。
package mapping

import (
	"encoding/json"
	"fmt"
	"sort"

	"github.com/himanoa/edlr/examples/plugins/inara-uploader/inara"
)

// converter は 1 つの Journal イベントのペイロードに対応する型。
//
// convert は State を書き換えてよい(コマンダー名の学習や、直近の星系の
// 記録がこれにあたる)。送信するものが無ければ nil を返す。
type converter interface {
	convert(st *State) []inara.Event
}

// handler は 1 つの Journal イベントの扱い。
type handler struct {
	// convert はペイロードのデコードと変換(送信対象でないイベントは nil)
	convert func(raw json.RawMessage, st *State) ([]inara.Event, error)
	// flushLive は live モードで即時フラッシュを促すか(Shutdown のみ true)
	flushLive bool
}

// handlerFor は converter を実装した型 T へデコードして変換する handler を作る。
func handlerFor[T converter]() handler {
	return handler{
		convert: func(raw json.RawMessage, st *State) ([]inara.Event, error) {
			var v T
			if err := json.Unmarshal(raw, &v); err != nil {
				return nil, err
			}
			return v.convert(st), nil
		},
	}
}

var handlers = map[string]handler{
	"Commander":   handlerFor[commander](),
	"LoadGame":    handlerFor[loadGame](),
	"FSDJump":     handlerFor[fsdJump](),
	"CarrierJump": handlerFor[carrierJump](),
	"Docked":      handlerFor[docked](),
	"Location":    handlerFor[location](),
}

// Result は Journal イベント 1 件を扱った結果。
type Result struct {
	Events    []inara.Event
	FlushLive bool
}

// Convert は Journal イベント 1 件を INARA のイベントへ変換する。
// 未知のイベントは空の Result を返す(エラーではない)。
func Convert(name, timestamp string, payload json.RawMessage, st *State) (Result, error) {
	h, ok := handlers[name]
	if !ok {
		return Result{}, nil
	}

	res := Result{FlushLive: h.flushLive}
	if h.convert == nil {
		return res, nil
	}

	events, err := h.convert(payload, st)
	if err != nil {
		return res, fmt.Errorf("%s: %w", name, err)
	}
	// timestamp はここでまとめて埋める。個々のマッパーに配って回らずに済む。
	for i := range events {
		events[i].Timestamp = timestamp
	}
	res.Events = events
	return res, nil
}

// Names は購読すべき Journal イベント名を返す。manifest.toml の `events` と
// 一致していること。
func Names() []string {
	names := make([]string, 0, len(handlers))
	for name := range handlers {
		names = append(names, name)
	}
	sort.Strings(names)
	return names
}
```

- [ ] **Step 3: `mapping/identity.go` を書く**

```go
package mapping

import "github.com/himanoa/edlr/examples/plugins/inara-uploader/inara"

// commander はコマンダー名の学習だけに使う(送信するものは無い)。
type commander struct {
	Name string `json:"Name"`
	FID  string `json:"FID"`
}

func (c commander) convert(st *State) []inara.Event {
	st.learnIdentity(c.Name, c.FID)
	return nil
}

type loadGame struct {
	Commander string `json:"Commander"`
	FID       string `json:"FID"`
	Credits   *int64 `json:"Credits"`
	Loan      *int64 `json:"Loan"`
}

// credits の Loan はポインタ。0 が有効値(借金なし)なので omitempty では
// 「借金を返した」と「Journal に入っていない」を区別できない。
type credits struct {
	Credits int64  `json:"commanderCredits"`
	Loan    *int64 `json:"commanderLoan,omitempty"`
}

func (g loadGame) convert(st *State) []inara.Event {
	st.learnIdentity(g.Commander, g.FID)
	if g.Credits == nil {
		return nil
	}
	return []inara.Event{inara.New("setCommanderCredits", credits{
		Credits: *g.Credits,
		Loan:    g.Loan,
	})}
}
```

- [ ] **Step 4: `mapping/travel.go` を書く**

```go
package mapping

import "github.com/himanoa/edlr/examples/plugins/inara-uploader/inara"

type fsdJump struct {
	StarSystem string      `json:"StarSystem"`
	JumpDist   float64     `json:"JumpDist"`
	StarPos    *[3]float64 `json:"StarPos"`
}

type travelJump struct {
	System   string      `json:"starsystemName"`
	Distance float64     `json:"jumpDistance,omitempty"`
	Coords   *[3]float64 `json:"starsystemCoords,omitempty"`
}

func (j fsdJump) convert(st *State) []inara.Event {
	if j.StarSystem == "" {
		return nil
	}
	st.LastSystem = j.StarSystem
	return []inara.Event{inara.New("addCommanderTravelFSDJump", travelJump{
		System:   j.StarSystem,
		Distance: j.JumpDist,
		Coords:   j.StarPos,
	})}
}

// station は星系と(あれば)ステーションを持つ Journal イベントの共通部分。
// CarrierJump / Docked / Location はいずれもこの形。
type station struct {
	StarSystem  string `json:"StarSystem"`
	StationName string `json:"StationName"`
	MarketID    *int64 `json:"MarketID"`
}

type travelStation struct {
	System   string `json:"starsystemName"`
	Station  string `json:"stationName,omitempty"`
	MarketID *int64 `json:"marketID,omitempty"`
}

func (s station) event(name string, st *State) []inara.Event {
	if s.StarSystem == "" {
		return nil
	}
	st.LastSystem = s.StarSystem
	return []inara.Event{inara.New(name, travelStation{
		System:   s.StarSystem,
		Station:  s.StationName,
		MarketID: s.MarketID,
	})}
}

type carrierJump station

func (c carrierJump) convert(st *State) []inara.Event {
	return station(c).event("addCommanderTravelCarrierJump", st)
}

type docked station

func (d docked) convert(st *State) []inara.Event {
	// ドックはステーション名が要る。無ければ送らない。
	if d.StationName == "" {
		return nil
	}
	return station(d).event("addCommanderTravelDock", st)
}

type location station

func (l location) convert(st *State) []inara.Event {
	return station(l).event("setCommanderTravelLocation", st)
}
```

- [ ] **Step 5: テストを書く**

`mapping/identity_test.go`:

```go
package mapping

import (
	"encoding/json"
	"testing"
)

// convertOne は 1 イベントを変換するテストヘルパー。
func convertOne(t *testing.T, st *State, name, payload string) Result {
	t.Helper()
	res, err := Convert(name, "2026-07-27T00:00:00Z", json.RawMessage(payload), st)
	if err != nil {
		t.Fatalf("Convert(%s) failed: %v", name, err)
	}
	return res
}

func TestCommanderLearnsIdentityWithoutSendingAnything(t *testing.T) {
	st := NewState()
	res := convertOne(t, st, "Commander", `{"Name":"Hutton","FID":"F123"}`)

	if len(res.Events) != 0 {
		t.Errorf("Commander must not produce inara events, got %v", res.Events)
	}
	if st.CommanderName != "Hutton" || st.FrontierID != "F123" {
		t.Errorf("identity was not learned: %+v", st)
	}
}

func TestLoadGameLearnsIdentityAndSendsCredits(t *testing.T) {
	st := NewState()
	res := convertOne(t, st, "LoadGame", `{"Commander":"Hutton","FID":"F123","Credits":1000,"Loan":0}`)

	if st.CommanderName != "Hutton" {
		t.Errorf("LoadGame must learn the commander name, got %q", st.CommanderName)
	}
	if len(res.Events) != 1 || res.Events[0].Name != "setCommanderCredits" {
		t.Fatalf("unexpected events: %+v", res.Events)
	}
	if res.Events[0].Timestamp != "2026-07-27T00:00:00Z" {
		t.Errorf("Convert must stamp the timestamp, got %q", res.Events[0].Timestamp)
	}

	body, _ := json.Marshal(res.Events[0].Data)
	// Loan は 0 でも送る(借金を返したことを INARA に反映させるため)。
	if string(body) != `{"commanderCredits":1000,"commanderLoan":0}` {
		t.Errorf("unexpected credits payload: %s", body)
	}
}

func TestLoadGameWithoutCreditsSendsNothing(t *testing.T) {
	st := NewState()
	res := convertOne(t, st, "LoadGame", `{"Commander":"Hutton"}`)
	if len(res.Events) != 0 {
		t.Errorf("expected no events without Credits, got %+v", res.Events)
	}
}

func TestLoanIsOmittedWhenAbsent(t *testing.T) {
	st := NewState()
	res := convertOne(t, st, "LoadGame", `{"Commander":"Hutton","Credits":5}`)
	body, _ := json.Marshal(res.Events[0].Data)
	if string(body) != `{"commanderCredits":5}` {
		t.Errorf("an absent loan must be omitted, got %s", body)
	}
}
```

`mapping/travel_test.go`:

```go
package mapping

import (
	"encoding/json"
	"testing"
)

func TestFSDJumpCarriesDistanceAndCoords(t *testing.T) {
	st := NewState()
	res := convertOne(t, st, "FSDJump", `{"StarSystem":"Sol","JumpDist":8.5,"StarPos":[1,2,3]}`)

	if len(res.Events) != 1 || res.Events[0].Name != "addCommanderTravelFSDJump" {
		t.Fatalf("unexpected events: %+v", res.Events)
	}
	body, _ := json.Marshal(res.Events[0].Data)
	if string(body) != `{"starsystemName":"Sol","jumpDistance":8.5,"starsystemCoords":[1,2,3]}` {
		t.Errorf("unexpected payload: %s", body)
	}
	if st.LastSystem != "Sol" {
		t.Errorf("FSDJump must record the system, got %q", st.LastSystem)
	}
}

func TestDockedNeedsBothSystemAndStation(t *testing.T) {
	st := NewState()
	if res := convertOne(t, st, "Docked", `{"StarSystem":"Sol"}`); len(res.Events) != 0 {
		t.Errorf("Docked without a station must send nothing, got %+v", res.Events)
	}
	if res := convertOne(t, st, "Docked", `{"StationName":"Abraham Lincoln"}`); len(res.Events) != 0 {
		t.Errorf("Docked without a system must send nothing, got %+v", res.Events)
	}

	res := convertOne(t, st, "Docked", `{"StarSystem":"Sol","StationName":"Abraham Lincoln","MarketID":128}`)
	if len(res.Events) != 1 || res.Events[0].Name != "addCommanderTravelDock" {
		t.Fatalf("unexpected events: %+v", res.Events)
	}
	body, _ := json.Marshal(res.Events[0].Data)
	if string(body) != `{"starsystemName":"Sol","stationName":"Abraham Lincoln","marketID":128}` {
		t.Errorf("unexpected payload: %s", body)
	}
}

// Location と CarrierJump はステーションが無くても星系だけで送る。
func TestLocationAndCarrierJumpSendWithoutAStation(t *testing.T) {
	for name, want := range map[string]string{
		"Location":    "setCommanderTravelLocation",
		"CarrierJump": "addCommanderTravelCarrierJump",
	} {
		st := NewState()
		res := convertOne(t, st, name, `{"StarSystem":"Sol"}`)
		if len(res.Events) != 1 || res.Events[0].Name != want {
			t.Errorf("%s: unexpected events: %+v", name, res.Events)
		}
		if st.LastSystem != "Sol" {
			t.Errorf("%s must record the system", name)
		}
	}
}
```

`mapping/mapping_test.go`:

```go
package mapping

import (
	"encoding/json"
	"testing"
)

func TestUnknownEventsAreIgnored(t *testing.T) {
	st := NewState()
	// 未知のイベントはデコードもしないので、壊れた JSON でもエラーにならない。
	res, err := Convert("Scan", "2026-07-27T00:00:00Z", json.RawMessage("not json"), st)
	if err != nil {
		t.Fatalf("unknown events must not fail: %v", err)
	}
	if len(res.Events) != 0 || res.FlushLive {
		t.Errorf("unexpected result: %+v", res)
	}
}

func TestBrokenPayloadIsReportedWithTheEventName(t *testing.T) {
	st := NewState()
	_, err := Convert("FSDJump", "2026-07-27T00:00:00Z", json.RawMessage("{oops"), st)
	if err == nil {
		t.Fatal("expected an error for a broken payload")
	}
	if got := err.Error(); got[:7] != "FSDJump" {
		t.Errorf("the error must name the event, got %q", got)
	}
}
```

- [ ] **Step 6: テストを走らせる**

Run: `go test ./mapping/ -v`
Expected: PASS(9 件)

- [ ] **Step 7: コミット**

```bash
git add mapping/
git commit -m "feat(examples/inara): add the mapping registry with identity and travel events"
```

---

### Task 3: 残りのマッパー(ランク・在庫・戦闘)

**Files:**
- Create: `mapping/ranks.go`, `mapping/inventory.go`, `mapping/combat.go`
- Modify: `mapping/mapping.go`(`handlers` に 8 件追加)
- Test: `mapping/ranks_test.go`, `mapping/inventory_test.go`, `mapping/combat_test.go`

**Interfaces:**
- Consumes: `handlerFor`、`converter`、`State`(Task 2)
- Produces: `handlers` に `Rank` / `Progress` / `Reputation` / `EngineerProgress` / `Materials` / `Statistics` / `Died` / `Shutdown` が加わる。`Shutdown` は `handler{flushLive: true}`(convert なし)

- [ ] **Step 1: `mapping/ranks.go` を書く**

```go
package mapping

import "github.com/himanoa/edlr/examples/plugins/inara-uploader/inara"

// rankSet は Rank と Progress で共通のフィールド集合。
// ポインタなのは「0(未昇進)」と「Journal に入っていない」を区別するため。
type rankSet struct {
	Combat       *float64 `json:"Combat"`
	Trade        *float64 `json:"Trade"`
	Explore      *float64 `json:"Explore"`
	CQC          *float64 `json:"CQC"`
	Soldier      *float64 `json:"Soldier"`
	Exobiologist *float64 `json:"Exobiologist"`
	Empire       *float64 `json:"Empire"`
	Federation   *float64 `json:"Federation"`
}

type rankValue struct {
	// name は INARA の rankName(小文字)
	name  string
	value float64
}

// values は入っているフィールドだけを INARA の名前で返す。順序は固定
// (map を回すと送信順が毎回変わり、テストも書けない)。
func (r rankSet) values() []rankValue {
	out := make([]rankValue, 0, 8)
	add := func(name string, v *float64) {
		if v != nil {
			out = append(out, rankValue{name: name, value: *v})
		}
	}
	add("combat", r.Combat)
	add("trade", r.Trade)
	add("exploration", r.Explore)
	add("cqc", r.CQC)
	add("mercenary", r.Soldier)
	add("exobiologist", r.Exobiologist)
	add("empire", r.Empire)
	add("federation", r.Federation)
	return out
}

type pilotRank struct {
	Name     string  `json:"rankName"`
	Value    int     `json:"rankValue"`
	Progress float64 `json:"rankProgress"`
}

func rankEvent(name string, value int, progress float64) inara.Event {
	return inara.New("setCommanderRankPilot", pilotRank{
		Name:     name,
		Value:    value,
		Progress: progress,
	})
}

// rank は段位を記録し、進捗が既知のものだけ送る。
//
// INARA は段位(rankValue)と段位内の進捗(rankProgress)を一緒に受け取る。
// 進捗は Journal では別イベント(Progress)で来るため、進捗を見ていない
// 段階で送ると INARA 側の進捗が 0 に潰れる。Journal では Rank の直後に必ず
// Progress が来るので、未知のうちは Progress に任せる。
type rank rankSet

func (r rank) convert(st *State) []inara.Event {
	var events []inara.Event
	for _, rv := range rankSet(r).values() {
		st.ranks[rv.name] = int(rv.value)
		progress, ok := st.progress[rv.name]
		if !ok {
			continue
		}
		events = append(events, rankEvent(rv.name, int(rv.value), progress))
	}
	return events
}

// progress は段位内の進捗を送る。段位そのものは直近の Rank 由来の値を使い、
// 段位が未知のうちは送らない(送ると INARA 側の段位が 0 に落ちうる)。
type progress rankSet

func (p progress) convert(st *State) []inara.Event {
	var events []inara.Event
	for _, rv := range rankSet(p).values() {
		// Journal はパーセント、INARA は 0..1 の比率。
		ratio := rv.value / 100
		st.progress[rv.name] = ratio

		value, ok := st.ranks[rv.name]
		if !ok {
			continue
		}
		events = append(events, rankEvent(rv.name, value, ratio))
	}
	return events
}

// reputation は主要勢力への評判。Journal は -100..100 のパーセント、
// INARA は -1..1 の比率。
type reputation struct {
	Empire      *float64 `json:"Empire"`
	Federation  *float64 `json:"Federation"`
	Alliance    *float64 `json:"Alliance"`
	Independent *float64 `json:"Independent"`
}

type factionReputation struct {
	Name       string  `json:"majorfactionName"`
	Reputation float64 `json:"majorfactionReputation"`
}

func (r reputation) convert(*State) []inara.Event {
	var events []inara.Event
	add := func(name string, v *float64) {
		if v == nil {
			return
		}
		events = append(events, inara.New("setCommanderReputationMajorFaction", factionReputation{
			Name:       name,
			Reputation: *v / 100,
		}))
	}
	add("empire", r.Empire)
	add("federation", r.Federation)
	add("alliance", r.Alliance)
	add("independent", r.Independent)
	return events
}

type engineer struct {
	Engineer string `json:"Engineer"`
	Progress string `json:"Progress"`
	Rank     *int   `json:"Rank"`
}

// engineerProgress は起動直後だけ全エンジニアの配列で来て、以降は単体で来る。
// 埋め込みで単体形式のフィールドをそのまま受ける。
type engineerProgress struct {
	Engineers []engineer `json:"Engineers"`
	engineer
}

type engineerRank struct {
	Name  string `json:"engineerName"`
	Stage string `json:"rankStage,omitempty"`
	Value *int   `json:"rankValue,omitempty"`
}

func (e engineer) event() (inara.Event, bool) {
	if e.Engineer == "" {
		return inara.Event{}, false
	}
	return inara.New("setCommanderRankEngineer", engineerRank{
		Name:  e.Engineer,
		Stage: e.Progress,
		Value: e.Rank,
	}), true
}

func (p engineerProgress) convert(*State) []inara.Event {
	if len(p.Engineers) > 0 {
		var events []inara.Event
		for _, e := range p.Engineers {
			if ev, ok := e.event(); ok {
				events = append(events, ev)
			}
		}
		return events
	}
	if ev, ok := p.engineer.event(); ok {
		return []inara.Event{ev}
	}
	return nil
}
```

旧実装の `lower()`(手書きの ASCII 小文字化)はここで消える。INARA の rankName と
majorfactionName は定数で持つので、Journal の名前を変換する必要が無い。

- [ ] **Step 2: `mapping/inventory.go` を書く**

```go
package mapping

import (
	"encoding/json"

	"github.com/himanoa/edlr/examples/plugins/inara-uploader/inara"
)

type materialItem struct {
	Name  string `json:"Name"`
	Count *int64 `json:"Count"`
}

// materials は素材在庫。Journal は Raw / Manufactured / Encoded に分かれるが、
// INARA は種別を区別しない 1 本のリストを受け取る。
type materials struct {
	Raw          []materialItem `json:"Raw"`
	Manufactured []materialItem `json:"Manufactured"`
	Encoded      []materialItem `json:"Encoded"`
}

type inventoryItem struct {
	Name  string `json:"itemName"`
	Count int64  `json:"itemCount"`
}

func (m materials) convert(*State) []inara.Event {
	var items []inventoryItem
	for _, list := range [][]materialItem{m.Raw, m.Manufactured, m.Encoded} {
		for _, item := range list {
			if item.Name == "" || item.Count == nil {
				continue
			}
			items = append(items, inventoryItem{Name: item.Name, Count: *item.Count})
		}
	}
	if len(items) == 0 {
		return nil
	}
	return []inara.Event{inara.New("setCommanderInventoryMaterials", items)}
}

// statistics は Journal の中身をそのまま送る(INARA が同じ構造を受け付ける)。
// イベント本体のメタデータだけ落とす。
type statistics map[string]json.RawMessage

func (s statistics) convert(*State) []inara.Event {
	data := make(map[string]json.RawMessage, len(s))
	for key, value := range s {
		if key == "timestamp" || key == "event" {
			continue
		}
		data[key] = value
	}
	if len(data) == 0 {
		return nil
	}
	return []inara.Event{inara.New("setCommanderGameStatistics", data)}
}
```

- [ ] **Step 3: `mapping/combat.go` を書く**

```go
package mapping

import "github.com/himanoa/edlr/examples/plugins/inara-uploader/inara"

// died は撃墜。Journal の Died は星系名を含まないため、直近の移動イベントで
// 覚えた星系を添える。相手は単独なら KillerName、ウイングなら Killers。
type died struct {
	KillerName string `json:"KillerName"`
	Killers    []struct {
		Name string `json:"Name"`
	} `json:"Killers"`
}

type combatDeath struct {
	System   string `json:"starsystemName,omitempty"`
	Opponent string `json:"opponentName,omitempty"`
}

func (d died) convert(st *State) []inara.Event {
	data := combatDeath{System: st.LastSystem, Opponent: d.KillerName}
	if data.Opponent == "" && len(d.Killers) > 0 {
		data.Opponent = d.Killers[0].Name
	}
	return []inara.Event{inara.New("addCommanderCombatDeath", data)}
}
```

- [ ] **Step 4: `mapping/mapping.go` の `handlers` を全 14 件にする**

```go
var handlers = map[string]handler{
	"Commander":        handlerFor[commander](),
	"LoadGame":         handlerFor[loadGame](),
	"FSDJump":          handlerFor[fsdJump](),
	"CarrierJump":      handlerFor[carrierJump](),
	"Docked":           handlerFor[docked](),
	"Location":         handlerFor[location](),
	"Rank":             handlerFor[rank](),
	"Progress":         handlerFor[progress](),
	"Reputation":       handlerFor[reputation](),
	"EngineerProgress": handlerFor[engineerProgress](),
	"Materials":        handlerFor[materials](),
	"Statistics":       handlerFor[statistics](),
	"Died":             handlerFor[died](),
	// Shutdown は送るものが無く、live モードでの即時フラッシュだけを促す。
	"Shutdown": {flushLive: true},
}
```

- [ ] **Step 5: テストを書く**

`mapping/ranks_test.go`:

```go
package mapping

import (
	"encoding/json"
	"testing"
)

// 段位だけ先に来ても送らない。送ると INARA 側の進捗が 0 に潰れる。
func TestRankWaitsForProgress(t *testing.T) {
	st := NewState()
	if res := convertOne(t, st, "Rank", `{"Combat":3}`); len(res.Events) != 0 {
		t.Fatalf("Rank alone must send nothing, got %+v", res.Events)
	}

	res := convertOne(t, st, "Progress", `{"Combat":40}`)
	if len(res.Events) != 1 {
		t.Fatalf("Progress must send the pending rank, got %+v", res.Events)
	}
	body, _ := json.Marshal(res.Events[0].Data)
	if string(body) != `{"rankName":"combat","rankValue":3,"rankProgress":0.4}` {
		t.Errorf("unexpected payload: %s", body)
	}
}

// 進捗だけ先に来ても送らない。送ると INARA 側の段位が 0 に落ちうる。
func TestProgressWaitsForRank(t *testing.T) {
	st := NewState()
	if res := convertOne(t, st, "Progress", `{"Trade":10}`); len(res.Events) != 0 {
		t.Fatalf("Progress alone must send nothing, got %+v", res.Events)
	}

	res := convertOne(t, st, "Rank", `{"Trade":2}`)
	if len(res.Events) != 1 {
		t.Fatalf("Rank must send once progress is known, got %+v", res.Events)
	}
	body, _ := json.Marshal(res.Events[0].Data)
	if string(body) != `{"rankName":"trade","rankValue":2,"rankProgress":0.1}` {
		t.Errorf("unexpected payload: %s", body)
	}
}

// 送信順は固定(map を回すと毎回変わる)。
func TestRankOrderIsStable(t *testing.T) {
	st := NewState()
	convertOne(t, st, "Progress", `{"Combat":10,"Trade":20,"Explore":30}`)
	res := convertOne(t, st, "Rank", `{"Combat":1,"Trade":2,"Explore":3}`)

	var names []string
	for _, ev := range res.Events {
		names = append(names, ev.Data.(pilotRank).Name)
	}
	want := []string{"combat", "trade", "exploration"}
	if len(names) != len(want) {
		t.Fatalf("expected %v, got %v", want, names)
	}
	for i := range want {
		if names[i] != want[i] {
			t.Fatalf("expected %v, got %v", want, names)
		}
	}
}

func TestReputationIsScaledToRatio(t *testing.T) {
	st := NewState()
	res := convertOne(t, st, "Reputation", `{"Empire":50,"Federation":-25}`)
	if len(res.Events) != 2 {
		t.Fatalf("expected 2 events, got %+v", res.Events)
	}
	body, _ := json.Marshal(res.Events)
	want := `[{"eventName":"setCommanderReputationMajorFaction","eventTimestamp":"2026-07-27T00:00:00Z","eventData":{"majorfactionName":"empire","majorfactionReputation":0.5}},` +
		`{"eventName":"setCommanderReputationMajorFaction","eventTimestamp":"2026-07-27T00:00:00Z","eventData":{"majorfactionName":"federation","majorfactionReputation":-0.25}}]`
	if string(body) != want {
		t.Errorf("unexpected payload:\n got %s\nwant %s", body, want)
	}
}

func TestEngineerProgressAcceptsBothShapes(t *testing.T) {
	st := NewState()

	batch := convertOne(t, st, "EngineerProgress", `{"Engineers":[{"Engineer":"Felicity Farseer","Progress":"Unlocked","Rank":5},{"Engineer":"Elvira Martuuk","Progress":"Known"}]}`)
	if len(batch.Events) != 2 {
		t.Fatalf("expected 2 events from the array form, got %+v", batch.Events)
	}

	single := convertOne(t, st, "EngineerProgress", `{"Engineer":"Felicity Farseer","Progress":"Unlocked","Rank":5}`)
	if len(single.Events) != 1 {
		t.Fatalf("expected 1 event from the single form, got %+v", single.Events)
	}
	body, _ := json.Marshal(single.Events[0].Data)
	if string(body) != `{"engineerName":"Felicity Farseer","rankStage":"Unlocked","rankValue":5}` {
		t.Errorf("unexpected payload: %s", body)
	}
}
```

`mapping/inventory_test.go`:

```go
package mapping

import (
	"encoding/json"
	"testing"
)

func TestMaterialsAreFlattenedIntoOneList(t *testing.T) {
	st := NewState()
	res := convertOne(t, st, "Materials", `{"Raw":[{"Name":"iron","Count":10}],"Manufactured":[{"Name":"basicconductors","Count":3}],"Encoded":[]}`)

	if len(res.Events) != 1 || res.Events[0].Name != "setCommanderInventoryMaterials" {
		t.Fatalf("unexpected events: %+v", res.Events)
	}
	body, _ := json.Marshal(res.Events[0].Data)
	if string(body) != `[{"itemName":"iron","itemCount":10},{"itemName":"basicconductors","itemCount":3}]` {
		t.Errorf("unexpected payload: %s", body)
	}
}

func TestEmptyMaterialsSendNothing(t *testing.T) {
	st := NewState()
	if res := convertOne(t, st, "Materials", `{"Raw":[],"Manufactured":[],"Encoded":[]}`); len(res.Events) != 0 {
		t.Errorf("expected no events, got %+v", res.Events)
	}
}

func TestStatisticsDropEventMetadata(t *testing.T) {
	st := NewState()
	res := convertOne(t, st, "Statistics", `{"timestamp":"2026-07-27T00:00:00Z","event":"Statistics","Bank_Account":{"Current_Wealth":1}}`)

	if len(res.Events) != 1 {
		t.Fatalf("unexpected events: %+v", res.Events)
	}
	body, _ := json.Marshal(res.Events[0].Data)
	if string(body) != `{"Bank_Account":{"Current_Wealth":1}}` {
		t.Errorf("unexpected payload: %s", body)
	}
}

func TestStatisticsWithOnlyMetadataSendNothing(t *testing.T) {
	st := NewState()
	if res := convertOne(t, st, "Statistics", `{"timestamp":"t","event":"Statistics"}`); len(res.Events) != 0 {
		t.Errorf("expected no events, got %+v", res.Events)
	}
}
```

`mapping/combat_test.go`:

```go
package mapping

import (
	"encoding/json"
	"testing"
)

func TestDiedBorrowsTheSystemFromTheLastTravelEvent(t *testing.T) {
	st := NewState()
	convertOne(t, st, "FSDJump", `{"StarSystem":"Sol"}`)
	res := convertOne(t, st, "Died", `{"KillerName":"Salvation"}`)

	body, _ := json.Marshal(res.Events[0].Data)
	if string(body) != `{"starsystemName":"Sol","opponentName":"Salvation"}` {
		t.Errorf("unexpected payload: %s", body)
	}
}

func TestDiedFallsBackToTheFirstOfAWing(t *testing.T) {
	st := NewState()
	res := convertOne(t, st, "Died", `{"Killers":[{"Name":"Wing Leader"},{"Name":"Wingman"}]}`)

	body, _ := json.Marshal(res.Events[0].Data)
	if string(body) != `{"opponentName":"Wing Leader"}` {
		t.Errorf("unexpected payload: %s", body)
	}
}

func TestShutdownOnlyRequestsAFlush(t *testing.T) {
	st := NewState()
	res := convertOne(t, st, "Shutdown", `{}`)
	if len(res.Events) != 0 {
		t.Errorf("Shutdown must not produce events, got %+v", res.Events)
	}
	if !res.FlushLive {
		t.Error("Shutdown must request a flush")
	}
}
```

- [ ] **Step 6: テストを走らせる**

Run: `go test ./mapping/ -v`
Expected: PASS(21 件)

- [ ] **Step 7: コミット**

```bash
git add mapping/
git commit -m "feat(examples/inara): port the remaining journal mappers to typed decoding"
```

---

### Task 4: manifest との整合テスト

**Files:**
- Create: `mapping/manifest_test.go`

**Interfaces:**
- Consumes: `mapping.Names()`(Task 2)
- Produces: なし(テストのみ)

`manifest.toml` の `events` と `handlers` のキーがズレたら落ちるテスト。TOML パーサは追加せず、`events = [` から `]` までの引用符付き文字列を拾う。

- [ ] **Step 1: `mapping/manifest_test.go` を書く**

```go
package mapping

import (
	"os"
	"strings"
	"testing"
)

// manifestEvents は manifest.toml の `events = [...]` から名前を拾う。
//
// TOML パーサを go.mod に足すほどではない(読むのは自分たちが書いた 1
// ファイルの 1 配列だけ)。想定するのは複数行の配列で、要素は 1 行に 1 つ。
func manifestEvents(t *testing.T) []string {
	t.Helper()

	raw, err := os.ReadFile("../manifest.toml")
	if err != nil {
		t.Fatalf("cannot read manifest.toml: %v", err)
	}

	var names []string
	inArray := false
	for _, line := range strings.Split(string(raw), "\n") {
		line = strings.TrimSpace(line)
		switch {
		case !inArray && strings.HasPrefix(line, "events") && strings.HasSuffix(line, "["):
			inArray = true
		case inArray && strings.HasPrefix(line, "]"):
			return names
		case inArray:
			if name, ok := quoted(line); ok {
				names = append(names, name)
			}
		}
	}
	t.Fatal("manifest.toml has no multi-line `events = [` array")
	return nil
}

// quoted は `"Docked",` のような行から中身を取り出す。コメント行と空行は false。
func quoted(line string) (string, bool) {
	start := strings.Index(line, `"`)
	if start < 0 {
		return "", false
	}
	end := strings.Index(line[start+1:], `"`)
	if end < 0 {
		return "", false
	}
	return line[start+1 : start+1+end], true
}

// manifest の events とレジストリのキーは一致していること。ズレると
// 「manifest に書いたのに変換されない」「変換を書いたのにイベントが
// 届かない」のどちらかが黙って起きる。
func TestManifestEventsMatchTheRegistry(t *testing.T) {
	manifest := map[string]bool{}
	for _, name := range manifestEvents(t) {
		manifest[name] = true
	}

	registry := map[string]bool{}
	for _, name := range Names() {
		registry[name] = true
	}

	for name := range manifest {
		if !registry[name] {
			t.Errorf("manifest.toml subscribes to %q but mapping has no handler for it", name)
		}
	}
	for name := range registry {
		if !manifest[name] {
			t.Errorf("mapping handles %q but manifest.toml does not subscribe to it", name)
		}
	}
}

func TestManifestEventsAreParsed(t *testing.T) {
	if got := manifestEvents(t); len(got) == 0 {
		t.Fatal("no events parsed from manifest.toml")
	}
}
```

- [ ] **Step 2: テストを走らせる**

Run: `go test ./mapping/ -run Manifest -v`
Expected: PASS(2 件)。落ちる場合は `manifest.toml` の `events` と `handlers` のどちらかが欠けているので、**`manifest.toml` 側を正とせず、設計書の対応表(14 件)と突き合わせて直す**。

- [ ] **Step 3: 意図的に壊して、テストが検出することを確認する**

`mapping/mapping.go` の `handlers` から `"Died"` の行を一時的にコメントアウトし、`go test ./mapping/ -run Manifest` が FAIL することを確認してから戻す。

Expected: `manifest.toml subscribes to "Died" but mapping has no handler for it`

- [ ] **Step 4: コミット**

```bash
git add mapping/manifest_test.go
git commit -m "test(examples/inara): fail when manifest events and the registry drift apart"
```

---

### Task 5: `uploader` のキュー(リングバッファ)

**Files:**
- Create: `uploader/queue.go`
- Test: `uploader/queue_test.go`

**Interfaces:**
- Consumes: `inara.Event`(Task 1)
- Produces(パッケージ内部。以降のタスクが使う):
  - `newQueue(max int) *queue`
  - `(*queue).push(events []inara.Event)` — 上限超過分は先頭から捨て、`dropped` に加算
  - `(*queue).peek() []inara.Event`
  - `(*queue).clear()`
  - `(*queue).len() int`
  - `(*queue).takeDropped() int` — 累積した破棄件数を返してリセット

- [ ] **Step 1: `uploader/queue_test.go` を書く(失敗する状態)**

```go
package uploader

import (
	"testing"

	"github.com/himanoa/edlr/examples/plugins/inara-uploader/inara"
)

func events(names ...string) []inara.Event {
	out := make([]inara.Event, 0, len(names))
	for _, name := range names {
		out = append(out, inara.New(name, nil))
	}
	return out
}

func names(evs []inara.Event) []string {
	out := make([]string, 0, len(evs))
	for _, ev := range evs {
		out = append(out, ev.Name)
	}
	return out
}

func TestQueueKeepsEverythingBelowTheLimit(t *testing.T) {
	q := newQueue(3)
	q.push(events("a", "b"))

	if q.len() != 2 {
		t.Errorf("expected 2 queued, got %d", q.len())
	}
	if q.takeDropped() != 0 {
		t.Error("nothing should have been dropped")
	}
}

// 上限を超えたら古いものから捨てる。INARA は現在の状態を反映するサービスなので、
// 古い travel ログを落として最新を残すほうが実害が小さい。
func TestQueueDropsTheOldestOverTheLimit(t *testing.T) {
	q := newQueue(3)
	q.push(events("a", "b", "c"))
	q.push(events("d", "e"))

	if got := names(q.peek()); len(got) != 3 || got[0] != "c" || got[2] != "e" {
		t.Errorf("expected the newest 3 (c d e), got %v", got)
	}
	if got := q.takeDropped(); got != 2 {
		t.Errorf("expected 2 dropped, got %d", got)
	}
}

// takeDropped は読み出したらリセットする(同じ破棄を二度報告しない)。
func TestTakeDroppedResets(t *testing.T) {
	q := newQueue(1)
	q.push(events("a", "b"))

	if got := q.takeDropped(); got != 1 {
		t.Fatalf("expected 1 dropped, got %d", got)
	}
	if got := q.takeDropped(); got != 0 {
		t.Errorf("dropped count must reset after being taken, got %d", got)
	}
}

// 1 回の push が上限より大きい場合も、最新のぶんだけ残る。
func TestQueueHandlesAPushLargerThanTheLimit(t *testing.T) {
	q := newQueue(2)
	q.push(events("a", "b", "c", "d"))

	if got := names(q.peek()); len(got) != 2 || got[0] != "c" || got[1] != "d" {
		t.Errorf("expected (c d), got %v", got)
	}
	if got := q.takeDropped(); got != 2 {
		t.Errorf("expected 2 dropped, got %d", got)
	}
}

func TestClearEmptiesTheQueue(t *testing.T) {
	q := newQueue(3)
	q.push(events("a"))
	q.clear()

	if q.len() != 0 || len(q.peek()) != 0 {
		t.Errorf("expected an empty queue, got %v", q.peek())
	}
}
```

- [ ] **Step 2: テストが失敗することを確認する**

Run: `go test ./uploader/ -v`
Expected: FAIL(`undefined: newQueue`)

- [ ] **Step 3: `uploader/queue.go` を書く**

```go
package uploader

import "github.com/himanoa/edlr/examples/plugins/inara-uploader/inara"

// queue は送信待ちイベントを直近 max 件だけ保持するリングバッファ。
//
// 上限は「メモリ保護の保険」ではなく仕様。API キー未設定や capability 未承認で
// 送れない状態が続いてもメモリは伸びず、代わりに古いものから落ちる。
type queue struct {
	items   []inara.Event
	max     int
	dropped int
}

func newQueue(max int) *queue {
	return &queue{max: max}
}

func (q *queue) push(events []inara.Event) {
	q.items = append(q.items, events...)
	if overflow := len(q.items) - q.max; overflow > 0 {
		q.items = append(q.items[:0], q.items[overflow:]...)
		q.dropped += overflow
	}
}

func (q *queue) peek() []inara.Event { return q.items }

func (q *queue) len() int { return len(q.items) }

// clear は中身を捨てる。peek で借りたスライスを使い回さないよう、
// 同じ配列は再利用しない。
func (q *queue) clear() { q.items = nil }

// takeDropped は前回の報告以降に捨てた件数を返してリセットする。
func (q *queue) takeDropped() int {
	n := q.dropped
	q.dropped = 0
	return n
}
```

- [ ] **Step 4: テストが通ることを確認する**

Run: `go test ./uploader/ -v`
Expected: PASS(5 件)

- [ ] **Step 5: コミット**

```bash
git add uploader/
git commit -m "feat(examples/inara): add a bounded queue that drops the oldest events"
```

---

### Task 6: `uploader` 本体(モード・フラッシュ判断・送信)

**Files:**
- Create: `uploader/uploader.go`
- Test: `uploader/uploader_test.go`

**Interfaces:**
- Consumes: `queue`(Task 5)、`mapping.Convert` / `mapping.NewState`(Task 2,3)、`inara.Encode` / `inara.Interpret` / `inara.BatchError` / `inara.Rejection`(Task 1)、`settings.Settings`(既存)
- Produces:
  - `uploader.MaxQueued = 200`、`uploader.ReplayBatchSize = 100`
  - `uploader.Event{Kind, Name, Timestamp, Payload string; Replay bool}`
  - `uploader.Sender` インターフェース: `Send(body []byte) (status int, body []byte, err error)`
  - `uploader.New(now func() time.Time, sender Sender) *Uploader`
  - `(*Uploader).Handle(cfg settings.Settings, ev Event) Outcome`
  - `uploader.Outcome{Queued, Sent, Dropped, Skipped, Pending int; Held string; DryRun []byte; Rejected []inara.Rejection; Err error; Fatal bool}`

- [ ] **Step 1: `uploader/uploader_test.go` を書く(失敗する状態)**

```go
package uploader

import (
	"errors"
	"testing"
	"time"

	"github.com/himanoa/edlr/examples/plugins/inara-uploader/settings"
)

// stubSender は送信を記録し、あらかじめ決めた結果を返す。
type stubSender struct {
	calls  [][]byte
	status int
	body   string
	err    error
}

func (s *stubSender) Send(body []byte) (int, []byte, error) {
	s.calls = append(s.calls, append([]byte(nil), body...))
	if s.err != nil {
		return 0, nil, s.err
	}
	return s.status, []byte(s.body), nil
}

func okSender() *stubSender {
	return &stubSender{status: 200, body: `{"header":{"eventStatus":200},"events":[]}`}
}

// clock は手で進める時計。
type clock struct{ t time.Time }

func (c *clock) now() time.Time      { return c.t }
func (c *clock) advance(d time.Duration) { c.t = c.t.Add(d) }

func newClock() *clock {
	return &clock{t: time.Date(2026, 7, 27, 0, 0, 0, 0, time.UTC)}
}

func testSettings() settings.Settings {
	cfg := settings.Defaults()
	cfg.APIKey = "key"
	cfg.CommanderName = "cmdr"
	return cfg
}

// jump は 1 件の INARA イベントになる Journal イベントを作る。
func jump(system string, replay bool) Event {
	return Event{
		Kind:      "journal",
		Name:      "FSDJump",
		Timestamp: "2026-07-27T00:00:00Z",
		Payload:   `{"StarSystem":"` + system + `"}`,
		Replay:    replay,
	}
}

func feed(u *Uploader, cfg settings.Settings, n int, replay bool) []Outcome {
	out := make([]Outcome, 0, n)
	for i := 0; i < n; i++ {
		out = append(out, u.Handle(cfg, jump("Sol", replay)))
	}
	return out
}

func TestStatusEventsAreIgnored(t *testing.T) {
	sender := okSender()
	u := New(newClock().now, sender)

	out := u.Handle(testSettings(), Event{Kind: "status", Payload: `{}`})
	if out.Queued != 0 || len(sender.calls) != 0 {
		t.Errorf("status events must be ignored, got %+v", out)
	}
}

func TestDisabledPluginDoesNothing(t *testing.T) {
	sender := okSender()
	cfg := testSettings()
	cfg.Enabled = false
	u := New(newClock().now, sender)

	if out := u.Handle(cfg, jump("Sol", false)); out.Queued != 0 {
		t.Errorf("a disabled plugin must not queue, got %+v", out)
	}
}

// replay モードは minIntervalSeconds を無視し、ReplayBatchSize ごとに送る。
func TestReplayFlushesEveryReplayBatchIgnoringTheInterval(t *testing.T) {
	sender := okSender()
	c := newClock()
	u := New(c.now, sender)

	feed(u, testSettings(), ReplayBatchSize-1, true)
	if len(sender.calls) != 0 {
		t.Fatalf("expected no upload before the batch is full, got %d", len(sender.calls))
	}

	// 時計は 1 秒も進めていない(minIntervalSeconds は 60)。
	out := u.Handle(testSettings(), jump("Sol", true))
	if len(sender.calls) != 1 {
		t.Fatalf("replay must ignore the interval, got %d uploads", len(sender.calls))
	}
	if out.Sent != ReplayBatchSize {
		t.Errorf("expected %d sent, got %d", ReplayBatchSize, out.Sent)
	}
}

// replay 中の Shutdown は過去のゲーム終了ログであって「もう来ない」を意味しない。
func TestReplayedShutdownDoesNotFlush(t *testing.T) {
	sender := okSender()
	u := New(newClock().now, sender)
	cfg := testSettings()

	u.Handle(cfg, jump("Sol", true))
	out := u.Handle(cfg, Event{Kind: "journal", Name: "Shutdown", Payload: `{}`, Replay: true})

	if len(sender.calls) != 0 {
		t.Errorf("a replayed Shutdown must not flush, got %d uploads", len(sender.calls))
	}
	if out.Pending != 1 {
		t.Errorf("the event must stay queued, got %+v", out)
	}
}

// live に切り替わった瞬間、replay で溜まった端数を送り切る。
func TestTransitionToLiveFlushesTheBacklog(t *testing.T) {
	sender := okSender()
	u := New(newClock().now, sender)
	cfg := testSettings()

	feed(u, cfg, 3, true)
	out := u.Handle(cfg, jump("Sol", false))

	if len(sender.calls) != 1 {
		t.Fatalf("expected one upload at the transition, got %d", len(sender.calls))
	}
	if out.Sent != 4 {
		t.Errorf("expected the backlog plus the live event, got %d", out.Sent)
	}
}

// uploadHistorical=false のスキップ件数は、遷移時に 1 回だけ報告する。
func TestSkippedReplayIsReportedOnceAtTheTransition(t *testing.T) {
	sender := okSender()
	u := New(newClock().now, sender)
	cfg := testSettings()
	cfg.UploadHistorical = false

	for _, out := range feed(u, cfg, 5, true) {
		if out.Queued != 0 || out.Skipped != 0 {
			t.Fatalf("replayed events must be skipped silently, got %+v", out)
		}
	}

	out := u.Handle(cfg, jump("Sol", false))
	if out.Skipped != 5 {
		t.Errorf("expected 5 skipped reported at the transition, got %+v", out)
	}
	if next := u.Handle(cfg, jump("Sol", false)); next.Skipped != 0 {
		t.Errorf("the skipped total must be reported only once, got %+v", next)
	}
}

// live モードは batchSize と minIntervalSeconds の両方を満たすまで送らない。
func TestLiveRespectsBatchSizeAndInterval(t *testing.T) {
	sender := okSender()
	c := newClock()
	u := New(c.now, sender)
	cfg := testSettings()
	cfg.BatchSize = 3

	feed(u, cfg, 3, false)
	if len(sender.calls) != 0 {
		t.Fatalf("expected no upload before the interval elapses, got %d", len(sender.calls))
	}

	c.advance(time.Duration(cfg.MinIntervalSec) * time.Second)
	out := u.Handle(cfg, jump("Sol", false))
	if len(sender.calls) != 1 {
		t.Fatalf("expected an upload once the interval elapsed, got %d", len(sender.calls))
	}
	if out.Sent != 4 {
		t.Errorf("expected 4 sent, got %d", out.Sent)
	}
}

func TestLiveShutdownFlushesImmediately(t *testing.T) {
	sender := okSender()
	u := New(newClock().now, sender)
	cfg := testSettings()

	u.Handle(cfg, jump("Sol", false))
	out := u.Handle(cfg, Event{Kind: "journal", Name: "Shutdown", Payload: `{}`})

	if len(sender.calls) != 1 || out.Sent != 1 {
		t.Errorf("a live Shutdown must flush, got %d uploads / %+v", len(sender.calls), out)
	}
}

// 現行実装の実質バグの回帰テスト: 送れない状態でもキューは上限で止まる。
func TestQueueStaysBoundedWithoutAnApiKey(t *testing.T) {
	sender := okSender()
	c := newClock()
	u := New(c.now, sender)
	cfg := testSettings()
	cfg.APIKey = ""

	var lastPending, totalDropped int
	for i := 0; i < MaxQueued*3; i++ {
		out := u.Handle(cfg, jump("Sol", false))
		lastPending = out.Pending
		totalDropped += out.Dropped
		c.advance(time.Minute)
	}

	if lastPending > MaxQueued {
		t.Errorf("the queue must stay bounded, got %d pending", lastPending)
	}
	if totalDropped == 0 {
		t.Error("dropped events must be reported")
	}
	if len(sender.calls) != 0 {
		t.Errorf("nothing may be uploaded without an api key, got %d", len(sender.calls))
	}
}

func TestHoldsWhenTheCommanderIsUnknown(t *testing.T) {
	sender := okSender()
	u := New(newClock().now, sender)
	cfg := testSettings()
	cfg.CommanderName = ""

	u.Handle(cfg, jump("Sol", false))
	out := u.Handle(cfg, Event{Kind: "journal", Name: "Shutdown", Payload: `{}`})

	if out.Held == "" {
		t.Error("expected a held reason")
	}
	if out.Pending != 1 {
		t.Errorf("held events must stay queued, got %+v", out)
	}
	if len(sender.calls) != 0 {
		t.Errorf("nothing may be uploaded, got %d", len(sender.calls))
	}
}

// コマンダー名は Journal から学習できる(設定が空でも送れるようになる)。
func TestCommanderNameIsLearnedFromTheJournal(t *testing.T) {
	sender := okSender()
	u := New(newClock().now, sender)
	cfg := testSettings()
	cfg.CommanderName = ""

	u.Handle(cfg, Event{Kind: "journal", Name: "Commander", Payload: `{"Name":"Hutton","FID":"F1"}`})
	u.Handle(cfg, jump("Sol", false))
	out := u.Handle(cfg, Event{Kind: "journal", Name: "Shutdown", Payload: `{}`})

	if out.Held != "" || out.Sent != 1 {
		t.Fatalf("expected the learned name to allow the upload, got %+v", out)
	}
}

func TestDryRunReturnsTheBodyWithoutSending(t *testing.T) {
	sender := okSender()
	u := New(newClock().now, sender)
	cfg := testSettings()
	cfg.DryRun = true

	u.Handle(cfg, jump("Sol", false))
	out := u.Handle(cfg, Event{Kind: "journal", Name: "Shutdown", Payload: `{}`})

	if len(sender.calls) != 0 {
		t.Error("dry run must not send")
	}
	if len(out.DryRun) == 0 || out.Sent != 1 || out.Pending != 0 {
		t.Errorf("unexpected outcome: %+v", out)
	}
}

// 送信失敗はキューを残して次のイベントで再試行する。
func TestTransportFailureKeepsTheQueue(t *testing.T) {
	sender := &stubSender{err: errors.New("timeout")}
	c := newClock()
	u := New(c.now, sender)
	cfg := testSettings()

	u.Handle(cfg, jump("Sol", false))
	out := u.Handle(cfg, Event{Kind: "journal", Name: "Shutdown", Payload: `{}`})
	if out.Err == nil || out.Fatal {
		t.Errorf("expected a retryable error, got %+v", out)
	}
	if out.Pending != 1 {
		t.Errorf("the queue must be kept for a retry, got %+v", out)
	}

	sender.err = nil
	sender.status = 200
	sender.body = `{"header":{"eventStatus":200},"events":[]}`
	c.advance(time.Minute)
	retry := u.Handle(cfg, Event{Kind: "journal", Name: "Shutdown", Payload: `{}`})
	if retry.Sent != 1 || retry.Pending != 0 {
		t.Errorf("expected the retry to succeed, got %+v", retry)
	}
}

// バッチ全体の拒否は恒久的なのでキューを捨てる。
func TestBatchRejectionDropsTheQueue(t *testing.T) {
	sender := &stubSender{status: 200, body: `{"header":{"eventStatus":400,"eventStatusText":"bad key"},"events":[]}`}
	u := New(newClock().now, sender)
	cfg := testSettings()

	u.Handle(cfg, jump("Sol", false))
	out := u.Handle(cfg, Event{Kind: "journal", Name: "Shutdown", Payload: `{}`})

	if !out.Fatal || out.Err == nil {
		t.Errorf("expected a fatal error, got %+v", out)
	}
	if out.Pending != 0 || out.Dropped != 1 {
		t.Errorf("a rejected batch must be dropped, got %+v", out)
	}
}

func TestIndividualRejectionsAreReported(t *testing.T) {
	sender := &stubSender{
		status: 200,
		body:   `{"header":{"eventStatus":200},"events":[{"eventStatus":400,"eventStatusText":"nope"}]}`,
	}
	u := New(newClock().now, sender)
	cfg := testSettings()

	u.Handle(cfg, jump("Sol", false))
	out := u.Handle(cfg, Event{Kind: "journal", Name: "Shutdown", Payload: `{}`})

	if len(out.Rejected) != 1 || out.Rejected[0].StatusText != "nope" {
		t.Errorf("unexpected rejections: %+v", out.Rejected)
	}
	if out.Pending != 0 {
		t.Errorf("a rejected event is not retried, got %+v", out)
	}
}

func TestBrokenPayloadIsReportedButDoesNotStopTheUploader(t *testing.T) {
	sender := okSender()
	u := New(newClock().now, sender)
	cfg := testSettings()

	out := u.Handle(cfg, Event{Kind: "journal", Name: "FSDJump", Payload: "{oops"})
	if out.Err == nil {
		t.Fatal("expected an error for a broken payload")
	}

	next := u.Handle(cfg, jump("Sol", false))
	if next.Queued != 1 {
		t.Errorf("the uploader must keep working, got %+v", next)
	}
}

// batchSize が上限より大きくても、キューが詰まったまま止まらない。
func TestOversizedBatchSizeStillFlushes(t *testing.T) {
	sender := okSender()
	c := newClock()
	u := New(c.now, sender)
	cfg := testSettings()
	cfg.BatchSize = MaxQueued * 10

	for i := 0; i < MaxQueued+1; i++ {
		u.Handle(cfg, jump("Sol", false))
		c.advance(time.Minute)
	}

	if len(sender.calls) == 0 {
		t.Error("an oversized batchSize must not wedge the queue")
	}
}
```

- [ ] **Step 2: テストが失敗することを確認する**

Run: `go test ./uploader/ -v`
Expected: FAIL(`undefined: New`, `undefined: Event` など)

- [ ] **Step 3: `uploader/uploader.go` を書く**

```go
// Package uploader は Journal イベントの受け取りから INARA への送信までを
// 決める。ホストの import には依存せず、時計と送信手段は注入する。
//
// このパッケージはログを吐かない。何が起きたかは Outcome として返し、
// 文字列の整形とログレベルの決定は main が行う。テストは返り値を直接
// 検証できる。
package uploader

import (
	"encoding/json"
	"errors"
	"time"

	"github.com/himanoa/edlr/examples/plugins/inara-uploader/inara"
	"github.com/himanoa/edlr/examples/plugins/inara-uploader/mapping"
	"github.com/himanoa/edlr/examples/plugins/inara-uploader/settings"
)

const (
	appName    = "edlr-inara-uploader"
	appVersion = "0.1.0"

	// MaxQueued は保持する送信待ちイベントの上限。超えたぶんは古いものから捨てる。
	MaxQueued = 200

	// ReplayBatchSize は replay モードで 1 度に送る件数。バックログを流し切る
	// ための値で、minIntervalSeconds は適用しない。
	ReplayBatchSize = 100
)

// Event はホストから届く 1 イベント。WIT の record をそのまま写したもの。
type Event struct {
	// Kind は "journal" か "status"
	Kind      string
	Name      string
	Timestamp string
	Payload   string
	Replay    bool
}

// Sender は組み立て済みの JSON を送る。実体は main の driver-http 呼び出し。
type Sender interface {
	Send(body []byte) (status int, body []byte, err error)
}

// Outcome は Handle 1 回で起きたこと。ゼロ値は「何も起きなかった」。
type Outcome struct {
	// Queued はこのイベントでキューへ積んだ INARA イベント数
	Queued int
	// Sent は送信できた件数(dryRun では送ったことにする件数)
	Sent int
	// Dropped は捨てた件数(上限超過・組み立て失敗・バッチ拒否)
	Dropped int
	// Skipped は uploadHistorical=false で送らなかった件数(遷移時に合計を 1 回)
	Skipped int
	// Pending はキューに残っている件数
	Pending int
	// Held は送信を見送った理由。空なら見送っていない
	Held string
	// DryRun は dryRun のときに組み立てた JSON
	DryRun []byte
	// Rejected は INARA が個別に拒否したイベント
	Rejected []inara.Rejection
	// Err は変換または送信の失敗
	Err error
	// Fatal は Err が恒久的で、キューを捨てたことを表す
	Fatal bool
}

type Uploader struct {
	now    func() time.Time
	sender Sender

	state *mapping.State
	queue *queue

	// sawLive は live のイベントを 1 度でも見たか。replay モードを抜ける条件。
	sawLive bool
	// skipped は uploadHistorical=false で捨てた件数(遷移時に報告)
	skipped int
	// backoff は直前のフラッシュがキューを空にできなかったことを表す。
	// 送れない状態で replay の全速リトライを繰り返さないための目印。
	backoff   bool
	lastFlush time.Time
}

func New(now func() time.Time, sender Sender) *Uploader {
	return &Uploader{
		now:       now,
		sender:    sender,
		state:     mapping.NewState(),
		queue:     newQueue(MaxQueued),
		lastFlush: now(),
	}
}

// Handle は Journal イベントを 1 件処理する。
func (u *Uploader) Handle(cfg settings.Settings, ev Event) Outcome {
	if !cfg.Enabled {
		return Outcome{}
	}
	// Status.json の更新には INARA へ送るものが無い。
	if ev.Kind != "journal" {
		return Outcome{}
	}

	var out Outcome

	// replay を抜けた瞬間を先に確定させる。スキップ件数の報告と、
	// 端数フラッシュはこの遷移に紐づく。
	transition := !u.sawLive && !ev.Replay
	if transition {
		u.sawLive = true
		out.Skipped = u.skipped
		u.skipped = 0
	}

	// 変換は replay を捨てる場合も通す。コマンダー名の学習は、送信対象で
	// なくても済ませておきたい(リプレイ中の LoadGame からでも学習してよい)。
	res, err := mapping.Convert(ev.Name, ev.Timestamp, json.RawMessage(ev.Payload), u.state)
	if err != nil {
		out.Err = err
	}

	// 遷移時に送り切るのは replay で溜まったぶん。バックログが無いのに
	// 最初の live イベントで即送信すると、minIntervalSeconds を無視して
	// 1 件だけ送ることになる。
	backlog := u.queue.len()

	if ev.Replay && !cfg.UploadHistorical {
		u.skipped += len(res.Events)
	} else {
		u.queue.push(res.Events)
		out.Queued = len(res.Events)
	}

	if u.shouldFlush(cfg, res.FlushLive, transition && backlog > 0) {
		u.flush(cfg, &out)
	}

	out.Pending = u.queue.len()
	return out
}

// shouldFlush は送信を試すべきかを決める。
//
// backlogTransition は「replay で溜まったものを抱えたまま live へ移った」場合に
// だけ true(Handle が判定する)。
func (u *Uploader) shouldFlush(cfg settings.Settings, flushLive, backlogTransition bool) bool {
	if u.queue.len() == 0 {
		return false
	}
	// replay の端数は live へ移る瞬間に送り切る。
	if backlogTransition {
		return true
	}

	if !u.sawLive {
		if u.queue.len() < ReplayBatchSize {
			return false
		}
		// 送れない状態(未承認・キー未設定・通信断)では急がない。
		if u.backoff {
			return u.intervalElapsed(cfg)
		}
		return true
	}

	// Shutdown を受け取った。次のイベントはもう来ない。
	if flushLive {
		return true
	}

	// batchSize が上限より大きいと永遠に条件を満たせないので頭打ちにする。
	batch := cfg.BatchSize
	if batch > MaxQueued {
		batch = MaxQueued
	}
	return u.queue.len() >= batch && u.intervalElapsed(cfg)
}

// intervalElapsed は前回のフラッシュ試行から minIntervalSeconds 経ったか。
// INARA は高頻度の送信を控えるよう求めている。
func (u *Uploader) intervalElapsed(cfg settings.Settings) bool {
	return u.now().Sub(u.lastFlush) >= time.Duration(cfg.MinIntervalSec)*time.Second
}

// flush は送信を試み、結果を out へ書く。
//
// 試行のたびに lastFlush を進める(送れなかった場合も含む)。そうしないと
// 送れない状態が続くあいだ、イベントごとに試行と報告を繰り返すことになる。
func (u *Uploader) flush(cfg settings.Settings, out *Outcome) {
	u.lastFlush = u.now()
	u.backoff = true
	out.Dropped += u.queue.takeDropped()

	commander := cfg.CommanderName
	if commander == "" {
		commander = u.state.CommanderName
	}
	if commander == "" {
		// INARA はヘッダにコマンダー名を要求する。LoadGame / Commander を
		// まだ見ていない段階では送れないので、次の機会を待つ。
		out.Held = "commander name not known yet"
		return
	}
	if cfg.APIKey == "" {
		out.Held = "apiKey is not configured"
		return
	}

	batch := u.queue.peek()
	body, err := inara.Encode(inara.Header{
		AppName:          appName,
		AppVersion:       appVersion,
		IsBeingDeveloped: cfg.IsBeingDeveloped,
		APIKey:           cfg.APIKey,
		CommanderName:    commander,
		FrontierID:       u.state.FrontierID,
	}, batch)
	if err != nil {
		// 組み立てに失敗したバッチは捨てる(残しても次回同じ失敗をする)。
		out.Err = err
		out.Fatal = true
		out.Dropped += len(batch)
		u.discard()
		return
	}

	if cfg.DryRun {
		out.DryRun = body
		out.Sent = len(batch)
		u.discard()
		return
	}

	status, respBody, err := u.sender.Send(body)
	if err != nil {
		// 送信そのものの失敗(ネットワーク・タイムアウト・未承認)。
		// キューは残して次のイベントで再試行する。
		out.Err = err
		return
	}

	result, err := inara.Interpret(status, respBody)
	if err != nil {
		var rejected *inara.BatchError
		if errors.As(err, &rejected) {
			out.Fatal = true
			out.Dropped += len(batch)
			u.discard()
		}
		out.Err = err
		return
	}

	out.Sent = len(batch)
	out.Rejected = result.Rejected
	u.discard()
}

// discard はキューを空にし、backoff を解除する。
func (u *Uploader) discard() {
	u.queue.clear()
	u.backoff = false
}
```

- [ ] **Step 4: テストが通ることを確認する**

Run: `go test ./uploader/ -v`
Expected: PASS(22 件 = Task 5 のキュー 5 件 + 本体 17 件)

- [ ] **Step 5: 全パッケージのテストを走らせる**

Run: `go test ./...`
Expected: `ok` × 4(`inara` / `mapping` / `settings` / `uploader`)

- [ ] **Step 6: コミット**

```bash
git add uploader/
git commit -m "feat(examples/inara): split replay and live upload modes in the uploader"
```

---

### Task 7: `main.go` をアダプタへ書き換える

**Files:**
- Rewrite: `main.go`
- Delete: `mapping.go`(旧、リポジトリルート直下のもの)、`inara.go`(旧)

**Interfaces:**
- Consumes: `uploader.New` / `Handle` / `Event` / `Outcome` / `MaxQueued`(Task 6)、`inara.Rejection`(Task 1)、`settings.Parse`(既存)
- Produces: なし(最終消費者)

- [ ] **Step 1: 旧ファイルを削除する**

```bash
git rm mapping.go inara.go
```

- [ ] **Step 2: `main.go` を書き換える**

```go
// inara-uploader は Journal イベントを INARA (https://inara.cz) の
// API v1 へアップロードする edlr プラグイン。
//
// このファイルはホスト境界のアダプタに徹する。設定を読んで uploader へ渡し、
// driver-http を呼び、返ってきた Outcome をログへ整形するだけで、判断は
// 持たない(main は //go:wasmimport を含むためテストが書けない)。
//
// 設計上の前提:
//   - プラグインはイベント到着時にしか動けない(ホストにタイマーが無い)。
//     そのため送信は「イベントを受け取ったついでに」行う。最後のイベントから
//     ゲーム終了までの間に溜まったぶんは、Journal の `Shutdown` でまとめて送る。
//   - edlr は Journal の読み取り位置を永続化しており、デーモン再起動をまたいで
//     続きから配信する。デーモンが動き出す前に既に書かれていたイベントには
//     `event.replay` が立つが、重複配信は起きないので既定ではこれも送る。
//
// 詳細は README.md を参照。
package main

import (
	"fmt"
	"time"

	"go.bytecodealliance.org/cm"

	driverhttp "github.com/himanoa/edlr/examples/plugins/inara-uploader/gen/edlr/plugin/driver-http"
	hostlog "github.com/himanoa/edlr/examples/plugins/inara-uploader/gen/edlr/plugin/host-log"
	hostsettings "github.com/himanoa/edlr/examples/plugins/inara-uploader/gen/edlr/plugin/host-settings"
	plugin "github.com/himanoa/edlr/examples/plugins/inara-uploader/gen/edlr/plugin/plugin"
	"github.com/himanoa/edlr/examples/plugins/inara-uploader/settings"
	"github.com/himanoa/edlr/examples/plugins/inara-uploader/uploader"
)

// inaraEndpoint は INARA API v1 のエンドポイント。manifest の
// `[[capabilities]]` で `https://inara.cz` を要求し、ユーザーが承認した
// 場合にだけ `driver-http.send` が通る。
const inaraEndpoint = "https://inara.cz/inapi/v1/"

var up *uploader.Uploader

func init() {
	plugin.Exports.Init = onInit
	plugin.Exports.OnEvent = onEvent
}

// main は TinyGo が component をビルドするために必要。エントリポイントとしては
// 使われない(ホストは `init` / `on-event` の export を直接呼ぶ)。
func main() {}

func onInit() {
	up = uploader.New(func() time.Time { return time.Now().UTC() }, httpSender{})
	logf(hostlog.LevelInfo, "inara-uploader initialized")
}

func onEvent(ev plugin.Event) {
	cfg := settings.Parse(hostsettings.GetAll())
	report(up.Handle(cfg, uploader.Event{
		Kind:      ev.Kind,
		Name:      option(ev.Name),
		Timestamp: option(ev.Timestamp),
		Payload:   ev.PayloadJSON,
		Replay:    ev.Replay,
	}))
}

// option は WIT の option<string> を素の string にする。
func option(o cm.Option[string]) string {
	if v := o.Some(); v != nil {
		return *v
	}
	return ""
}

// httpSender は driver-http による送信。
//
// ホスト側で 1.5 秒のタイムアウトが掛かっており、リダイレクトも追わない。
// タイムアウトやネットワークエラーはここで error になり、uploader が
// キューを保持して再試行する。
type httpSender struct{}

func (httpSender) Send(body []byte) (int, []byte, error) {
	result := driverhttp.Send(driverhttp.Request{
		Method: "POST",
		URL:    inaraEndpoint,
		Headers: cm.ToList([][2]string{
			{"content-type", "application/json"},
			{"accept", "application/json"},
		}),
		Body: cm.Some(cm.ToList(body)),
	})

	if err := result.Err(); err != nil {
		return 0, nil, fmt.Errorf("%s: %s", err.String(), driverErrorMessage(err))
	}

	resp := result.OK()
	return int(resp.Status), resp.Body.Slice(), nil
}

// driverErrorMessage は variant の中身(理由文字列)を取り出す。
func driverErrorMessage(err *driverhttp.DriverError) string {
	if m := err.PermissionDenied(); m != nil {
		return *m
	}
	if m := err.InvalidRequest(); m != nil {
		return *m
	}
	if m := err.Transport(); m != nil {
		return *m
	}
	return "unknown driver error"
}

// report は Outcome をログへ落とす。判断はせず、起きたことをそのまま出す。
func report(out uploader.Outcome) {
	if out.Skipped > 0 {
		logf(hostlog.LevelInfo,
			"skipped %d event(s) from the replayed backlog (set uploadHistorical to send them)",
			out.Skipped)
	}
	if out.Dropped > 0 {
		logf(hostlog.LevelWarn,
			"dropped %d queued event(s); the queue keeps the most recent %d",
			out.Dropped, uploader.MaxQueued)
	}
	if out.Held != "" {
		logf(hostlog.LevelWarn, "holding %d event(s): %s", out.Pending, out.Held)
	}
	if out.DryRun != nil {
		logf(hostlog.LevelInfo, "dry run: would upload %d event(s): %s", out.Sent, string(out.DryRun))
	}
	for _, rejected := range out.Rejected {
		logf(hostlog.LevelWarn, "inara rejected event %d: %d %s",
			rejected.Index, rejected.Status, rejected.StatusText)
	}
	if out.Err != nil {
		level := hostlog.LevelWarn
		if out.Fatal {
			level = hostlog.LevelError
		}
		logf(level, "%v", out.Err)
	}
	if out.Sent > 0 && out.DryRun == nil {
		logf(hostlog.LevelInfo, "uploaded %d event(s) to inara", out.Sent)
	}
}

func logf(level hostlog.Level, format string, args ...any) {
	hostlog.Log(level, fmt.Sprintf(format, args...))
}
```

- [ ] **Step 3: 型チェックを通す**

Run: `go vet ./...`
Expected: 出力なし、exit 0

`ev.Name` / `ev.Timestamp` の型が `cm.Option[string]` でない、`resp.Status` の型が違うなどのエラーが出たら、`gen/edlr/plugin/plugin/plugin.wit.go` と `gen/edlr/plugin/driver-http/driver-http.wit.go` の定義に合わせて `option` / `httpSender.Send` を直す。バインディングは変更しないこと。

- [ ] **Step 4: 全テストを走らせる**

Run: `go test ./...`
Expected: `ok` × 4

- [ ] **Step 5: 旧実装の残骸が無いことを確認する**

Run: `ls *.go`
Expected: `main.go` のみ

- [ ] **Step 6: コミット**

```bash
git add -A .
git commit -m "refactor(examples/inara): reduce main to a host boundary adapter"
```

---

### Task 8: README の更新

**Files:**
- Modify: `README.md`

**Interfaces:**
- Consumes: なし
- Produces: なし

- [ ] **Step 1: 「送信は『イベントを受け取ったついで』に行い〜」の段落(73-81 行付近)を差し替える**

差し替え後の本文:

```markdown
送信は「イベントを受け取ったついで」に行う。デーモンが動き出す前に書かれていた
イベント(`event.replay`)と、動き出した後のイベントでは経路が分かれる。

**replay(バックログを流し切る)**

- キューが 100 件たまるごとに送る
- `minIntervalSeconds` は適用しない(バックログを流し切ることを優先する)
- `Shutdown` を受け取っても送信は促さない。過去のゲーム終了ログであって
  「もうイベントは来ない」ことを意味しないため
- live のイベントを初めて受け取った時点で、溜まっている端数を送り切る

**live(通常の運用)**

- Journal の `Shutdown` を受け取った(ゲーム終了。以後イベントは来ない)
- キューが `batchSize` 以上あり、かつ前回の送信試行から `minIntervalSeconds` 経過

キューは**直近 200 件だけを保持する**。API キー未設定や capability 未承認で送れない
状態が続くと、古いものから順に捨てられる(捨てた件数はログに出る)。INARA は
「現在の状態」を反映するサービスなので、古い履歴を落として最新を残す。

送信に失敗した場合(ネットワーク不通・未承認など)はキューを保持して次の
イベントで再試行する。INARA がバッチ全体を拒否した場合(API キー不正など)は
恒久的な失敗なのでキューを捨てる。個々のイベントが拒否された場合はログに出す
だけで再送はしない(同じ内容を送り直しても通らないため)。
```

- [ ] **Step 2: 「対応イベント」表の直後にある注記(101-102 行付近)を差し替える**

置き換え前:

```markdown
`manifest.toml` の `events` に列挙したイベントしかプラグインへ届かない。
イベントを増やすときは `mapping.go` と `manifest.toml` の両方を直すこと。
```

置き換え後:

```markdown
`manifest.toml` の `events` に列挙したイベントしかプラグインへ届かない。イベントを
増やすときは `mapping/` のレジストリと `manifest.toml` の両方を直す。片方だけ直すと
`mapping/manifest_test.go` が落ちる。
```

- [ ] **Step 3: 「設定値の解釈は `settings` パッケージ〜」の段落(24-26 行付近)を差し替える**

置き換え後:

```markdown
判断を持つコードは `main` の外にある。`main` はホスト境界のアダプタ(設定を読む・
`driver-http` を呼ぶ・結果をログへ整形する)だけを担い、`uploader`(キューと送信
判断)・`mapping`(Journal → INARA 変換)・`inara`(リクエスト組み立てと応答解釈)・
`settings`(設定値の解釈)はいずれも `go test ./...` で検証できる。`main` パッケージは
`//go:wasmimport` を含むためネイティブでリンクできず、テストを書けない。

```
go test ./...   # ロジック層のテスト
go vet ./...    # main を含む全パッケージの型チェック
```
```

- [ ] **Step 4: 「不足している実装」1 番の本文に、キューの寿命について 1 行足す**

`デーモンだけを止めた場合も同じ` の直後に次を足す:

```markdown
- キューはメモリ上にしか無く、デーモンを止めると未送信分は失われる(Journal の
  読み取り位置は永続化されているので、再起動後に replay で取り直される)
```

- [ ] **Step 5: 変更後の README を通しで読み、事実と食い違う記述が無いか確認する**

特に確認する点:

- 冒頭の「動作確認済み」の記述は実機での確認を指しており、今回のリファクタでは再確認していない。**「動作確認済み」を「(旧実装で)動作確認済み」に直し、再ビルドと実機確認が未了であることを 1 行添える**
- 設定表(60-71 行付近)のキーと既定値は変わっていない
- 「バインディングの再生成」の手順は変わっていない

- [ ] **Step 6: コミット**

```bash
git add README.md
git commit -m "docs(examples/inara): document the replay and live upload modes"
```

---

## 完了条件

- [ ] `go test ./...` が 4 パッケージすべて `ok`
- [ ] `go vet ./...` が exit 0
- [ ] リポジトリルート直下の `mapping.go` / `inara.go` が消えている
- [ ] `ls *.go` が `main.go` のみ
- [ ] `git status` がクリーン

**残作業(この計画のスコープ外、himanoa さんへ引き継ぎ):** TinyGo と `wasm-tools` を用意して `./build.sh` を走らせ、実機の edlr デーモンで動作を確認する。
