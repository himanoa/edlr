# Inara API アプリ登録依頼文

送り先: Artie(Inara 管理者)— https://inara.cz/elite/cmdr/1/ のプロフィールからメッセージ、
またはフッターの Contact から。以下をそのまま貼り付けて送れます。

---

Subject: API app white-list request: edlr-inara-uploader

Hi Artie,

I'd like to request white-listing of a new application for the Inara API.

**1. Application name (exactly as sent in requests):**
`edlr-inara-uploader`

**2. What the application does:**
It is a plugin for edlr (EliteDangerousLogRouter), an open-source local daemon
that tails the Elite Dangerous journal files and routes events to sandboxed
WASM plugins. The inara-uploader plugin maps journal events to Inara API
events and uploads them for the local player only — each user supplies their
own personal Inara API key. Events currently sent: addCommanderTravelFSDJump,
addCommanderTravelDock, addCommanderTravelCarrierJump, addCommanderCombatDeath,
setCommanderTravelLocation, setCommanderCredits, setCommanderRankPilot,
setCommanderRankEngineer, setCommanderReputationMajorFaction,
setCommanderInventoryMaterials and setCommanderGameStatistics.
Events are batched (flushed on Docked/FSDJump and on a timed interval with
backoff on errors), only Live game data is sent, and beta data is not sent.
I am currently testing with `isBeingDeveloped: true` and will switch it off
once everything works correctly.

**3. Short description (for the Inara application list):**
edlr plugin that keeps your Inara commander profile (location, credits, ranks,
reputation, materials and statistics) up to date from your local journal files.

**4. URL / homepage:**
https://github.com/himanoa/edlr
(the plugin lives in `examples/plugins/inara-uploader/`)

Thank you for maintaining Inara and its API!

CMDR <あなたの CMDR 名 / Inara ユーザー名>

---

## 送信前チェック

- [ ] 末尾の CMDR 名を自分の名前に置き換える
- [ ] リポジトリを公開していない場合は URL 行を "not yet published" 等に変える
- [ ] 登録が通ったら動作確認 → Plugins 画面で `isBeingDeveloped` をオフにする
