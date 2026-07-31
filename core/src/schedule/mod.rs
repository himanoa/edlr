//! プラグインスケジュールの「次回発火時刻」計算(純粋モジュール)。
//!
//! この型自身は壁時計を読まない。すべての「現在時刻」は引数として
//! `chrono::DateTime<chrono::Local>` で渡される(テストで時刻を固定するため)。
//!
//! ## 壁時計 vs 単調時計
//!
//! 設計(`docs/superpowers/specs/2026-07-28-plugin-scheduler-design.md`)どおり、
//! **interval は単調時計(`Instant`)、cron は壁時計(`chrono::Local`)** で
//! 追跡する。それぞれ意味が違うため:
//!
//! - `interval-seconds = 60` は「前回から 60 秒後」という**経過時間**の宣言
//!   であり、時計の付け替えとは無関係であるべき。壁時計で追うと、NTP の
//!   後方ステップやサスペンド/レジュームでステップ量ぶん発火が遅延し、
//!   前方ステップでは発火が早まって合体してしまう
//! - `cron = "0 9 * * *"` は「毎朝 9 時」という**定刻**の宣言なので、
//!   壁時計が正。時計が直れば定刻も追従してよい
//!
//! そのため現在時刻は `Clock`(壁時計 + 単調時計のペア)で受け取る。
//! この型自身はどちらの時計も読まない(テストで時刻を固定するため)。
//! `plugins/list` に載せる `next`(ISO8601 の壁時計時刻)は、`Clock` を
//! 基準に単調時刻を壁時計へ変換して組み立てる。

pub mod store;

use std::collections::BTreeMap;
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::{DateTime, Local};

use crate::manifest::{normalize_cron, ScheduleRequest, ScheduleSpec};

/// 「いまが何時か」を壁時計と単調時計の両方で表したもの。
///
/// interval は `mono`、cron は `wall` を基準に評価する。両者は同一時点を
/// 指している必要があるため、必ずペアで取得する(`Clock::now`)。
#[derive(Debug, Clone, Copy)]
pub(crate) struct Clock {
    pub wall: DateTime<Local>,
    pub mono: Instant,
}

impl Clock {
    /// 実時計を読む。`ScheduleState` の外側(`runner.rs` のループや
    /// `registry.rs` の RPC)でのみ呼ばれる。
    pub fn now() -> Clock {
        Clock {
            wall: Local::now(),
            mono: Instant::now(),
        }
    }

    /// 単調時刻を、この `Clock` を基準に壁時計へ変換する(表示用)。
    fn wall_at(&self, mono: Instant) -> DateTime<Local> {
        let delta = mono.saturating_duration_since(self.mono);
        self.wall + chrono::Duration::from_std(delta).unwrap_or_else(|_| chrono::Duration::zero())
    }
}

/// 実効最小発火間隔。これより短い interval や cron の間隔はここまで
/// クランプされる(5 秒未満の連射を防ぐ)。
pub(crate) const MIN_FIRE_INTERVAL: Duration = Duration::from_secs(5);

/// 1 スケジュールぶんの発火計算方法。
#[derive(Debug, Clone)]
enum Fire {
    /// 固定間隔(`MIN_FIRE_INTERVAL` 未満は丸め済み)。単調時計で追う。
    Interval(Duration),
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

/// 次回発火時刻。どちらの時計で追っているかで表現が分かれる。
#[derive(Debug, Clone, Copy)]
enum NextFire {
    /// interval — 単調時計基準。
    Mono(Instant),
    /// cron — 壁時計基準。
    Wall(DateTime<Local>),
}

impl NextFire {
    /// 発火までの残り時間。既に過ぎていれば `Duration::ZERO`。
    fn remaining(&self, clock: &Clock) -> Duration {
        match self {
            NextFire::Mono(at) => at.saturating_duration_since(clock.mono),
            NextFire::Wall(at) => (*at - clock.wall).to_std().unwrap_or(Duration::ZERO),
        }
    }

