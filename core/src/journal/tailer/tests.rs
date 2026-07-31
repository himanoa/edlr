
use super::*;
use std::fs::OpenOptions;
use std::io::Write;

fn append(path: &std::path::Path, s: &str) {
    let mut f = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap();
    f.write_all(s.as_bytes()).unwrap();
}

/// `JournalLine` の列から `text` だけ取り出す(既存テストの比較を簡潔にする)。
fn texts(lines: Vec<JournalLine>) -> Vec<String> {
    lines.into_iter().map(|l| l.text).collect()
}

#[test]
fn reads_only_appended_complete_lines() {
    let dir = tempfile::tempdir().unwrap();
    let j = dir.path().join("Journal.2026-07-25T120000.01.log");
    append(&j, "line1\nline2\n");
    let mut t = JournalTailer::new(dir.path().to_path_buf());
    assert_eq!(texts(t.poll().unwrap()), vec!["line1", "line2"]);
    assert_eq!(texts(t.poll().unwrap()), Vec::<String>::new()); // 追記なし → 空
    append(&j, "line3\npart"); // 書きかけ行は返さない
    assert_eq!(texts(t.poll().unwrap()), vec!["line3"]);
    append(&j, "ial\n"); // 書きかけの続き
    assert_eq!(texts(t.poll().unwrap()), vec!["partial"]);
}

#[test]
fn follows_rotation_to_newer_file() {
    let dir = tempfile::tempdir().unwrap();
    let old = dir.path().join("Journal.2026-07-25T120000.01.log");
    append(&old, "old1\n");
    let mut t = JournalTailer::new(dir.path().to_path_buf());
    assert_eq!(texts(t.poll().unwrap()), vec!["old1"]);
    append(&old, "old2\n"); // 新ファイル出現と同時に旧ファイルにも追記済みのケース
    let new = dir.path().join("Journal.2026-07-25T130000.01.log");
    append(&new, "new1\n");
    assert_eq!(texts(t.poll().unwrap()), vec!["old2", "new1"]);
}

#[test]
fn restarts_from_top_on_truncation() {
    let dir = tempfile::tempdir().unwrap();
    let j = dir.path().join("Journal.2026-07-25T120000.01.log");
    append(&j, "aaaa\nbbbb\n");
    let mut t = JournalTailer::new(dir.path().to_path_buf());
    texts(t.poll().unwrap());
    std::fs::write(&j, "cc\n").unwrap(); // 短縮
    assert_eq!(texts(t.poll().unwrap()), vec!["cc"]);
}

#[test]
fn empty_dir_yields_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let mut t = JournalTailer::new(dir.path().to_path_buf());
    assert_eq!(texts(t.poll().unwrap()), Vec::<String>::new());
}

#[test]
fn follows_multiple_rotations_in_sequence() {
    let dir = tempfile::tempdir().unwrap();
    let old = dir.path().join("Journal.2026-07-25T100000.01.log");
    append(&old, "old_line1\n");
    let mut t = JournalTailer::new(dir.path().to_path_buf());
    assert_eq!(texts(t.poll().unwrap()), vec!["old_line1"]);

    // 次回のpoll前に2つの新ファイルを作成
    append(&old, "old_line2\n"); // 旧ファイルにもさらに追記
    let mid = dir.path().join("Journal.2026-07-25T110000.01.log");
    append(&mid, "mid_line1\n");
    let new = dir.path().join("Journal.2026-07-25T120000.01.log");
    append(&new, "new_line1\n");

    // 次のpollで全ファイルを順番に読む
    assert_eq!(
        texts(t.poll().unwrap()),
        vec!["old_line2", "mid_line1", "new_line1"]
    );
}

