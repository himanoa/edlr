`bus-error` を関数を持たない型だけの interface に切り出したもの。
`bus` や `bus-host` のように関数を持つ interface から `use` で型を借りると、
その interface 全体が import 先の world に引き込まれてしまう
(WIT の `use` は型だけでなく interface 全体への依存を生む)。
`world driver` が `bus-host` を import しつつ `bus` を引き込まないためには、
両者が共有する型をこの関数なし interface から借りる必要がある。