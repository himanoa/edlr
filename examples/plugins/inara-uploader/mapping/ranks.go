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

// powerplay はパワープレイの所属。
type powerplay struct {
	Power  string `json:"Power"`
	Rank   int    `json:"Rank"`
	Merits *int64 `json:"Merits"`
}

func (p powerplay) convert(*State) []inara.Event {
	if p.Power == "" {
		return nil
	}
	return []inara.Event{inara.New("setCommanderRankPower", struct {
		Name   string `json:"powerName"`
		Rank   int    `json:"rankValue"`
		Merits *int64 `json:"meritsValue,omitempty"`
	}{p.Power, p.Rank, p.Merits})}
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
