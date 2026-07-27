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

func (c *clock) now() time.Time          { return c.t }
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

// replay 中に送れない状態(API キー未設定など)が続くと backoff が立ち、
// minIntervalSeconds が経つまで全速リトライしない。
func TestReplayBacksOffWhenItCannotSend(t *testing.T) {
	sender := okSender()
	c := newClock()
	u := New(c.now, sender)
	cfg := testSettings()
	cfg.APIKey = ""

	outs := feed(u, cfg, ReplayBatchSize, true)
	last := outs[len(outs)-1]
	if last.Held == "" {
		t.Fatalf("expected the first flush attempt to be held, got %+v", last)
	}

	// 時計を進めずに追加の replay イベントを流す。backoff が立っているので
	// 全速リトライは起きず、Held は再び現れないはずである。
	for _, out := range feed(u, cfg, 5, true) {
		if out.Held != "" {
			t.Errorf("expected no retry before the interval elapses, got %+v", out)
		}
	}

	c.advance(time.Duration(cfg.MinIntervalSec) * time.Second)
	retry := u.Handle(cfg, jump("Sol", true))
	if retry.Held == "" {
		t.Errorf("expected the retry to be attempted once the interval elapsed, got %+v", retry)
	}
	if len(sender.calls) != 0 {
		t.Errorf("nothing may be uploaded without an api key, got %d", len(sender.calls))
	}
}

// バックログが無ければ、最初の live イベントだけでは送信しない
// (batchSize/minIntervalSeconds を満たすまで待つ)。
func TestFirstLiveEventWithoutBacklogDoesNotFlush(t *testing.T) {
	sender := okSender()
	u := New(newClock().now, sender)
	cfg := testSettings()

	out := u.Handle(cfg, jump("Sol", false))
	if len(sender.calls) != 0 {
		t.Errorf("the first live event alone must not flush, got %d uploads / %+v", len(sender.calls), out)
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
