///|
test "parse_settings: 既定値" {
  assert_eq(
    @jumplog.parse_settings("{}"),
    @jumplog.Settings::{ enabled: true, min_distance: 0 },
  )
}

///|
test "parse_settings: 壊れた JSON は既定値へ倒す" {
  assert_eq(
    @jumplog.parse_settings("not json"),
    @jumplog.Settings::{ enabled: true, min_distance: 0 },
  )
}

///|
test "parse_settings: 設定済みの値を読む" {
  assert_eq(
    @jumplog.parse_settings("{\"enabled\":false,\"minDistance\":20}"),
    @jumplog.Settings::{ enabled: false, min_distance: 20 },
  )
}

///|
test "parse_jump: FSDJump の payload を読む" {
  assert_eq(
    @jumplog.parse_jump("{\"StarSystem\":\"Sol\",\"JumpDist\":8.19}"),
    Some(@jumplog.Jump::{ system: "Sol", distance: 8.19 }),
  )
}

///|
test "parse_jump: StarSystem が無ければ None" {
  assert_eq(@jumplog.parse_jump("{\"JumpDist\":8.19}"), None)
  assert_eq(@jumplog.parse_jump("{\"StarSystem\":\"\"}"), None)
  assert_eq(@jumplog.parse_jump("broken"), None)
}

///|
test "format_ly: 小数 2 桁" {
  assert_eq(@jumplog.format_ly(8.19), "8.19")
  assert_eq(@jumplog.format_ly(3.0), "3.00")
  assert_eq(@jumplog.format_ly(12.345), "12.35")
}
