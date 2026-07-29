---
id: golang-rust-sdk-9h8l
title: golangやRust向けのsdkを作って更新を簡単にできるようにする
summary: 
status: open
labels: 
created: 2026-07-29T05:46:52Z
updated: 2026-07-29T05:46:52Z
---

現状の実装だとABIに仕様変更が入るたびにSDK部分のwitを各リポジトリに `cp` で配る必要があり煩雑。golangやRust用のSDKを作ってそれに依存する形にして更新作業を楽にしたい
