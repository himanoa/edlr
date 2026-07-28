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
	Send(body []byte) (status int, respBody []byte, err error)
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
	// Attempted は実際に HTTP 送信を試みたバッチの件数。送信の成否は
	// 問わない(成功なら Sent、失敗なら Err に現れる)。Held・dryRun・
	// キューに積んだだけの場合は 0
	Attempted int
	// Held は送信を見送った理由。空なら見送っていない
	Held string
	// DryRun は dryRun のときに組み立てた JSON
	DryRun []byte
	// Rejected は INARA が個別に拒否した(400 以上)イベント
	Rejected []inara.Rejection
	// Warned は個別イベントの eventStatus が 200 以外の 2xx だったもの。
	// 拒否ではないので再送しないが、警告として報告できるようにしておく。
	Warned []inara.Rejection
	// Warning はヘッダの eventStatus が 200 以外の 2xx だったことを表す。
	// 空なら警告なし。バッチとしては成功しておりキューは捨てている。
	Warning string
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

// HandleSchedule は manifest の `[[schedule]]` (name = "flush") から呼ばれる。
// minIntervalSeconds を尊重するので、直近のイベントで送り切れなかった端数を
// 定期的に拾い上げる役割になる。
//
// replay 中(!u.sawLive)は送らない。replay のバッチングは Handle/shouldFlush
// が ReplayBatchSize 件ごとにまとめて送る設計になっており(全速で流し切る
// バックログを minIntervalSeconds 無視で大きなバッチにまとめるのが目的)、
// ここでスケジュール発火が割り込んで端数を先に送ってしまうと、その意図的な
// バッチングが壊れる(replay に 60 秒以上かかると、ReplayBatchSize 未満の
// 半端な件数を先に送ってしまう)。replay 中に溜まったキューは、live へ
// 移る遷移か次の ReplayBatchSize 到達で Handle が送る。
func (u *Uploader) HandleSchedule(cfg settings.Settings) Outcome {
	if !cfg.Enabled {
		return Outcome{}
	}

	var out Outcome
	if u.sawLive && u.queue.len() > 0 && u.intervalElapsed(cfg) {
		u.flush(cfg, &out)
	}
	out.Pending = u.queue.len()
	return out
}

// HandleStop はデーモンの graceful shutdown (`on-stop`) から呼ばれる。次の
// イベントはもう来ないので、minIntervalSeconds を無視して最後の機会として
// 無条件にフラッシュする。
//
// これは意図した仕様(送れるものは送ってから終了する)であり、
// HandleSchedule と違って replay 中の sawLive ガードは入れない。そのため
// replay の途中でデーモンが止まった場合、ReplayBatchSize 未満の半端な件数を
// 送ってしまうことがあるが、次のイベントはもう来ない(デーモンは終了する)
// ので、送らずに失うより良いという判断で許容する。
func (u *Uploader) HandleStop(cfg settings.Settings) Outcome {
	if !cfg.Enabled {
		return Outcome{}
	}

	var out Outcome
	if u.queue.len() > 0 {
		u.flush(cfg, &out)
	}
	out.Pending = u.queue.len()
	return out
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
		// これは恒久的な失敗なので backoff は立てたままにする。ここで
		// backoff を解除すると、replay 中に API キー不正のような恒久エラーが
		// 起きても shouldFlush が intervalElapsed を見ずに次の
		// ReplayBatchSize 件で即座にまた送信してしまう。
		setErr(out, err)
		out.Fatal = true
		out.Dropped += len(batch)
		u.discard()
		return
	}

	if cfg.DryRun {
		out.DryRun = body
		out.Sent = len(batch)
		u.backoff = false
		u.discard()
		return
	}

	out.Attempted = len(batch)
	status, respBody, err := u.sender.Send(body)
	if err != nil {
		// 送信そのものの失敗(ネットワーク・タイムアウト・未承認)。
		// キューは残して次のイベントで再試行する。backoff は関数の先頭で
		// 立てたままにする(送れていないため)。
		setErr(out, err)
		return
	}

	result, err := inara.Interpret(status, respBody)
	if err != nil {
		var rejected *inara.BatchError
		if errors.As(err, &rejected) {
			// バッチ拒否も恒久的な失敗。backoff は立てたままキューだけ捨てる
			// (上の Encode 失敗と同じ理由)。
			out.Fatal = true
			out.Dropped += len(batch)
			u.discard()
		}
		setErr(out, err)
		return
	}

	// ここまで来れば送信は成功している(202/204 のような警告つきの成功も
	// 含む)。backoff はここでだけ解除する。
	out.Sent = len(batch)
	out.Rejected = result.Rejected
	out.Warned = result.Warned
	out.Warning = result.Warning
	u.backoff = false
	u.discard()
}

// setErr は out.Err が未設定のときだけ書き込む。mapping.Convert の変換
// エラーを Handle が先に out.Err へ入れている場合があり、ここで無条件に
// 上書きすると変換失敗のログが消えてしまう(変換失敗とフラッシュ失敗が
// 同じイベントで重なるとき)。先に起きた変換エラーを優先する。
func setErr(out *Outcome, err error) {
	if out.Err == nil {
		out.Err = err
	}
}

// discard はキューを空にする。backoff の扱いは呼び出し側(flush)が
// 状況に応じて決める。
func (u *Uploader) discard() {
	u.queue.clear()
}
