///|
test "url_encode: 予約文字をエンコードする" {
  assert_eq(
    @jumplog.url_encode("Col 285 Sector AA-A a1"),
    "Col%20285%20Sector%20AA-A%20a1",
  )
  assert_eq(@jumplog.url_encode("Sol"), "Sol")
}

///|
test "url_encode: 非 ASCII は UTF-8 でエンコードする" {
  assert_eq(@jumplog.url_encode("é"), "%C3%A9")
}

///|
test "edsm_url" {
  assert_eq(
    @jumplog.edsm_url("https://example.test/api", "Shinrarta Dezhra"),
    "https://example.test/api?systemName=Shinrarta%20Dezhra&showId=1",
  )
}
