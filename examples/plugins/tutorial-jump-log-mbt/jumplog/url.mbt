///| edsm_url は EDSM の system API へ問い合わせる URL を組み立てる。
pub fn edsm_url(endpoint : String, system : String) -> String {
  endpoint + "?systemName=" + url_encode(system) + "&showId=1"
}

///| url_encode は文字列を UTF-8 のパーセントエンコーディングにする。
/// 星系名にはスペースが入る(`Col 285 Sector AA-A a1`)ので、URL に
/// 載せる前に必ず通すこと。
pub fn url_encode(s : String) -> String {
  let out = StringBuilder::new()
  for b in @utf8.encode(s) {
    let c = b.to_int()
    if (c >= 'A'.to_int() && c <= 'Z'.to_int()) ||
      (c >= 'a'.to_int() && c <= 'z'.to_int()) ||
      (c >= '0'.to_int() && c <= '9'.to_int()) ||
      c == '-'.to_int() ||
      c == '_'.to_int() ||
      c == '.'.to_int() ||
      c == '~'.to_int() {
      out.write_char(c.unsafe_to_char())
    } else {
      out.write_char('%')
      out.write_char(hex_digit(c / 16))
      out.write_char(hex_digit(c % 16))
    }
  }
  out.to_string()
}

///|
fn hex_digit(n : Int) -> Char {
  if n < 10 {
    ('0'.to_int() + n).unsafe_to_char()
  } else {
    ('A'.to_int() + n - 10).unsafe_to_char()
  }
}
