///| to_utf8 は文字列を UTF-8 のバイト列にする。
/// wit の `list<u8>`(bus の payload や HTTP の body)は UTF-8 で
/// やり取りするのが慣例で、MoonBit の String(UTF-16)とはここで変換する。
pub fn to_utf8(s : String) -> FixedArray[Byte] {
  @utf8.encode(s).to_fixedarray()
}

///| from_utf8 は UTF-8 のバイト列を文字列へ戻す。壊れた列は置換文字にする。
pub fn from_utf8(bytes : FixedArray[Byte]) -> String {
  @utf8.decode_lossy(Bytes::from_iter(bytes.iter())[:])
}
