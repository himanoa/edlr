//! `options-from` を持つ `select` 設定の候補を、ドライバの retain トピックから
//! 解決する。
//!
//! 解決は **RPC 応答を組み立てる時点**(`Registry::list` /
//! `DriverRegistry::list`)で行う。マニフェストを読み込んだ時点ではドライバが
//! まだ起動していないし、候補はサイドカーの起動などで後から変わるため、
//! 「読んだときの最新」を返すのが唯一まともに定義できる意味になる。
//!
//! プラグイン側の `[[bus]]` 宣言・承認は要求しない。ここで retained を読むのは
//! プラグインではなく**ユーザーの代理としてのデーモン**であり、返す先も設定
//! 画面だからである(設計書「承認との関係」参照)。

use edlr_driver_channel::Bus;

use crate::plugin::manifest::{SelectOption, SettingField};

/// `settings` 内の `options-from` を持つ select について、retained 値から候補を
/// 解決して `options` に埋める。
///
/// 解決できない場合(ドライバ未登録・retained 未着・JSON が壊れている)は
/// `options` を `None` のままにする。**ログは出さない**: `list()` は設定画面を
/// 開くたびに呼ばれるので、ドライバを入れていない環境では RPC のたびに同じ
/// 警告が積み上がるだけになる。原因は `options` が `null` であることと UI の
/// メッセージから追える。
pub(crate) fn resolve(settings: &mut [SettingField], bus: &Bus) {
    for setting in settings {
        let SettingField::Select {
            options,
            options_from: Some(from),
            ..
        } = setting
        else {
            continue;
        };
        *options = bus
            .retained_for(&from.driver, &from.topic)
            .as_deref()
            .and_then(parse_options);
    }
}

/// retained のペイロードを候補一覧としてパースする。
///
/// 期待する形は JSON 配列で、各要素は文字列か `{"value":..,"label":..}`
/// (`SelectOption` の `Deserialize` が両方受ける)。それ以外は `None` を返す
/// -- ドライバが載せた値の形まで edlr が矯正するのは越権で、UI に「候補を
/// 取得できません」を出させるほうが、間違った候補を見せるより正しい。
fn parse_options(payload: &[u8]) -> Option<Vec<SelectOption>> {
    serde_json::from_slice::<Vec<SelectOption>>(payload).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dynamic_select() -> SettingField {
        SettingField::Select {
            key: "speaker".into(),
            label: "話者".into(),
            default: String::new(),
            options: None,
            options_from: Some(crate::plugin::manifest::OptionsFrom {
                driver: "coeiroink".into(),
                topic: "speakers".into(),
            }),
        }
    }

    fn options_of(field: &SettingField) -> &Option<Vec<SelectOption>> {
        let SettingField::Select { options, .. } = field else {
            panic!("expected a select field");
        };
        options
    }

    /// retained をトピックへ載せる。`register_driver` を通さないと `emit` 側の
    /// 経路が使えないため、テストではバスに直接登録してから流し込む。
    fn bus_with_retained(topic: &str, payload: &[u8]) -> Bus {
        let bus = Bus::new();
        let (tx, _rx) = std::sync::mpsc::sync_channel(4);
        bus.register_driver(
            "coeiroink",
            vec![edlr_driver_channel::TopicSpec {
                name: topic.into(),
                retain: true,
                description: String::new(),
            }],
            tx,
        );
        bus.emit("coeiroink", topic, payload.to_vec())
            .expect("emit should reach the retained slot");
        bus
    }

    #[test]
    fn plain_strings_become_options_with_matching_labels() {
        let bus = bus_with_retained("speakers", br#"["Sol","Deciat"]"#);
        let mut settings = vec![dynamic_select()];

        resolve(&mut settings, &bus);

        assert_eq!(
            options_of(&settings[0]).as_deref(),
            Some(
                [
                    SelectOption {
                        value: "Sol".into(),
                        label: "Sol".into()
                    },
                    SelectOption {
                        value: "Deciat".into(),
                        label: "Deciat".into()
                    },
                ]
                .as_slice()
            )
        );
    }

    #[test]
    fn labeled_objects_keep_value_and_label_apart() {
        let bus = bus_with_retained(
            "speakers",
            r#"[{"value":"a1b2:3","label":"アメノちゃん/ギャル"}]"#.as_bytes(),
        );
        let mut settings = vec![dynamic_select()];

        resolve(&mut settings, &bus);

        assert_eq!(
            options_of(&settings[0]).as_deref(),
            Some(
                [SelectOption {
                    value: "a1b2:3".into(),
                    label: "アメノちゃん/ギャル".into()
                }]
                .as_slice()
            )
        );
    }

    #[test]
    fn an_unknown_driver_leaves_the_options_unresolved() {
        let mut settings = vec![dynamic_select()];

        resolve(&mut settings, &Bus::new());

        assert_eq!(options_of(&settings[0]), &None);
    }

    /// トピックは宣言されているが、まだ一度も `emit` されていない状態
    /// (ドライバ起動直後・サイドカー起動前)。
    #[test]
    fn a_topic_without_a_retained_value_leaves_the_options_unresolved() {
        let bus = Bus::new();
        let (tx, _rx) = std::sync::mpsc::sync_channel(4);
        bus.register_driver(
            "coeiroink",
            vec![edlr_driver_channel::TopicSpec {
                name: "speakers".into(),
                retain: true,
                description: String::new(),
            }],
            tx,
        );
        let mut settings = vec![dynamic_select()];

        resolve(&mut settings, &bus);

        assert_eq!(options_of(&settings[0]), &None);
    }

    #[test]
    fn a_malformed_payload_leaves_the_options_unresolved() {
        let bus = bus_with_retained("speakers", b"{\"not\":\"an array\"}");
        let mut settings = vec![dynamic_select()];

        resolve(&mut settings, &bus);

        assert_eq!(options_of(&settings[0]), &None);
    }

    /// 静的な候補を持つ select は触らない。
    #[test]
    fn static_options_are_left_alone() {
        let bus = bus_with_retained("speakers", br#"["from-the-bus"]"#);
        let mut settings = vec![SettingField::Select {
            key: "size".into(),
            label: "サイズ".into(),
            default: "small".into(),
            options: Some(vec![SelectOption {
                value: "small".into(),
                label: "small".into(),
            }]),
            options_from: None,
        }];

        resolve(&mut settings, &bus);

        assert_eq!(
            options_of(&settings[0]).as_deref(),
            Some(
                [SelectOption {
                    value: "small".into(),
                    label: "small".into()
                }]
                .as_slice()
            )
        );
    }
}