    fn is_due(&self, clock: &Clock) -> bool {
        match self {
            NextFire::Mono(at) => *at <= clock.mono,
            NextFire::Wall(at) => *at <= clock.wall,
        }
    }

    /// 表示用の壁時計時刻。
    fn wall(&self, clock: &Clock) -> DateTime<Local> {
        match self {
            NextFire::Mono(at) => clock.wall_at(*at),
            NextFire::Wall(at) => *at,
        }
    }
}

/// プラグインスレッドが所有する `ScheduleState` の「次回発火時刻」を、
/// 他スレッド(`plugins/list` の RPC)から読める形で公開する窓口。
///
/// `ScheduleState` 自身はプラグイン専用スレッドの外に出さない(`take_due` が
/// 状態を進める可変操作であり、スレッドをまたいで共有すると発火のタイミングと
/// RPC が競合する)。代わりに、ランナーループが自分の状態を更新するたびに
/// ここへ壁時計へ変換済みのスナップショットを書き込む。読み手はロックを
/// 取ってコピーするだけで、発火ロジックには一切触れない。
#[derive(Clone, Default)]
pub(crate) struct ScheduleView {
    inner: Arc<Mutex<ScheduleSnapshot>>,
}

/// `ScheduleView` が公開する `(スケジュール名, 次回発火時刻)` の一覧(宣言順)。
type ScheduleSnapshot = Vec<(String, DateTime<Local>)>;

impl ScheduleView {
    fn lock(&self) -> std::sync::MutexGuard<'_, ScheduleSnapshot> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// ランナーループが自分の状態を公開する。発火の直後と、待ちに入る前に
    /// 呼ぶ(`runner.rs` 参照)。
    pub(crate) fn publish(&self, state: &ScheduleState, clock: Clock) {
        let snapshot = state
            .next_times(clock)
            .into_iter()
            .map(|(name, next)| (name.to_string(), next))
            .collect();
        *self.lock() = snapshot;
    }

    /// 公開済みのスナップショットを返す(宣言順)。まだ一度も `publish` されて
    /// いなければ空。
    pub(crate) fn snapshot(&self) -> ScheduleSnapshot {
        self.lock().clone()
    }

    /// テスト用: ランナーループを起こさずに公開値を差し込む。
    #[cfg(test)]
    pub(crate) fn set_for_test(&self, snapshot: ScheduleSnapshot) {
        *self.lock() = snapshot;
    }
}

/// 1 スケジュールの実行時状態。
#[derive(Debug, Clone)]
struct Entry {
    name: String,
    fire: Fire,
    next: NextFire,
    /// `catch-up = true` が宣言されているか。発火するたびに最終発火時刻を
    /// 永続化する必要があるかの判定に使う(`ScheduleState::is_catch_up`)。
    catch_up: bool,
}

/// 1 プラグインの全スケジュールの発火状態。
///
/// 時計は `Clock` として引数で受け取り、この型自身は時計を読まない
/// (テストのため)。
pub(crate) struct ScheduleState {
    entries: Vec<Entry>,
}

impl ScheduleState {
    /// manifest から構築する。下限未満の interval は 5 秒へ丸めて
    /// warn ログを出した状態にする。
    ///
    /// 打ち漏らしの追い掛け実行は行わない(`new_with_catch_up` を使うこと)。
    pub fn new(schedules: &[ScheduleRequest], clock: Clock) -> Self {
        Self::new_with_catch_up(schedules, clock, &BTreeMap::new())
    }

