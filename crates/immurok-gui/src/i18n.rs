use immurok_client::enroll_hint::HintId;

pub(crate) struct Catalog;

pub(crate) const ENGLISH: Catalog = Catalog;

impl Catalog {
    pub(crate) const fn enrollment_title(&self, is_switch: bool) -> &'static str {
        if is_switch {
            "Add switch fingerprint"
        } else {
            "Add Fingerprint"
        }
    }

    pub(crate) const fn enrollment_gate_hint(&self) -> &'static str {
        "Verify with an enrolled finger"
    }

    pub(crate) const fn enrollment_subtitle(&self, is_switch: bool) -> &'static str {
        if is_switch {
            "This finger will only switch between the two hosts."
        } else {
            "Press the sensor six times, shifting slightly each time."
        }
    }

    pub(crate) const fn cancel(&self) -> &'static str {
        "Cancel"
    }

    pub(crate) const fn close(&self) -> &'static str {
        "Close"
    }

    pub(crate) const fn enrollment_hint(&self, hint: HintId) -> &'static str {
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

    pub(crate) fn enrollment_captured(&self, captured: u8, total: u8) -> String {
        format!("Captured ({captured}/{total})")
    }

    pub(crate) const fn enrollment_place_finger(&self) -> &'static str {
        "Place your finger on the sensor..."
    }

    pub(crate) const fn enrollment_lift_finger(&self) -> &'static str {
        "Lift your finger, then press again..."
    }

    pub(crate) const fn enrollment_overlap(&self) -> &'static str {
        "Too similar — shift your finger and press again"
    }

    pub(crate) const fn enrollment_processing(&self) -> &'static str {
        "Processing..."
    }

    pub(crate) const fn enrollment_disconnected(&self) -> &'static str {
        "Device disconnected before enrollment finished. Reconnect the device and try again."
    }

    pub(crate) const fn enrollment_failed(&self) -> &'static str {
        "Enrollment failed. Please try again."
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enrollment_copy_covers_both_fingerprint_kinds() {
        assert_eq!(ENGLISH.enrollment_title(false), "Add Fingerprint");
        assert_eq!(ENGLISH.enrollment_title(true), "Add switch fingerprint");
        assert_eq!(
            ENGLISH.enrollment_subtitle(false),
            "Press the sensor six times, shifting slightly each time."
        );
        assert_eq!(
            ENGLISH.enrollment_subtitle(true),
            "This finger will only switch between the two hosts."
        );
    }

    #[test]
    fn enrollment_hints_have_english_copy() {
        let cases = [
            (HintId::Center, "Press finger pad on center"),
            (HintId::Left, "Tilt slightly left 5–10°"),
            (HintId::Right, "Tilt slightly right 5–10°"),
            (HintId::TowardFingertip, "Shift slightly toward fingertip"),
            (HintId::TowardWrist, "Shift slightly toward wrist"),
            (HintId::BackToCenter, "Back to center, press once more"),
            (HintId::HoldSteady, "Hold steady"),
        ];
        for (hint, expected) in cases {
            assert_eq!(ENGLISH.enrollment_hint(hint), expected);
        }
    }

    #[test]
    fn enrollment_progress_preserves_values() {
        assert_eq!(ENGLISH.enrollment_captured(2, 6), "Captured (2/6)");
    }
}