/// pos は metadata() で取得した len ではなく、実際に read_to_string で
/// 読んだバイト数だけ進めなければならない。もし len を使うと、
/// metadata() 取得後・read_to_string 実行前にファイルへ追記された分は
/// 今回の poll で読み取られてしまうにもかかわらず pos には反映されず、
/// 次回 poll で同じ行が再送出されてしまう(重複配信)。
#[test]
fn pos_advances_by_bytes_actually_read_not_stale_metadata_len() {
    let dir = tempfile::tempdir().unwrap();
    let j = dir.path().join("Journal.2026-07-25T120000.01.log");
    append(&j, "hello\n"); // 6 bytes
    let mut t = JournalTailer::new(dir.path().to_path_buf());
    let mut lines = Vec::new();
    t.read_new(&j, &mut lines).unwrap();
    assert_eq!(t.pos, 6);
    assert_eq!(texts(lines), vec!["hello"]);

    append(&j, "world\n"); // さらに6バイト追記(合計12バイト)
    let mut lines2 = Vec::new();
    t.read_new(&j, &mut lines2).unwrap();
    // pos はこれまでの pos + 今回読んだバイト数(6) = 12 になるべき。
    // len() をそのまま使う実装でも今回はたまたま一致してしまうが、
    // 「pos は読んだバイト数の積み上げ」という不変条件をここで固定する。
    assert_eq!(t.pos, 12);
    assert_eq!(texts(lines2), vec!["world"]);
}

/// ファイルが複数回のポーリングをまたいで少しずつ追記される場合でも、
/// 一度返した行が再び返される(重複配信される)ことがあってはならない。
#[test]
fn no_duplicate_lines_across_successive_polls_as_file_grows() {
    let dir = tempfile::tempdir().unwrap();
    let j = dir.path().join("Journal.2026-07-25T120000.01.log");
    append(&j, "a\n");
    let mut t = JournalTailer::new(dir.path().to_path_buf());

    let mut all = Vec::new();
    all.extend(texts(t.poll().unwrap()));
    append(&j, "b\n");
    all.extend(texts(t.poll().unwrap()));
    append(&j, "c\n");
    all.extend(texts(t.poll().unwrap()));

    assert_eq!(all, vec!["a", "b", "c"]);
    // 追記がない状態でさらに poll しても重複しない
    assert_eq!(texts(t.poll().unwrap()), Vec::<String>::new());
}

/// 現在ファイルの読み取りが失敗し続けても、ローテーション探索処理へ
/// フォールスルーして新しい Journal ファイルを発見できなければならない。
/// (現在ファイルを同名のディレクトリに置き換えることで読み取り失敗を再現する)
#[test]
fn continues_rotation_discovery_after_current_file_read_error() {
    let dir = tempfile::tempdir().unwrap();
    let old = dir.path().join("Journal.2026-07-25T100000.01.log");
    append(&old, "old1\n");
    let mut t = JournalTailer::new(dir.path().to_path_buf());
    assert_eq!(texts(t.poll().unwrap()), vec!["old1"]);

    // 現在ファイルを削除して同名のディレクトリに置き換える → 以後の読み取りは
    // 「ディレクトリを read しようとしてエラー」になる。
    std::fs::remove_file(&old).unwrap();
    std::fs::create_dir(&old).unwrap();

    let new = dir.path().join("Journal.2026-07-25T110000.01.log");
    append(&new, "new1\n");

    // 現在ファイル(ディレクトリ)の読み取りに失敗しても、poll は
    // エラーを返さずローテーション探索を継続し、新ファイルの行を返す。
    assert_eq!(texts(t.poll().unwrap()), vec!["new1"]);
}

#[test]
fn resumes_from_a_saved_position_without_re_reading() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("Journal.2026-07-27T120000.01.log");
    append(&path, "{\"a\":1}\n{\"b\":2}\n");

    let mut first = JournalTailer::resume_from(dir.path().to_path_buf(), None);
    let lines = first.poll().unwrap();
    assert_eq!(lines.len(), 2);
    let saved = first.position().expect("position after reading");

    append(&path, "{\"c\":3}\n");

    let mut second = JournalTailer::resume_from(dir.path().to_path_buf(), Some(saved));
    let lines = second.poll().unwrap();
    assert_eq!(lines.len(), 1, "must not re-read what was already consumed");
    assert!(lines[0].text.contains("\"c\""));
}

