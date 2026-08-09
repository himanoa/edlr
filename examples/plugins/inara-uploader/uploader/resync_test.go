package uploader

import (
	"encoding/json"
	"errors"
	"fmt"
	"strings"
	"testing"

	"github.com/himanoa/edlr/examples/plugins/inara-uploader/settings"
)

type fakeFS struct {
	files map[string][]byte
	// listErr / readErr で失敗を注入する
	listErr error
	readErr error
}

func (f *fakeFS) List() ([]FileInfo, error) {
	if f.listErr != nil {
		return nil, f.listErr
	}
	infos := make([]FileInfo, 0, len(f.files))
	for name, data := range f.files {
		infos = append(infos, FileInfo{Path: name, Size: uint64(len(data))})
	}
	return infos, nil
}

func (f *fakeFS) ReadRange(path string, offset uint64, n uint32) ([]byte, error) {
	if f.readErr != nil {
		return nil, f.readErr
	}
	data := f.files[path]
	if offset >= uint64(len(data)) {
		return nil, nil
	}
	end := offset + uint64(n)
	if end > uint64(len(data)) {
		end = uint64(len(data))
	}
	return data[offset:end], nil
}

type fakeSubmitter struct {
	bodies [][]byte
	err    error
}

func (s *fakeSubmitter) Submit(body []byte) error {
	if s.err != nil {
		return s.err
	}
	s.bodies = append(s.bodies, body)
	return nil
}

func enabledSettings() settings.Settings {
	return settings.Settings{Enabled: true, APIKey: "key", CommanderName: "CMDR Test"}
}

func loadGameLine(i int) string {
	return fmt.Sprintf(
		`{"timestamp":"2026-08-09T20:%02d:00Z","event":"LoadGame","Commander":"Foo","FID":"F1","Credits":%d,"gameversion":"4.0.0.1904"}`,
		i%60, 1000+i)
}

func journalOf(lines ...string) []byte {
	return []byte(strings.Join(lines, "\n") + "\n")
}

func okResponse() []byte {
	return []byte(`{"header":{"eventStatus":200,"eventStatusText":"ok"}}`)
}

func decodeEvents(t *testing.T, body []byte) []map[string]any {
	t.Helper()
	var req struct {
		Events []map[string]any `json:"events"`
	}
	if err := json.Unmarshal(body, &req); err != nil {
		t.Fatalf("送信ボディが JSON ではありません: %v", err)
	}
	return req.Events
}

// 100 件を超えるイベントが 1 バッチ 100 件ずつの数珠つなぎで送られ、
// 最後に累計とともに完了することの検証。
func TestResyncSendsTheSessionInBatches(t *testing.T) {
	lines := make([]string, 0, 150)
	for i := 0; i < 150; i++ {
		lines = append(lines, loadGameLine(i))
	}
	fs := &fakeFS{files: map[string][]byte{"Journal.2026-08-09T201135.01.log": journalOf(lines...)}}
	sender := &fakeSubmitter{}
	r := NewResync(fs, sender)

	out := r.Start(enabledSettings())
	if out.Refused != "" || out.Err != nil {
		t.Fatalf("開始できません: %+v", out)
	}
	if out.Submitted != ResyncBatchSize {
		t.Fatalf("最初のバッチが %d 件です(期待 %d)", out.Submitted, ResyncBatchSize)
	}
	if got := len(decodeEvents(t, sender.bodies[0])); got != ResyncBatchSize {
		t.Fatalf("送信ボディのイベント数が %d です", got)
	}

	out = r.OnSendResult(200, okResponse(), nil, enabledSettings())
	if out.Sent != ResyncBatchSize || out.Submitted != 50 || out.Done {
		t.Fatalf("2 バッチ目の状態が不正: %+v", out)
	}

	out = r.OnSendResult(200, okResponse(), nil, enabledSettings())
	if !out.Done || out.Err != nil || out.Total != 150 || out.Sent != 50 {
		t.Fatalf("完了状態が不正: %+v", out)
	}

	// 完了後はもう一度開始できる
	if out := r.Start(enabledSettings()); out.Refused != "" {
		t.Fatalf("完了後に再開始できません: %s", out.Refused)
	}
}

