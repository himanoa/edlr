//! HostCtx と Registry が共有するランタイムバッファの JSON 形式と取りこぼしカウンタ。
//! 文字列整形と Atomic カウンタのみで I/O・Mutex・スレッドを持たない純粋モジュール。

pub mod bus;
pub mod dropped;
pub mod fs;
pub mod sidecar;
