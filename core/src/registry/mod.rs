//! プラグイン/ドライバの registry(facade と各サービス)。
//!
//! 現状は trait 定義のみ(Phase 0)。Phase 4 で `plugin/registry.rs` と
//! `driver/registry.rs` の実装がここへ移ってくる。

use edlr_driver_process::{InstanceStatus, ProcessError, ProcessSpec};

pub(crate) mod bus;
pub mod driver;
pub(crate) mod entries;
pub(crate) mod filesystem;
pub(crate) mod grants;
pub mod plugin;
pub(crate) mod settings;
pub(crate) mod sidecar;
pub(crate) mod subject;
pub(crate) mod supervisor;

/// サイドカープロセス制御の口。実運用の実装は
/// [`edlr_driver_process::ProcessDriver`]。
///
/// `ProcessDriver::stop_detached` は `Arc<Self>` を要求するため trait には
/// 含めない(必要になった時点で `Arc` 前提のメソッドとして追加を検討)。
pub trait ProcessControl {
    fn ensure_started(
        &self,
        key: &str,
        spec: &ProcessSpec,
    ) -> Result<Vec<InstanceStatus>, ProcessError>;
    fn status(&self, key: &str, spec: &ProcessSpec) -> Vec<InstanceStatus>;
    fn stop(&self, key: &str);
    fn stop_all(&self);
}

impl ProcessControl for edlr_driver_process::ProcessDriver {
    fn ensure_started(
        &self,
        key: &str,
        spec: &ProcessSpec,
    ) -> Result<Vec<InstanceStatus>, ProcessError> {
        edlr_driver_process::ProcessDriver::ensure_started(self, key, spec)
    }
    fn status(&self, key: &str, spec: &ProcessSpec) -> Vec<InstanceStatus> {
        edlr_driver_process::ProcessDriver::status(self, key, spec)
    }
    fn stop(&self, key: &str) {
        edlr_driver_process::ProcessDriver::stop(self, key)
    }
    fn stop_all(&self) {
        edlr_driver_process::ProcessDriver::stop_all(self)
    }
}

/// プラグイン間バスへの読み取り口。実運用の実装は
/// [`edlr_driver_channel::Bus`]。
///
/// メソッドは現時点で registry 系が実際に使っている 1 本だけ
/// (`select_options::resolve` が retain 値から select 候補を解決する)。
/// spec の方針どおり、必要が実証されたときだけ増やす。
pub trait BusPort {
    fn retained_for(&self, driver_id: &str, topic: &str) -> Option<Vec<u8>>;
}

impl BusPort for edlr_driver_channel::Bus {
    fn retained_for(&self, driver_id: &str, topic: &str) -> Option<Vec<u8>> {
        edlr_driver_channel::Bus::retained_for(self, driver_id, topic)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn retained_via_trait<B: BusPort>(bus: &B, driver: &str, topic: &str) -> Option<Vec<u8>> {
        bus.retained_for(driver, topic)
    }

    #[test]
    fn bus_satisfies_bus_port() {
        let bus = edlr_driver_channel::Bus::new();
        assert_eq!(retained_via_trait(&bus, "no-such-driver", "topic"), None);
    }

    /// ProcessDriver が trait を満たすことのコンパイル時確認
    /// (実プロセスを起動しないよう、呼び出しはしない)。
    #[test]
    fn process_driver_satisfies_process_control() {
        fn assert_impl<T: ProcessControl>() {}
        assert_impl::<edlr_driver_process::ProcessDriver>();
    }
}
