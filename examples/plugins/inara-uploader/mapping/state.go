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

	// gameVersion は直近の LoadGame で確認したゲームのバージョン。
	// INARA は Live(4.0 以降、beta 除く)のデータしか受け付けないため、
	// これが Live と確認できるまでは何も送らない(未学習も送らない側に倒す。
	// journal の途中から replay した場合、次の LoadGame までは捨てる)。
	gameVersion string
}

func NewState() *State {
	return &State{
		ranks:    map[string]int{},
		progress: map[string]float64{},
	}
}

// learnGameVersion は空でない値だけを取り込む(LoadGame にフィールドが
// 無い古い journal で、学習済みの値を消さないようにする)。
func (s *State) learnGameVersion(version string) {
	if version != "" {
		s.gameVersion = version
	}
}

// liveAllowed は現在のセッションが INARA へ送ってよい Live 版かどうか。
func (s *State) liveAllowed() bool {
	return isLiveVersion(s.gameVersion)
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