// Live と確認できるまでのイベントは送らない(mapping のゲートが再送でも
// 効いている)ことの検証。gameversion の無い LoadGame は学習も送信もされず、
// Live 版の LoadGame 以降だけが載る。
func TestResyncDropsEventsUntilTheSessionIsKnownLive(t *testing.T) {
	preGate := `{"timestamp":"2026-08-09T20:00:00Z","event":"LoadGame","Commander":"Foo","FID":"F1","Credits":1}`
	fs := &fakeFS{files: map[string][]byte{
		"Journal.2026-08-09T201135.01.log": journalOf(preGate, loadGameLine(1)),
	}}
	sender := &fakeSubmitter{}
	r := NewResync(fs, sender)

	out := r.Start(enabledSettings())
	if out.Submitted != 1 {
		t.Fatalf("Live 確認前のイベントが送られています(submitted=%d)", out.Submitted)
	}
}

func TestResyncRefusals(t *testing.T) {
	journal := map[string][]byte{"Journal.2026-08-09T201135.01.log": journalOf(loadGameLine(1))}

	cases := []struct {
		name string
		cfg  settings.Settings
	}{
		{"disabled", settings.Settings{Enabled: false, APIKey: "key"}},
		{"no api key", settings.Settings{Enabled: true}},
		{"dry run", settings.Settings{Enabled: true, APIKey: "key", DryRun: true}},
	}
	for _, tc := range cases {
		r := NewResync(&fakeFS{files: journal}, &fakeSubmitter{})
		if out := r.Start(tc.cfg); out.Refused == "" {
			t.Errorf("%s: 拒否されるべき開始が通りました", tc.name)
		}
	}

	// 実行中の再押下は無視
	r := NewResync(&fakeFS{files: journal}, &fakeSubmitter{})
	if out := r.Start(enabledSettings()); out.Refused != "" {
		t.Fatalf("開始できません: %s", out.Refused)
	}
	if out := r.Start(enabledSettings()); out.Refused == "" {
		t.Error("実行中の Start が拒否されません")
	}
}

// バッチ拒否(認証エラー等)は再送を中断し、状態がリセットされて
// もう一度開始できることの検証。
func TestResyncAbortsOnBatchRejection(t *testing.T) {
	fs := &fakeFS{files: map[string][]byte{"Journal.2026-08-09T201135.01.log": journalOf(loadGameLine(1))}}
	r := NewResync(fs, &fakeSubmitter{})
	if out := r.Start(enabledSettings()); out.Refused != "" {
		t.Fatalf("開始できません: %s", out.Refused)
	}

	rejected := []byte(`{"header":{"eventStatus":400,"eventStatusText":"no access"}}`)
	out := r.OnSendResult(200, rejected, nil, enabledSettings())
	if !out.Done || out.Err == nil {
		t.Fatalf("バッチ拒否で中断していません: %+v", out)
	}
	if out := r.Start(enabledSettings()); out.Refused != "" {
		t.Fatalf("中断後に再開始できません: %s", out.Refused)
	}
}

func TestResyncAbortsOnTransportError(t *testing.T) {
	fs := &fakeFS{files: map[string][]byte{"Journal.2026-08-09T201135.01.log": journalOf(loadGameLine(1))}}
	r := NewResync(fs, &fakeSubmitter{})
	if out := r.Start(enabledSettings()); out.Refused != "" {
		t.Fatalf("開始できません: %s", out.Refused)
	}
	out := r.OnSendResult(0, nil, errors.New("timeout"), enabledSettings())
	if !out.Done || out.Err == nil {
		t.Fatalf("送信エラーで中断していません: %+v", out)
	}
}

// 最新の Journal ファイル(辞書順の最大)だけが対象になり、Journal 以外の
// ファイルは無視されることの検証。
func TestResyncPicksTheLatestJournalOnly(t *testing.T) {
	old := journalOf(loadGameLine(1), loadGameLine(2))
	latest := journalOf(loadGameLine(3))
	fs := &fakeFS{files: map[string][]byte{
		"Journal.2026-08-08T235828.01.log": old,
		"Journal.2026-08-09T201135.01.log": latest,
		"Status.json":                      []byte(`{}`),
	}}
	sender := &fakeSubmitter{}
	r := NewResync(fs, sender)

	out := r.Start(enabledSettings())
	if out.Submitted != 1 {
		t.Fatalf("最新ファイルの 1 件だけが送られるはずが submitted=%d", out.Submitted)
	}
}