#[test]
fn the_saved_position_never_includes_a_partial_line() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("Journal.2026-07-27T120000.01.log");
    append(&path, "{\"a\":1}\n{\"partial\":");

    let mut tailer = JournalTailer::resume_from(dir.path().to_path_buf(), None);
    let lines = tailer.poll().unwrap();
    assert_eq!(lines.len(), 1);
    let saved = tailer.position().expect("position");

    // 途中で切れた行を書き足してから、保存位置で再開する。
    append(&path, "2}\n");
    let mut resumed = JournalTailer::resume_from(dir.path().to_path_buf(), Some(saved));
    let lines = resumed.poll().unwrap();

    assert_eq!(lines.len(), 1);
    assert_eq!(
        lines[0].text, "{\"partial\":2}",
        "resuming must not lose the head of a line that was incomplete"
    );
}

#[test]
fn everything_read_in_the_first_poll_is_replay_and_later_appends_are_not() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("Journal.2026-07-27T120000.01.log");
    append(&path, "{\"a\":1}\n{\"b\":2}\n");

    let mut tailer = JournalTailer::resume_from(dir.path().to_path_buf(), None);
    let first = tailer.poll().unwrap();
    assert!(
        first.iter().all(|l| l.replay),
        "pre-existing lines are replay"
    );

    append(&path, "{\"c\":3}\n");
    let second = tailer.poll().unwrap();
    assert!(
        second.iter().all(|l| !l.replay),
        "lines appended after startup are live"
    );
}

#[test]
fn resumed_catch_up_lines_are_also_replay() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("Journal.2026-07-27T120000.01.log");
    append(&path, "{\"a\":1}\n");

    let mut first = JournalTailer::resume_from(dir.path().to_path_buf(), None);
    first.poll().unwrap();
    let saved = first.position().unwrap();

    // デーモンが止まっている間に書かれたぶん。
    append(&path, "{\"b\":2}\n");

    let mut second = JournalTailer::resume_from(dir.path().to_path_buf(), Some(saved));
    let lines = second.poll().unwrap();
    assert_eq!(lines.len(), 1);
    assert!(
        lines[0].replay,
        "lines written while the daemon was down were already in the file at startup"
    );
}

#[test]
fn a_saved_offset_past_the_end_restarts_that_file_from_the_top() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("Journal.2026-07-27T120000.01.log");
    append(&path, "{\"a\":1}\n");

    let mut tailer = JournalTailer::resume_from(
        dir.path().to_path_buf(),
        Some(Position {
            file: "Journal.2026-07-27T120000.01.log".into(),
            offset: 9_999,
        }),
    );
    let lines = tailer.poll().unwrap();

    assert_eq!(
        lines.len(),
        1,
        "a truncated/replaced file is read from the top"
    );
}

#[test]
fn a_saved_file_that_no_longer_exists_resumes_at_the_next_file() {
    let dir = tempfile::tempdir().unwrap();
    let newer = dir.path().join("Journal.2026-07-27T130000.01.log");
    append(&newer, "{\"b\":2}\n");

    let mut tailer = JournalTailer::resume_from(
        dir.path().to_path_buf(),
        Some(Position {
            file: "Journal.2026-07-27T120000.01.log".into(), // 既に消えている
            offset: 10,
        }),
    );
    let lines = tailer.poll().unwrap();

    assert_eq!(lines.len(), 1);
    assert!(lines[0].text.contains("\"b\""));
    assert!(
        lines[0].replay,
        "catching up on a file that was already there at startup is replay"
    );
}

