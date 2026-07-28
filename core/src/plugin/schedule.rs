//! プラグインスケジュールの「次回発火時刻」計算(純粋モジュール)。
//!
//! この型自身は壁時計を読まない。すべての「現在時刻」は引数として
//! `chrono::DateTime<chrono::Local>` で渡される(テストで時刻を固定するため)。
//!
//! ## 壁時計 vs 単調時計(設計からの逸脱)
//!
//! 設計時点(`docs/superpowers/specs/2026-07-28-plugin-scheduler-design.md`)
//! では interval スケジュールの発火間隔追跡には単調時計(`Instant`)を使い、
//! cron の定刻計算にだけ壁時計(`chrono::Local`)を使う想定だった。実装では
//! interval・cron の両方を `chrono::Local` の壁時計で統一して扱っている
//! (この `Entry::next` も `runner.rs` 側の現在時刻取得も、常に
//! `chrono::Local::now()` 系統)。
//!
//! これは意図的な逸脱であり、値そのものは変更しない: 時刻表現をひとつに
//! 統一した方が実装・テストが単純になり、`plugins/list` の応答に載せる
//! `next`(ISO8601 の壁時計時刻)も結局はどこかで壁時計へ変換する必要が
//! ある。また前方への時刻ジャンプ(NTP 補正など)はそもそも「1 回だけ
//! 発火してそれ以降追いつく」既存のクランプ処理で吸収される。
//!
//! トレードオフとして、interval スケジュールは NTP のステップ補正や
//! サスペンド/レジュームの影響を単調時計より受けやすい:
//! - 前方へのステップ(時計が進む方向)-- 上記のクランプにより、単に
//!   1 回早めに(コアレスして)発火するだけで済む
//! - 後方へのステップ(時計が戻る方向)-- 次回発火が、戻った分だけ
//!   遅延する
//!
//! いずれも許容している: edlr のスケジュールはユーザー体感のための
//! おおまかな定期実行であり、秒単位の厳密さは要求されない。

use std::str::FromStr;
use std::time::Duration;

use chrono::{DateTime, Local};

use super::manifest::{normalize_cron, ScheduleRequest, ScheduleSpec};

/// 実効最小発火間隔。これより短い interval や cron の間隔はここまで
/// クランプされる(5 秒未満の連射を防ぐ)。
pub(crate) const MIN_FIRE_INTERVAL: Duration = Duration::from_secs(5);

/// 1 スケジュールぶんの発火計算方法。
#[derive(Debug, Clone)]
enum Fire {
    /// 固定間隔(`MIN_FIRE_INTERVAL` 未満は丸め済み)。
    Interval(chrono::Duration),
    /// cron 式(正規化前の 5 欄形式)。`cron::Schedule` は大きいため
    /// `Interval` とのサイズ差を抑えるべく Box で包む。
    ///
    /// `clamp_warned` は「この cron の間隔クランプについて、既に
    /// warn ログを出したか」を追跡する。間隔の詰まった cron 式は
    /// 発火のたびにクランプされ得るため、毎回 warn すると発火のたびに
    /// ログが出てスパムになる。スケジュールごとに 1 回だけ warn する。
    Cron {
        schedule: Box<cron::Schedule>,
        clamp_warned: bool,
    },
}

/// 1 スケジュールの実行時状態。
#[derive(Debug, Clone)]
struct Entry {
    name: String,
    fire: Fire,
    next: DateTime<Local>,
}

/// 1 プラグインの全スケジュールの発火状態。
///
/// 壁時計は引数で受け取り、この型自身は時計を読まない(テストのため)。
pub(crate) struct ScheduleState {
    entries: Vec<Entry>,
}

impl ScheduleState {
    /// manifest から構築する。下限未満の interval は 5 秒へ丸めて
    /// warn ログを出した状態にする。
    pub fn new(schedules: &[ScheduleRequest], now: DateTime<Local>) -> Self {
        let entries = schedules
            .iter()
            .map(|req| Self::build_entry(req, now))
            .collect();
        ScheduleState { entries }
    }

