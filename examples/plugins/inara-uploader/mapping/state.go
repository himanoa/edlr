package mapping

// State はイベントをまたいで覚えておく情報。edlr は Journal の読み取り位置を
// 永続化しているため、デーモンを再起動しても現行セッションの LoadGame は
// **再配信されない**。そのため State は persist.go で JSON 化して driver-fs に
// 保存し、再起動時に読み戻す(揮発のままだと、セッション途中の再起動で次の
// LoadGame まで Live ゲートが閉じ、全イベントを捨ててしまう)。
type State struct {
	CommanderName string
	FrontierID    string
	// LastSystem は直近に確認した星系。Died は星系名を含まないため、
	// 移動系イベントから覚えておいたものを添える。
	LastSystem string
	// LastStation は直近にドッキングしたステーション。ミッション受注地や
	// 船の輸送先(ShipyardTransfer)のように、Journal 側がステーション名を
	// 含まないイベントに添える。
	LastStation string

	// ShipType / ShipID は現在搭乗している船。Touchdown(着陸)が INARA 側で
	// 船種を要求するため、Loadout / Shipyard 系イベントから覚えておく。
	// ShipID がポインタなのは「0 番の船」と「未学習」を区別するため。
	ShipType string
	ShipID   *int64

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
