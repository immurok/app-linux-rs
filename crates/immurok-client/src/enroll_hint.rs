//! 登记角度引导：步骤文案 + 方向箭头。CLI 与 TUI 共享单一真相。
//! `step` 为用户下一步应按压的帧序号（1..=6），即 daemon `current + 1`。
//!
//! 固件 FP_ENROLL_CAPTURES = 6（mode-1 广覆盖）：每帧必须按压**不同**
//! 区域，连续按同一位置会被传感器以 0x28 拒绝。步骤与 macOS
//! `enrollTitleKeyForStep` / `enrollOffsetForStep` 保持一致：
//! 正中 → 左 → 右 → 指尖 → 手腕 → 回正中。

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HintId {
    Center,
    Left,
    Right,
    TowardFingertip,
    TowardWrist,
    BackToCenter,
    HoldSteady,
}

pub const fn step_hint_id(step: u8) -> HintId {
    match step {
        1 => HintId::Center,
        2 => HintId::Left,
        3 => HintId::Right,
        4 => HintId::TowardFingertip,
        5 => HintId::TowardWrist,
        6 => HintId::BackToCenter,
        _ => HintId::HoldSteady,
    }
}

pub const fn english_step_hint(hint: HintId) -> &'static str {
    match hint {
        HintId::Center => "Press finger pad on center",
        HintId::Left => "Tilt slightly left 5–10°",
        HintId::Right => "Tilt slightly right 5–10°",
        HintId::TowardFingertip => "Shift slightly toward fingertip",
        HintId::TowardWrist => "Shift slightly toward wrist",
        HintId::BackToCenter => "Back to center, press once more",
        HintId::HoldSteady => "Hold steady",
    }
}

/// 6 帧引导文案，mirrors the macOS `enroll.step.*` English strings.
pub const fn step_hint(step: u8) -> &'static str {
    english_step_hint(step_hint_id(step))
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
        let cases = [
            (1, HintId::Center, "Press finger pad on center"),
            (2, HintId::Left, "Tilt slightly left 5–10°"),
            (3, HintId::Right, "Tilt slightly right 5–10°"),
            (4, HintId::TowardFingertip, "Shift slightly toward fingertip"),
            (5, HintId::TowardWrist, "Shift slightly toward wrist"),
            (6, HintId::BackToCenter, "Back to center, press once more"),
        ];
        for (step, id, expected) in cases {
            assert_eq!(step_hint_id(step), id);
            assert_eq!(english_step_hint(id), expected);
            assert_eq!(step_hint(step), expected);
        }
        assert_eq!(step_hint_id(0), HintId::HoldSteady);
        assert_eq!(step_hint_id(7), HintId::HoldSteady);
        assert_eq!(step_hint(0), "Hold steady");
        assert_eq!(step_hint(7), "Hold steady");
    }
}