    fn build_entry(req: &ScheduleRequest, now: DateTime<Local>) -> Entry {
        match &req.spec {
            ScheduleSpec::IntervalSeconds(secs) => {
                let effective = if Duration::from_secs(*secs) < MIN_FIRE_INTERVAL {
                    tracing::warn!(
                        schedule = %req.name,
                        requested_seconds = secs,
                        "interval-seconds below the minimum fire interval; clamped to {}s",
                        MIN_FIRE_INTERVAL.as_secs()
                    );
                    MIN_FIRE_INTERVAL
                } else {
                    Duration::from_secs(*secs)
                };
                let interval = chrono::Duration::from_std(effective)
                    .unwrap_or_else(|_| chrono::Duration::seconds(MIN_FIRE_INTERVAL.as_secs() as i64));
                Entry {
                    name: req.name.clone(),
                    next: now + interval,
                    fire: Fire::Interval(interval),
                }
            }
            ScheduleSpec::Cron(expr) => {
                let normalized = normalize_cron(expr);
                // このマニフェストは登録時点で `cron::Schedule::from_str` に
                // 通っていることが保証されているが、想定外の入力でパニックしない
                // よう防御的にフォールバックする(発火しないスケジュールとして扱う)。
                let schedule = cron::Schedule::from_str(&normalized).unwrap_or_else(|e| {
                    tracing::warn!(
                        schedule = %req.name,
                        cron = %expr,
                        error = %e,
                        "failed to re-parse a supposedly validated cron expression; \
                         falling back to a far-future schedule"
                    );
                    // 実質的に発火しない(パースできる無害なダミー式)。
                    cron::Schedule::from_str("0 0 0 1 1 * 2999").expect("dummy cron must parse")
                });
                let mut clamp_warned = false;
                let next = Self::next_cron_fire(&req.name, &schedule, now, &mut clamp_warned);
                Entry {
                    name: req.name.clone(),
                    fire: Fire::Cron {
                        schedule: Box::new(schedule),
                        clamp_warned,
                    },
                    next,
                }
            }
        }
    }

    /// cron の次回発火時刻を求め、`MIN_FIRE_INTERVAL` 未満なら
    /// `now + MIN_FIRE_INTERVAL` まで遅らせる(5 秒間隔へのクランプ)。
    ///
    /// クランプが発生した場合、`clamp_warned` がまだ false のときだけ
    /// `tracing::warn!` を出し、以降は `clamp_warned` を true にして
    /// 同じスケジュールでの毎発火の warn(ログスパム)を防ぐ。
    fn next_cron_fire(
        name: &str,
        schedule: &cron::Schedule,
        now: DateTime<Local>,
        clamp_warned: &mut bool,
    ) -> DateTime<Local> {
        let candidate = schedule.after(&now).next().unwrap_or_else(|| {
            // cron クレートの実装上まず起こらないが、念のため防御する。
            now + chrono::Duration::from_std(MIN_FIRE_INTERVAL).unwrap()
        });
        let min_next = now
            + chrono::Duration::from_std(MIN_FIRE_INTERVAL)
                .unwrap_or_else(|_| chrono::Duration::seconds(5));
        if candidate < min_next {
            if !*clamp_warned {
                tracing::warn!(
                    schedule = %name,
                    "cron-produced gap below the minimum fire interval; clamped to {}s \
                     (further clamps for this schedule will not be logged)",
                    MIN_FIRE_INTERVAL.as_secs()
                );
                *clamp_warned = true;
            }
            min_next
        } else {
            candidate
        }
    }

    /// 各スケジュールの `(name, 次回発火時刻)` を宣言順に返す。
    ///
    /// `plugins/list` の RPC 応答(`registry.rs`)が「表示用の次回発火時刻」を
    /// 組み立てるための純粋な読み取り専用アクセサ。この型自身は壁時計を
    /// 読まないので、呼び出し側が現在時刻を渡す(`until_next`/`take_due` と
    /// 同じ流儀)。
    pub(crate) fn next_times(&self) -> Vec<(&str, DateTime<Local>)> {
        self.entries
            .iter()
            .map(|e| (e.name.as_str(), e.next))
            .collect()
    }

    /// 次の発火までの残り時間(スケジュールが無ければ None)。
    pub fn until_next(&self, now: DateTime<Local>) -> Option<Duration> {
        self.entries
            .iter()
            .map(|e| e.next)
            .min()
            .map(|next| (next - now).to_std().unwrap_or(Duration::ZERO))
    }