    /// `new` に加えて、`catch-up = true` なスケジュールの**打ち漏らし**
    /// (デーモンが動いていなかった間に過ぎた定刻)を判定する。
    ///
    /// `last_fires` は永続化された最終発火時刻(`ScheduleStore`)。ある
    /// `catch-up` スケジュールについて、**いま以前の直近の定刻**が記録済みの
    /// 最終発火より後なら、打ち漏らしがあったとみなして直ちに 1 回発火させる
    /// (`next` を現在時刻に置く)。何回打ち漏らしていても 1 回に集約する
    /// -- 起動時に日次レポートが何通も飛ぶのは誰も望まない。
    ///
    /// 記録が無い場合(初回起動、ファイル破損)は追い掛けない。「起動しただけで
    /// 過去の定刻が 1 回走る」より「1 回取りこぼす」方が害が小さいため。
    pub fn new_with_catch_up(
        schedules: &[ScheduleRequest],
        clock: Clock,
        last_fires: &BTreeMap<String, DateTime<Local>>,
    ) -> Self {
        let entries = schedules
            .iter()
            .map(|req| {
                let mut entry = Self::build_entry(req, clock);
                if req.catch_up {
                    if let Some(last) = last_fires.get(&req.name) {
                        Self::apply_catch_up(&mut entry, req, *last, clock);
                    }
                }
                entry
            })
            .collect();
        ScheduleState { entries }
    }

    /// 打ち漏らしがあれば `next` を「いま」に倒す(= 次の評価で直ちに発火)。
    fn apply_catch_up(
        entry: &mut Entry,
        req: &ScheduleRequest,
        last_fire: DateTime<Local>,
        clock: Clock,
    ) {
        let Fire::Cron { schedule, .. } = &entry.fire else {
            // `catch-up` は cron 専用(manifest のパース時点で拒否済み)。
            return;
        };
        // 記録済みの最終発火の直後から数えて、いま以前に過ぎた定刻があるか。
        let missed = schedule
            .after(&last_fire)
            .take_while(|instant| *instant <= clock.wall)
            .count();
        if missed == 0 {
            return;
        }
        tracing::info!(
            schedule = %req.name,
            missed,
            last_fire = %last_fire.to_rfc3339(),
            "catching up a schedule missed while the daemon was down (coalesced to one fire)"
        );
        entry.next = NextFire::Wall(clock.wall);
    }

