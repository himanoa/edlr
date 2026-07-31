///| host-settings.get-all() が返す JSON を解釈した設定値。
pub(all) struct Settings {
  enabled : Bool
  min_distance : Double
} derive(Show, Eq)

///| parse_settings は host-settings.get-all() の JSON を解釈する。
/// 壊れた JSON や未設定のキーは既定値へ倒す(プラグインを止めない)。
pub fn parse_settings(raw : String) -> Settings {
  let defaults = Settings::{ enabled: true, min_distance: 0 }
  guard (try? @json.parse(raw)) is Ok(Object(obj)) else { return defaults }
  let enabled = match obj.get("enabled") {
    Some(True) => true
    Some(False) => false
    _ => defaults.enabled
  }
  let min_distance = match obj.get("minDistance") {
    Some(Number(n, ..)) => n
    _ => defaults.min_distance
  }
  Settings::{ enabled, min_distance }
}

///| FSDJump イベントから読み取った値。
pub(all) struct Jump {
  system : String
  distance : Double
} derive(Show, Eq)

///| parse_jump は FSDJump の payload JSON を解釈する。
/// StarSystem が無い・空のときは None。
pub fn parse_jump(payload_json : String) -> Jump? {
  guard (try? @json.parse(payload_json)) is Ok(Object(obj)) else { return None }
  guard obj.get("StarSystem") is Some(String(system)) && system != "" else {
    return None
  }
  let distance = match obj.get("JumpDist") {
    Some(Number(n, ..)) => n
    _ => 0
  }
  Some(Jump::{ system, distance })
}

///| format_ly は跳躍距離を小数 2 桁で整形する(8.19 → "8.19"、3.0 → "3.00")。
pub fn format_ly(distance : Double) -> String {
  let cents = @double.round(distance * 100.0).to_int()
  let whole = cents / 100
  let frac = cents % 100
  let frac_str = if frac < 10 { "0" + frac.to_string() } else { frac.to_string() }
  "\{whole}.\{frac_str}"
}