    /// 期限が来ているスケジュール名を最大 1 つ返し、その次回時刻を
    /// 「未来の直近」まで進める。
    ///
    /// 複数のスケジュールが同時に期限切れでも 1 回の呼び出しでは 1 件だけ
    /// 返す(呼び出し側がループで繰り返し呼ぶ想定)。同時に複数が期限切れの
    /// 場合の順序は宣言順で決定的。
    pub fn take_due(&mut self, now: DateTime<Local>) -> Option<String> {
        let idx = self.entries.iter().position(|e| e.next <= now)?;
        let entry = &mut self.entries[idx];
        let name = entry.name.clone();
        entry.next = Self::advance_to_future(&name, &mut entry.fire, entry.next, now);
        Some(name)
    }

    /// どれだけ遅れていても、`next` を「now より未来の直近の発火時刻」まで
    /// 一気に進める(見逃した発火は 1 回に集約される)。
    fn advance_to_future(
        name: &str,
        fire: &mut Fire,
        next: DateTime<Local>,
        now: DateTime<Local>,
    ) -> DateTime<Local> {
        match fire {
            Fire::Interval(interval) => {
                let mut next = next;
                // interval は必ず正(MIN_FIRE_INTERVAL 未満は構築時に丸め済み)。
                while next <= now {
                    next += *interval;
                }
                next
            }
            Fire::Cron {
                schedule,
                clamp_warned,
            } => Self::next_cron_fire(name, schedule, now, clamp_warned),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn req(name: &str, spec: ScheduleSpec) -> ScheduleRequest {
        ScheduleRequest {
            name: name.to_string(),
            spec,
        }
    }

    #[test]
    fn interval_fires_after_the_interval() {
        let t0 = Local.with_ymd_and_hms(2026, 7, 28, 0, 0, 0).unwrap();
        let mut s = ScheduleState::new(&[req("flush", ScheduleSpec::IntervalSeconds(60))], t0);
        assert_eq!(s.take_due(t0), None);
        assert_eq!(s.until_next(t0), Some(Duration::from_secs(60)));
        assert_eq!(
            s.take_due(t0 + chrono::Duration::seconds(60)),
            Some("flush".into())
        );
    }

    #[test]
    fn missed_fires_are_coalesced_to_one() {
        let t0 = Local.with_ymd_and_hms(2026, 7, 28, 0, 0, 0).unwrap();
        let mut s = ScheduleState::new(&[req("flush", ScheduleSpec::IntervalSeconds(60))], t0);

        // 10 分経過(本来 10 回発火しているはず)。
        let t_later = t0 + chrono::Duration::minutes(10);
        assert_eq!(s.take_due(t_later), Some("flush".into()));
        // 2 回目は None(同時刻ではもう期限切れがない = 1 回に集約された)。
        assert_eq!(s.take_due(t_later), None);
        // 次回は未来(正の残り時間)。
        let remaining = s.until_next(t_later).expect("schedule should remain");
        assert!(remaining > Duration::ZERO);
    }

    #[test]
    fn interval_below_minimum_is_clamped_to_five_seconds() {
        let t0 = Local.with_ymd_and_hms(2026, 7, 28, 0, 0, 0).unwrap();
        let mut s = ScheduleState::new(&[req("tick", ScheduleSpec::IntervalSeconds(1))], t0);
        assert_eq!(s.until_next(t0), Some(Duration::from_secs(5)));
        assert_eq!(s.take_due(t0 + chrono::Duration::seconds(4)), None);
        assert_eq!(
            s.take_due(t0 + chrono::Duration::seconds(5)),
            Some("tick".into())
        );
    }

    #[test]
    fn cron_fires_at_the_wall_clock_time() {
        let t0 = Local.with_ymd_and_hms(2026, 7, 28, 8, 59, 0).unwrap();
        let mut s = ScheduleState::new(
            &[req("daily", ScheduleSpec::Cron("0 9 * * *".to_string()))],
            t0,
        );
        assert_eq!(s.until_next(t0), Some(Duration::from_secs(60)));
        assert_eq!(s.take_due(t0), None);
        let t_fire = Local.with_ymd_and_hms(2026, 7, 28, 9, 0, 0).unwrap();
        assert_eq!(s.take_due(t_fire), Some("daily".into()));
    }

    #[test]
    fn two_schedules_fire_independently() {
        // interval は 70 秒にして、cron の分境界(09:00:00)と重ならない
        // ようにする(でないと両方が同時刻で期限切れになり得る)。
        let t0 = Local.with_ymd_and_hms(2026, 7, 28, 8, 58, 0).unwrap();
        let mut s = ScheduleState::new(
            &[
                req("flush", ScheduleSpec::IntervalSeconds(70)),
                req("daily", ScheduleSpec::Cron("0 9 * * *".to_string())),
            ],
            t0,
        );

        // 08:59:10: interval 側だけが期限切れ。
        let t1 = t0 + chrono::Duration::seconds(70);
        assert_eq!(s.take_due(t1), Some("flush".into()));
        assert_eq!(s.take_due(t1), None);

        // 09:00:00: cron 側だけが期限切れ(interval の次回は 09:00:20)。
        let t2 = Local.with_ymd_and_hms(2026, 7, 28, 9, 0, 0).unwrap();
        assert_eq!(s.take_due(t2), Some("daily".into()));
        assert_eq!(s.take_due(t2), None);
    }

    #[test]
    fn take_due_returns_simultaneous_schedules_in_declaration_order() {
        // 同一 interval の 2 件を用意し、同時に期限切れになるようにする。
        // take_due は 1 回の呼び出しで 1 件だけ返すため、宣言順(a → b)で
        // 決定的に返ることを確認する。
        let t0 = Local.with_ymd_and_hms(2026, 7, 28, 0, 0, 0).unwrap();
        let mut s = ScheduleState::new(
            &[
                req("a", ScheduleSpec::IntervalSeconds(60)),
                req("b", ScheduleSpec::IntervalSeconds(60)),
            ],
            t0,
        );
        let t1 = t0 + chrono::Duration::seconds(60);
        assert_eq!(s.take_due(t1), Some("a".into()));
        assert_eq!(s.take_due(t1), Some("b".into()));
        assert_eq!(s.take_due(t1), None);
    }

    #[test]
    fn next_times_reports_each_schedules_next_fire_in_declaration_order() {
        let t0 = Local.with_ymd_and_hms(2026, 7, 28, 8, 58, 0).unwrap();
        let s = ScheduleState::new(
            &[
                req("flush", ScheduleSpec::IntervalSeconds(60)),
                req("daily", ScheduleSpec::Cron("0 9 * * *".to_string())),
            ],
            t0,
        );

        let next = s.next_times();
        assert_eq!(next.len(), 2);
        assert_eq!(next[0].0, "flush");
        assert_eq!(next[0].1, t0 + chrono::Duration::seconds(60));
        assert_eq!(next[1].0, "daily");
        assert_eq!(
            next[1].1,
            Local.with_ymd_and_hms(2026, 7, 28, 9, 0, 0).unwrap()
        );
    }

    #[test]
    fn cron_gap_below_minimum_is_clamped_and_warns_once() {
        // `ScheduleSpec::Cron` は 5 欄形式(分単位)のみを保持するため、
        // 公開 API 経由では秒単位の詰まった間隔を作れない。ここでは
        // `next_cron_fire` を直接呼び、7 欄形式(秒単位)で「毎秒発火」の
        // cron を渡してクランプ経路を検証する。
        let now = Local.with_ymd_and_hms(2026, 7, 28, 0, 0, 0).unwrap();
        let schedule = cron::Schedule::from_str("* * * * * * *").expect("valid cron");
        let mut clamp_warned = false;

        let next1 = ScheduleState::next_cron_fire("tight-cron", &schedule, now, &mut clamp_warned);
        // 毎秒発火(1 秒間隔)は MIN_FIRE_INTERVAL(5 秒)未満なのでクランプされる。
        assert_eq!(
            next1,
            now + chrono::Duration::from_std(MIN_FIRE_INTERVAL).unwrap()
        );
        assert!(clamp_warned, "first clamp should mark clamp_warned");

        // Fire::Cron は clamp_warned を発火状態として保持し続けるため
        // (advance_to_future 経由で同じ &mut フラグに書き戻される)、
        // 2 回目以降の呼び出しでは同じ結果へ変わらずクランプされつつ、
        // フラグは true のまま(false に戻らない = 再度 warn しない)。
        let next2 = ScheduleState::next_cron_fire("tight-cron", &schedule, now, &mut clamp_warned);
        assert_eq!(next2, next1);
        assert!(clamp_warned);
    }
}
