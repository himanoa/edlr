//! 承認済みルートディレクトリ配下に限ってファイルを操作するドライバ。
//!
//! 呼び出し元(edlr のプラグインホスト)は「どのルートか」と「その配下の
//! 相対パス」だけを渡す。ルートの外へ出る経路が無いことをこのクレートが
//! 保証する。承認そのもの(誰がどのルートを使ってよいか)は呼び出し元の
//! 責務で、このクレートは grants を知らない。

pub mod path;

use std::fmt;

#[derive(Debug)]
pub enum FsError {
    /// 構文が不正、またはルート配下から出ている。
    InvalidPath(String),
    NotFound(String),
    TooLarge(String),
    Io(String),
}

impl fmt::Display for FsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FsError::InvalidPath(m) => write!(f, "invalid path: {m}"),
            FsError::NotFound(m) => write!(f, "not found: {m}"),
            FsError::TooLarge(m) => write!(f, "too large: {m}"),
            FsError::Io(m) => write!(f, "io error: {m}"),
        }
    }
}

impl std::error::Error for FsError {}