/// 現在のファイルが消えたとき、残っているのが**より古いファイルだけ**なら
/// フォールバックしてはならない。フォールバックすると古いファイルを先頭から
/// 読み直し、それ以降の全ファイルを `replay = false` で再配信してしまう。
#[test]
fn a_vanished_current_file_never_falls_back_to_an_older_one() {
    let dir = tempfile::tempdir().unwrap();
    let older = dir.path().join("Journal.2026-07-27T100000.01.log");
    let current = dir.path().join("Journal.2026-07-27T120000.01.log");
    append(&older, "{\"old\":1}\n");
    append(&current, "{\"cur\":1}\n");

    let mut t = JournalTailer::new(dir.path().to_path_buf());
    assert_eq!(texts(t.poll().unwrap()), vec!["{\"cur\":1}"]);

    std::fs::remove_file(&current).unwrap();

    assert_eq!(
        texts(t.poll().unwrap()),
        Vec::<String>::new(),
        "an older file must not be re-delivered"
    );
    assert_eq!(
        t.position().map(|p| p.file),
        Some("Journal.2026-07-27T120000.01.log".to_string()),
        "the position stays put until a strictly newer file appears"
    );

    // より新しいファイルが現れたら、そちらへ進む。
    let newer = dir.path().join("Journal.2026-07-27T130000.01.log");
    append(&newer, "{\"new\":1}\n");
    assert_eq!(texts(t.poll().unwrap()), vec!["{\"new\":1}"]);
}

/// ディレクトリを「実行のみ可(r なし)」にすると、中のファイルは開ける
/// まま `read_dir` だけが失敗する。root では権限を無視できるため注入
/// できない(その場合はテストを飛ばす)。
fn make_unlistable(dir: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o111)).unwrap();
    std::fs::read_dir(dir).is_err()
}

fn make_listable(dir: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// ローテーション走査(`read_dir`)が一時的に失敗しても、それまでに読んだ
/// 行を捨ててはならない。捨てると `self.pos` だけが進んでいるため、その行は
/// 二度と配信されない。
#[test]
fn lines_already_read_survive_a_failing_rotation_scan() {
    let dir = tempfile::tempdir().unwrap();
    let j = dir.path().join("Journal.2026-07-27T120000.01.log");
    append(&j, "line1\n");
    let mut t = JournalTailer::new(dir.path().to_path_buf());
    assert_eq!(texts(t.poll().unwrap()), vec!["line1"]);

    append(&j, "line2\n");
    if !make_unlistable(dir.path()) {
        make_listable(dir.path());
        return; // root: 権限で失敗を注入できない
    }
    let out = t.poll();
    make_listable(dir.path());

    assert_eq!(
        texts(out.expect("the lines read before the scan failed must not be dropped")),
        vec!["line2"]
    );
}

/// ローテーション先のファイルが一時的に開けない場合、それを黙って飛ばして
/// さらに次のファイルへ進んではならない(そのファイルは恒久的に失われる)。
/// あわせて、この早期 return では `caught_up` を立てない — まだ追いつけて
/// いないので、残りは次の poll で `replay` として読む。
#[test]
fn a_temporarily_unreadable_rotated_file_is_not_skipped_and_stays_replay() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("Journal.2026-07-27T100000.01.log");
    let b = dir.path().join("Journal.2026-07-27T110000.01.log");
    let c = dir.path().join("Journal.2026-07-27T120000.01.log");
    append(&a, "a1\n");
    append(&b, "b1\n");
    append(&c, "c1\n");

    std::fs::set_permissions(&b, std::fs::Permissions::from_mode(0o000)).unwrap();
    if std::fs::File::open(&b).is_ok() {
        std::fs::set_permissions(&b, std::fs::Permissions::from_mode(0o644)).unwrap();
        return; // root: 権限で失敗を注入できない
    }

    let mut t = JournalTailer::resume_from(
        dir.path().to_path_buf(),
        Some(Position {
            file: "Journal.2026-07-27T100000.01.log".into(),
            offset: 0,
        }),
    );
    let first = t.poll().unwrap();
    assert_eq!(texts(first), vec!["a1"], "must stop at the unreadable file");

    std::fs::set_permissions(&b, std::fs::Permissions::from_mode(0o644)).unwrap();
    let second = t.poll().unwrap();
    assert_eq!(
        texts(second.clone()),
        vec!["b1", "c1"],
        "the file that could not be opened must be picked up on the next poll"
    );
    assert!(
        second.iter().all(|l| l.replay),
        "these lines were written before the daemon started"
    );
}
