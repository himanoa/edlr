// inara-uploader は Journal イベントを INARA (https://inara.cz) の
// API v1 へアップロードする edlr プラグイン。
//
// 設計上の前提:
//   - プラグインはイベント到着時にしか動けない(ホストにタイマーが無い)。
//     そのため送信は「イベントを受け取ったついでに」行う。最後のイベントから
//     ゲーム終了までの間に溜まったぶんは、Journal の `Shutdown` イベントで
//     まとめて送る。
//   - プラグインには永続ストレージが無い(`host-settings` は読み取り専用)。
//     そのため「どこまで送ったか」を再起動をまたいで覚えられない。デーモン
//     再起動時に Journal を先頭から読み直す edlr の挙動と組み合わさると
//     二重送信になるため、既定では**プラグイン起動時刻より古いイベントを
//     送らない**(`uploadHistorical` で変更可)。
//
// どちらも詳細は README.md の「不足している実装」を参照。
package main

import (
	"encoding/json"
	"fmt"
	"time"

	hostlog "github.com/himanoa/edlr/examples/plugins/inara-uploader/gen/edlr/plugin/host-log"
	hostsettings "github.com/himanoa/edlr/examples/plugins/inara-uploader/gen/edlr/plugin/host-settings"
	plugin "github.com/himanoa/edlr/examples/plugins/inara-uploader/gen/edlr/plugin/plugin"
)

// appName / appVersion は INARA のヘッダに載せるクライアント識別子。
const (
	appName    = "edlr-inara-uploader"
	appVersion = "0.1.0"
)

// キューの上限。これを超えたら `minInterval` を無視して強制送信する。
// 送信できないまま無制限にメモリを食うのを防ぐための保険。
const maxQueued = 200

// settings は manifest の `[[settings]]` に対応する。値は毎イベント
// `host-settings.get-all` から読み直すので、UI での変更は次のイベントから効く。
type settings struct {
	Enabled          bool
	APIKey           string
	CommanderName    string
	IsBeingDeveloped bool
	BatchSize        int
	MinIntervalSec   int
	UploadHistorical bool
	DryRun           bool
}

// state はプラグインのプロセス内状態。永続化はされない。
type state struct {
	// startedAt は init() が呼ばれた時刻。これより古い Journal イベントは
	// 既定では「過去に送信済みのはず」として捨てる(README の replay 節)。
	startedAt time.Time

	commanderName string
	frontierID    string
	// lastSystem は直近に確認した星系。Died は星系名を含まないため、
	// 移動系イベントから覚えておいたものを添える。
	lastSystem string

	queue      []inaraEvent
	lastFlush  time.Time
	lastRanks  map[string]int
	lastProg   map[string]float64
	skippedOld int
}

var st = &state{
	lastRanks: map[string]int{},
	lastProg:  map[string]float64{},
}

func init() {
	plugin.Exports.Init = onInit
	plugin.Exports.OnEvent = onEvent
}

// main は TinyGo が component をビルドするために必要。エントリポイントとしては
// 使われない(ホストは `init` / `on-event` の export を直接呼ぶ)。
func main() {}

func onInit() {
	st.startedAt = time.Now().UTC()
	st.lastFlush = st.startedAt
	logf(hostlog.LevelInfo, "inara-uploader initialized (started at %s)", st.startedAt.Format(time.RFC3339))
}

func onEvent(ev plugin.Event) {
	cfg := loadSettings()
	if !cfg.Enabled {
		return
	}

	// Status.json 更新には INARA へ送るものが無い。
	if ev.Kind != "journal" {
		return
	}

	name := ""
	if n := ev.Name.Some(); n != nil {
		name = *n
	}

	var payload map[string]any
	if err := json.Unmarshal([]byte(ev.PayloadJSON), &payload); err != nil {
		logf(hostlog.LevelWarn, "unparsable journal payload for %s: %v", name, err)
		return
	}

	timestamp := ""
	if t := ev.Timestamp.Some(); t != nil {
		timestamp = *t
	}

	// コマンダー識別子は、送信対象かどうかに関わらず常に拾っておく
	// (リプレイ中の LoadGame からでも名前は学習してよい)。
	learnIdentity(name, payload)

	if !cfg.UploadHistorical && isReplay(timestamp, st.startedAt) {
		st.skippedOld++
		if st.skippedOld == 1 || st.skippedOld%100 == 0 {
			logf(hostlog.LevelInfo,
				"skipping %d journal event(s) older than plugin start (set uploadHistorical to send them)",
				st.skippedOld)
		}
		return
	}

	st.queue = append(st.queue, mapEvent(name, timestamp, payload, st)...)

	flushIfDue(cfg, name)
}

