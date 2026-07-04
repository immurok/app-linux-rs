//! 登记角度引导：步骤文案 + 方向箭头。CLI 与 TUI 共享单一真相。
//! `step` 为用户下一步应按压的帧序号（1..=6），即 daemon `current + 1`。
//!
//! 固件 FP_ENROLL_CAPTURES = 6（mode-1 广覆盖）：每帧必须按压**不同**
//! 区域，连续按同一位置会被传感器以 0x28 拒绝。步骤与 macOS
//! `enrollTitleKeyForStep` / `enrollOffsetForStep` 保持一致：
//! 正中 → 左 → 右 → 指尖 → 手腕 → 回正中。

/// 6 帧引导文案，mirrors the macOS `enroll.step.*` English strings.
pub fn step_hint(step: u8) -> &'static str {
    match step {
        1 => "Press finger pad on center",
        2 => "Tilt slightly left 5–10°",
        3 => "Tilt slightly right 5–10°",
        4 => "Shift slightly toward fingertip",
        5 => "Shift slightly toward wrist",
        6 => "Back to center, press once more",
        _ => "Hold steady",
    }
}

/// 与 step_hint 对应的方向符号。
pub fn step_arrow(step: u8) -> &'static str {
    match step {
        2 => "←",
        3 => "→",
        4 => "↑",
        5 => "↓",
        _ => "·",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arrows_match_phases() {
        assert_eq!(step_arrow(1), "·");
        assert_eq!(step_arrow(2), "←");
        assert_eq!(step_arrow(3), "→");
        assert_eq!(step_arrow(4), "↑");
        assert_eq!(step_arrow(5), "↓");
        assert_eq!(step_arrow(6), "·");
        assert_eq!(step_arrow(99), "·");
    }

    #[test]
    fn hints_cover_boundaries() {
        assert_eq!(step_hint(1), "Press finger pad on center");
        assert_eq!(step_hint(2), "Tilt slightly left 5–10°");
        assert_eq!(step_hint(6), "Back to center, press once more");
        assert_eq!(step_hint(0), "Hold steady");
        assert_eq!(step_hint(7), "Hold steady");
    }
}