    fn build_entry(req: &ScheduleRequest, clock: Clock) -> Entry {
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
                Entry {
                    name: req.name.clone(),
                    next: NextFire::Mono(clock.mono + effective),
                    fire: Fire::Interval(effective),
                    catch_up: req.catch_up,
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
                let next =
                    Self::next_cron_fire(&req.name, &schedule, clock.wall, &mut clamp_warned);
                Entry {
                    name: req.name.clone(),
                    fire: Fire::Cron {
                        schedule: Box::new(schedule),
                        clamp_warned,
                    },
                    next: NextFire::Wall(next),
                    catch_up: req.catch_up,
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
    /// 組み立てるための純粋な読み取り専用アクセサ。この型自身は時計を
    /// 読まないので、呼び出し側が `Clock` を渡す(`until_next`/`take_due` と
    /// 同じ流儀)。interval の単調時刻はここで壁時計へ変換される。
    pub(crate) fn next_times(&self, clock: Clock) -> Vec<(&str, DateTime<Local>)> {
        self.entries
            .iter()
            .map(|e| (e.name.as_str(), e.next.wall(&clock)))
            .collect()
    }

    /// `name` のスケジュールが `catch-up = true` を宣言しているか。
    /// ランナーは発火のたびにこれを見て、最終発火時刻を永続化するか決める
    /// (`catch-up` でないスケジュールのためにディスクへ書く理由は無い)。
    pub(crate) fn is_catch_up(&self, name: &str) -> bool {
        self.entries
            .iter()
            .any(|entry| entry.name == name && entry.catch_up)
    }

    /// 次の発火までの残り時間(スケジュールが無ければ None)。
    pub fn until_next(&self, clock: Clock) -> Option<Duration> {
        self.entries.iter().map(|e| e.next.remaining(&clock)).min()
    }

    /// 期限が来ているスケジュール名を最大 1 つ返し、その次回時刻を
    /// 「未来の直近」まで進める。
    ///
    /// 複数のスケジュールが同時に期限切れでも 1 回の呼び出しでは 1 件だけ
    /// 返す(呼び出し側がループで繰り返し呼ぶ想定)。同時に複数が期限切れの
    /// 場合の順序は宣言順で決定的。
    pub fn take_due(&mut self, clock: Clock) -> Option<String> {
        let idx = self.entries.iter().position(|e| e.next.is_due(&clock))?;
        let entry = &mut self.entries[idx];
        let name = entry.name.clone();
        entry.next = Self::advance_to_future(&name, &mut entry.fire, entry.next, clock);
        Some(name)
    }

    /// どれだけ遅れていても、`next` を「いまより未来の直近の発火時刻」まで
    /// 一気に進める(見逃した発火は 1 回に集約される)。
    fn advance_to_future(name: &str, fire: &mut Fire, next: NextFire, clock: Clock) -> NextFire {
        match (fire, next) {
            (Fire::Interval(interval), NextFire::Mono(next)) => {
                let mut next = next;
                // interval は必ず正(MIN_FIRE_INTERVAL 未満は構築時に丸め済み)。
                while next <= clock.mono {
                    next += *interval;
                }
                NextFire::Mono(next)
            }
            (
                Fire::Cron {
                    schedule,
                    clamp_warned,
                },
                _,
            ) => NextFire::Wall(Self::next_cron_fire(
                name,
                schedule,
                clock.wall,
                clamp_warned,
            )),
            // `Fire` と `NextFire` は構築時に対で決まるため到達しない。
            // 万一ずれても、次の周期で拾い直せる時刻へ倒す。
            (Fire::Interval(interval), NextFire::Wall(_)) => NextFire::Mono(clock.mono + *interval),
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
            catch_up: false,
        }
    }

    fn catch_up_req(name: &str, cron: &str) -> ScheduleRequest {
        ScheduleRequest {
            name: name.to_string(),
            spec: ScheduleSpec::Cron(cron.to_string()),
            catch_up: true,
        }
    }

    /// テスト用の固定時計。基準時刻からのオフセットで `Clock` を作る。
    ///
    /// 通常は `at()` で壁時計と単調時計を同じだけ進める(時計が正常なケース)。
    /// NTP のステップ補正やサスペンド/レジュームを再現したいときは `skewed()`
    /// で両者を別々に進める。
    struct FakeClock {
        wall0: DateTime<Local>,
        mono0: Instant,
    }

    impl FakeClock {
        fn new(wall0: DateTime<Local>) -> FakeClock {
            FakeClock {
                wall0,
                // 壁時計が後方へステップするテストでも単調時計側を引き算
                // できるよう、十分に先を基準点にしておく。
                mono0: Instant::now() + Duration::from_secs(86_400),
            }
        }

        fn at(&self, offset: chrono::Duration) -> Clock {
            self.skewed(offset, offset)
        }

        fn skewed(&self, mono_offset: chrono::Duration, wall_offset: chrono::Duration) -> Clock {
            let mono = if mono_offset >= chrono::Duration::zero() {
                self.mono0 + mono_offset.to_std().unwrap()
            } else {
                self.mono0 - (-mono_offset).to_std().unwrap()
            };
            Clock {
                wall: self.wall0 + wall_offset,
                mono,
            }
        }
    }

    fn secs(n: i64) -> chrono::Duration {
        chrono::Duration::seconds(n)
    }

    #[test]
    fn interval_fires_after_the_interval() {
        let c = FakeClock::new(Local.with_ymd_and_hms(2026, 7, 28, 0, 0, 0).unwrap());
        let t0 = c.at(secs(0));
        let mut s = ScheduleState::new(&[req("flush", ScheduleSpec::IntervalSeconds(60))], t0);
        assert_eq!(s.take_due(t0), None);
        assert_eq!(s.until_next(t0), Some(Duration::from_secs(60)));
        assert_eq!(s.take_due(c.at(secs(60))), Some("flush".into()));
    }

    #[test]
    fn missed_fires_are_coalesced_to_one() {
        let c = FakeClock::new(Local.with_ymd_and_hms(2026, 7, 28, 0, 0, 0).unwrap());
        let mut s = ScheduleState::new(
            &[req("flush", ScheduleSpec::IntervalSeconds(60))],
            c.at(secs(0)),
        );

        // 10 分経過(本来 10 回発火しているはず)。
        let t_later = c.at(chrono::Duration::minutes(10));
        assert_eq!(s.take_due(t_later), Some("flush".into()));
        // 2 回目は None(同時刻ではもう期限切れがない = 1 回に集約された)。
        assert_eq!(s.take_due(t_later), None);
        // 次回は未来(正の残り時間)。
        let remaining = s.until_next(t_later).expect("schedule should remain");
        assert!(remaining > Duration::ZERO);
    }

    #[test]
    fn interval_below_minimum_is_clamped_to_five_seconds() {
        let c = FakeClock::new(Local.with_ymd_and_hms(2026, 7, 28, 0, 0, 0).unwrap());
        let t0 = c.at(secs(0));
        let mut s = ScheduleState::new(&[req("tick", ScheduleSpec::IntervalSeconds(1))], t0);
        assert_eq!(s.until_next(t0), Some(Duration::from_secs(5)));
        assert_eq!(s.take_due(c.at(secs(4))), None);
        assert_eq!(s.take_due(c.at(secs(5))), Some("tick".into()));
    }

    /// 壁時計が後方へステップしても(NTP 補正・サスペンドからの復帰)、
    /// interval の発火は経過時間どおりに来ること。壁時計で追っていた頃は、
    /// ここでステップ量ぶん発火が遅延していた。
    #[test]
    fn interval_ignores_a_backward_wall_clock_step() {
        let c = FakeClock::new(Local.with_ymd_and_hms(2026, 7, 28, 0, 0, 0).unwrap());
        let mut s = ScheduleState::new(
            &[req("flush", ScheduleSpec::IntervalSeconds(60))],
            c.at(secs(0)),
        );

        // 単調時計は 60 秒進んだが、壁時計は 1 時間戻された。
        let stepped_back = c.skewed(secs(60), -chrono::Duration::hours(1));
        assert_eq!(s.take_due(stepped_back), Some("flush".into()));
    }

    /// 壁時計が前方へステップしても、interval の発火が早まって合体しないこと。
    #[test]
    fn interval_ignores_a_forward_wall_clock_step() {
        let c = FakeClock::new(Local.with_ymd_and_hms(2026, 7, 28, 0, 0, 0).unwrap());
        let mut s = ScheduleState::new(
            &[req("flush", ScheduleSpec::IntervalSeconds(60))],
            c.at(secs(0)),
        );

        // 単調時計は 10 秒しか進んでいないのに、壁時計は 1 時間進んだ。
        let stepped_forward = c.skewed(secs(10), chrono::Duration::hours(1));
        assert_eq!(s.take_due(stepped_forward), None);
        assert_eq!(s.until_next(stepped_forward), Some(Duration::from_secs(50)));
    }

    /// cron は逆に壁時計が正。時計が直れば定刻も追従してよい。
    #[test]
    fn cron_follows_a_forward_wall_clock_step() {
        let c = FakeClock::new(Local.with_ymd_and_hms(2026, 7, 28, 8, 59, 0).unwrap());
        let mut s = ScheduleState::new(
            &[req("daily", ScheduleSpec::Cron("0 9 * * *".to_string()))],
            c.at(secs(0)),
        );

        // 単調時計は 1 秒しか進んでいないが、壁時計は 09:00 を回った。
        let stepped = c.skewed(secs(1), secs(60));
        assert_eq!(s.take_due(stepped), Some("daily".into()));
    }

    /// デーモンが止まっていた間に過ぎた定刻を、起動直後に 1 回だけ
    /// 追い掛けること。
    #[test]
    fn a_catch_up_schedule_fires_immediately_for_a_missed_instant() {
        // いまは 7/28 の 12:00。日次 09:00 の最終発火は 7/27。
        // つまり 7/28 09:00 を打ち漏らしている。
        let c = FakeClock::new(Local.with_ymd_and_hms(2026, 7, 28, 12, 0, 0).unwrap());
        let last_fires = BTreeMap::from([(
            "daily".to_string(),
            Local.with_ymd_and_hms(2026, 7, 27, 9, 0, 0).unwrap(),
        )]);

        let mut s = ScheduleState::new_with_catch_up(
            &[catch_up_req("daily", "0 9 * * *")],
            c.at(secs(0)),
            &last_fires,
        );

        assert_eq!(s.take_due(c.at(secs(0))), Some("daily".into()));
        // 打ち漏らしが何日ぶんあっても 1 回に集約する。
        assert_eq!(s.take_due(c.at(secs(0))), None);
    }

    /// 何日ぶん打ち漏らしていても発火は 1 回。
    #[test]
    fn many_missed_instants_are_coalesced_into_one_catch_up_fire() {
        let c = FakeClock::new(Local.with_ymd_and_hms(2026, 7, 28, 12, 0, 0).unwrap());
        let last_fires = BTreeMap::from([(
            "daily".to_string(),
            // 3 ヶ月前 = 90 回以上の打ち漏らし。
            Local.with_ymd_and_hms(2026, 4, 20, 9, 0, 0).unwrap(),
        )]);

        let mut s = ScheduleState::new_with_catch_up(
            &[catch_up_req("daily", "0 9 * * *")],
            c.at(secs(0)),
            &last_fires,
        );

        assert_eq!(s.take_due(c.at(secs(0))), Some("daily".into()));
        assert_eq!(s.take_due(c.at(secs(0))), None);
    }

    /// 打ち漏らしが無ければ追い掛けない(今日ぶんは既に発火済み)。
    #[test]
    fn a_catch_up_schedule_does_not_fire_when_nothing_was_missed() {
        let c = FakeClock::new(Local.with_ymd_and_hms(2026, 7, 28, 12, 0, 0).unwrap());
        let last_fires = BTreeMap::from([(
            "daily".to_string(),
            Local.with_ymd_and_hms(2026, 7, 28, 9, 0, 0).unwrap(),
        )]);

        let mut s = ScheduleState::new_with_catch_up(
            &[catch_up_req("daily", "0 9 * * *")],
            c.at(secs(0)),
            &last_fires,
        );

        assert_eq!(s.take_due(c.at(secs(0))), None);
    }

    /// 記録が無い(初回起動・ファイル破損)場合は追い掛けない。「起動しただけで
    /// 過去の定刻が走る」より「1 回取りこぼす」方が害が小さい。
    #[test]
    fn a_catch_up_schedule_does_not_fire_without_a_recorded_last_fire() {
        let c = FakeClock::new(Local.with_ymd_and_hms(2026, 7, 28, 12, 0, 0).unwrap());

        let mut s = ScheduleState::new_with_catch_up(
            &[catch_up_req("daily", "0 9 * * *")],
            c.at(secs(0)),
            &BTreeMap::new(),
        );

        assert_eq!(s.take_due(c.at(secs(0))), None);
    }

    /// `catch-up` を宣言していないスケジュールは、記録があっても追い掛けない。
    #[test]
    fn a_schedule_without_catch_up_ignores_recorded_last_fires() {
        let c = FakeClock::new(Local.with_ymd_and_hms(2026, 7, 28, 12, 0, 0).unwrap());
        let last_fires = BTreeMap::from([(
            "daily".to_string(),
            Local.with_ymd_and_hms(2026, 7, 27, 9, 0, 0).unwrap(),
        )]);

        let mut s = ScheduleState::new_with_catch_up(
            &[req("daily", ScheduleSpec::Cron("0 9 * * *".to_string()))],
            c.at(secs(0)),
            &last_fires,
        );

        assert_eq!(s.take_due(c.at(secs(0))), None);
    }

    #[test]
    fn is_catch_up_reports_only_the_declared_schedules() {
        let c = FakeClock::new(Local.with_ymd_and_hms(2026, 7, 28, 12, 0, 0).unwrap());
        let s = ScheduleState::new(
            &[
                catch_up_req("daily", "0 9 * * *"),
                req("flush", ScheduleSpec::IntervalSeconds(60)),
            ],
            c.at(secs(0)),
        );

        assert!(s.is_catch_up("daily"));
        assert!(!s.is_catch_up("flush"));
        assert!(!s.is_catch_up("nonexistent"));
    }

    #[test]
    fn cron_fires_at_the_wall_clock_time() {
        let c = FakeClock::new(Local.with_ymd_and_hms(2026, 7, 28, 8, 59, 0).unwrap());
        let t0 = c.at(secs(0));
        let mut s = ScheduleState::new(
            &[req("daily", ScheduleSpec::Cron("0 9 * * *".to_string()))],
            t0,
        );
        assert_eq!(s.until_next(t0), Some(Duration::from_secs(60)));
        assert_eq!(s.take_due(t0), None);
        assert_eq!(s.take_due(c.at(secs(60))), Some("daily".into()));
    }

    #[test]
    fn two_schedules_fire_independently() {
        // interval は 70 秒にして、cron の分境界(09:00:00)と重ならない
        // ようにする(でないと両方が同時刻で期限切れになり得る)。
        let c = FakeClock::new(Local.with_ymd_and_hms(2026, 7, 28, 8, 58, 0).unwrap());
        let mut s = ScheduleState::new(
            &[
                req("flush", ScheduleSpec::IntervalSeconds(70)),
                req("daily", ScheduleSpec::Cron("0 9 * * *".to_string())),
            ],
            c.at(secs(0)),
        );

        // 08:59:10: interval 側だけが期限切れ。
        let t1 = c.at(secs(70));
        assert_eq!(s.take_due(t1), Some("flush".into()));
        assert_eq!(s.take_due(t1), None);

        // 09:00:00: cron 側だけが期限切れ(interval の次回は 09:00:20)。
        let t2 = c.at(secs(120));
        assert_eq!(s.take_due(t2), Some("daily".into()));
        assert_eq!(s.take_due(t2), None);
    }

    #[test]
    fn take_due_returns_simultaneous_schedules_in_declaration_order() {
        // 同一 interval の 2 件を用意し、同時に期限切れになるようにする。
        // take_due は 1 回の呼び出しで 1 件だけ返すため、宣言順(a → b)で
        // 決定的に返ることを確認する。
        let c = FakeClock::new(Local.with_ymd_and_hms(2026, 7, 28, 0, 0, 0).unwrap());
        let mut s = ScheduleState::new(
            &[
                req("a", ScheduleSpec::IntervalSeconds(60)),
                req("b", ScheduleSpec::IntervalSeconds(60)),
            ],
            c.at(secs(0)),
        );
        let t1 = c.at(secs(60));
        assert_eq!(s.take_due(t1), Some("a".into()));
        assert_eq!(s.take_due(t1), Some("b".into()));
        assert_eq!(s.take_due(t1), None);
    }

    #[test]
    fn next_times_reports_each_schedules_next_fire_in_declaration_order() {
        let wall0 = Local.with_ymd_and_hms(2026, 7, 28, 8, 58, 0).unwrap();
        let c = FakeClock::new(wall0);
        let t0 = c.at(secs(0));
        let s = ScheduleState::new(
            &[
                req("flush", ScheduleSpec::IntervalSeconds(60)),
                req("daily", ScheduleSpec::Cron("0 9 * * *".to_string())),
            ],
            t0,
        );

        // interval 側は単調時計で持っているので、表示用に壁時計へ変換される。
        let next = s.next_times(t0);
        assert_eq!(next.len(), 2);
        assert_eq!(next[0].0, "flush");
        assert_eq!(next[0].1, wall0 + secs(60));
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