// flushIfDue は送信条件を満たしていればキューを送る。
//
// 条件は次のいずれか:
//   - `Shutdown` を受け取った(ゲーム終了。次のイベントはもう来ない)
//   - キューが `maxQueued` を超えた(メモリ保護)
//   - キューが `batchSize` 以上あり、かつ前回送信から `minIntervalSec` 経過
//
// INARA はクライアントに高頻度の送信を控えるよう求めているため、通常経路では
// 必ず `minIntervalSec` を尊重する。
func flushIfDue(cfg settings, eventName string) {
	if len(st.queue) == 0 {
		return
	}

	forced := eventName == "Shutdown" || len(st.queue) >= maxQueued
	if !forced {
		if len(st.queue) < cfg.BatchSize {
			return
		}
		if time.Since(st.lastFlush) < time.Duration(cfg.MinIntervalSec)*time.Second {
			return
		}
	}

	flush(cfg)
}

func flush(cfg settings) {
	commander := cfg.CommanderName
	if commander == "" {
		commander = st.commanderName
	}
	if commander == "" {
		// INARA はヘッダにコマンダー名を要求する。まだ LoadGame を見ていない
		// 段階では送れないので、キューに積んだまま次の機会を待つ。
		logf(hostlog.LevelDebug, "holding %d event(s): commander name not known yet", len(st.queue))
		return
	}
	if cfg.APIKey == "" {
		logf(hostlog.LevelWarn, "holding %d event(s): apiKey is not configured", len(st.queue))
		return
	}

	batch := st.queue
	payload := inaraRequest{
		Header: inaraHeader{
			AppName:          appName,
			AppVersion:       appVersion,
			IsBeingDeveloped: cfg.IsBeingDeveloped,
			APIKey:           cfg.APIKey,
			CommanderName:    commander,
			FrontierID:       st.frontierID,
		},
		Events: batch,
	}

	body, err := json.Marshal(payload)
	if err != nil {
		// 組み立てに失敗したバッチは捨てる(残しても次回同じ失敗をするため)。
		logf(hostlog.LevelError, "dropping %d event(s): failed to encode payload: %v", len(batch), err)
		st.queue = nil
		st.lastFlush = time.Now()
		return
	}

	if cfg.DryRun {
		logf(hostlog.LevelInfo, "dry run: would upload %d event(s): %s", len(batch), string(body))
		st.queue = nil
		st.lastFlush = time.Now()
		return
	}

	result, err := postToInara(body)
	st.lastFlush = time.Now()
	if err != nil {
		// 送信自体が失敗(ネットワーク・タイムアウト・未承認)。キューは
		// 残して次のイベントで再試行する。上限は maxQueued。
		logf(hostlog.LevelWarn, "upload of %d event(s) failed, will retry: %v", len(batch), err)
		return
	}

	st.queue = nil
	logResult(result, len(batch))
}

// isReplay は、イベントのタイムスタンプがプラグイン起動より前かどうかを返す。
//
// edlr の Journal tailer はデーモン起動時に現行 Journal ファイルを先頭から
// 読み直すため、この判定が無いと再起動のたびに同じイベントを INARA へ
// 送ってしまう。パースできないタイムスタンプは「新しい」側に倒す
// (送らないより送るほうがマシ、かつ握り潰しを避ける)。
func isReplay(timestamp string, startedAt time.Time) bool {
	if timestamp == "" {
		return false
	}
	t, err := time.Parse(time.RFC3339, timestamp)
	if err != nil {
		return false
	}
	return t.Before(startedAt)
}

func learnIdentity(name string, payload map[string]any) {
	switch name {
	case "Commander", "LoadGame":
		if v, ok := payload["Name"].(string); ok && v != "" {
			st.commanderName = v
		}
		if v, ok := payload["Commander"].(string); ok && v != "" {
			st.commanderName = v
		}
		if v, ok := payload["FID"].(string); ok && v != "" {
			st.frontierID = v
		}
	}
}

func loadSettings() settings {
	cfg := settings{
		Enabled:          true,
		IsBeingDeveloped: true,
		BatchSize:        10,
		MinIntervalSec:   60,
	}

	var raw map[string]any
	if err := json.Unmarshal([]byte(hostsettings.GetAll()), &raw); err != nil {
		return cfg
	}

	if v, ok := raw["enabled"].(bool); ok {
		cfg.Enabled = v
	}
	if v, ok := raw["apiKey"].(string); ok {
		cfg.APIKey = v
	}
	if v, ok := raw["commanderName"].(string); ok {
		cfg.CommanderName = v
	}
	if v, ok := raw["isBeingDeveloped"].(bool); ok {
		cfg.IsBeingDeveloped = v
	}
	if v, ok := raw["batchSize"].(float64); ok && v >= 1 {
		cfg.BatchSize = int(v)
	}
	if v, ok := raw["minIntervalSeconds"].(float64); ok && v >= 0 {
		cfg.MinIntervalSec = int(v)
	}
	if v, ok := raw["uploadHistorical"].(bool); ok {
		cfg.UploadHistorical = v
	}
	if v, ok := raw["dryRun"].(bool); ok {
		cfg.DryRun = v
	}
	return cfg
}

func logf(level hostlog.Level, format string, args ...any) {
	hostlog.Log(level, fmt.Sprintf(format, args...))
}
