ドライバ(ホスト側 `bindgen!` が使う)。`bus` は import しない
-- ドライバ間の呼び出しを構造的に不可能にするため。`on-event` も無い
-- ドライバは journal / status を受け取らない。