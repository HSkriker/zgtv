//! zg-api 常量 —— 对齐 `modules/zgapp/config.py` 的服务层子集。

/// 方法论版本（随 zg-chan 单一真相源，M13）。`config.CZSC_METHOD_VER = 2`
pub const CZSC_METHOD_VER: i64 = zg_chan::CZSC_METHOD_VER;

/// `config.CZSC_FREQ_OF_PERIOD`
pub fn czsc_freq_of_period(period: &str) -> Option<&'static str> {
    match period {
        "day" => Some("D"),
        "week" => Some("W"),
        "month" => Some("M"),
        "min1" => Some("F1"),
        "min5" => Some("F5"),
        "min15" => Some("F15"),
        "min30" => Some("F30"),
        "min60" => Some("F60"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn freq_mapping() {
        assert_eq!(czsc_freq_of_period("day"), Some("D"));
        assert_eq!(czsc_freq_of_period("min60"), Some("F60"));
        assert_eq!(czsc_freq_of_period("bad"), None);
    }
}
