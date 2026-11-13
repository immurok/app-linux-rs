use immurok_client::enroll_hint::HintId;
use rust_i18n::t;

pub(crate) struct Catalog;

pub(crate) const TEXT: Catalog = Catalog;

pub(crate) fn init_system_locale() {
    let values = ["LC_ALL", "LC_MESSAGES", "LANGUAGE", "LANG"].map(|name| std::env::var(name).ok());
    let borrowed = values.each_ref().map(|value| value.as_deref());
    rust_i18n::set_locale(select_locale(&borrowed));
}

fn select_locale(values: &[Option<&str>]) -> &'static str {
    let Some(value) = values
        .iter()
        .flatten()
        .find(|value| !value.trim().is_empty())
    else {
        return "en";
    };
    for candidate in value.split(':') {
        let normalized = candidate
            .split(['.', '@'])
            .next()
            .unwrap_or(candidate)
            .replace('_', "-")
            .to_ascii_lowercase();
        if normalized == "zh"
            || normalized == "zh-cn"
            || normalized == "zh-sg"
            || normalized.starts_with("zh-hans")
        {
            return "zh-CN";
        }
        if normalized == "en" || normalized.starts_with("en-") {
            return "en";
        }
    }
    "en"
}

impl Catalog {
    pub(crate) fn enrollment_title(&self, is_switch: bool) -> String {
        if is_switch {
            t!("enrollment.title.switch").to_string()
        } else {
            t!("enrollment.title.default").to_string()
        }
    }

    pub(crate) fn enrollment_gate_hint(&self) -> String {
        t!("enrollment.gate_hint").to_string()
    }

    pub(crate) fn enrollment_subtitle(&self, is_switch: bool) -> String {
        if is_switch {
            t!("enrollment.subtitle.switch").to_string()
        } else {
            t!("enrollment.subtitle.default").to_string()
        }
    }

    pub(crate) fn cancel(&self) -> String {
        t!("action.cancel").to_string()
    }

    pub(crate) fn close(&self) -> String {
        t!("action.close").to_string()
    }

    pub(crate) fn enrollment_hint(&self, hint: HintId) -> String {
        match hint {
            HintId::Center => t!("enrollment.hint.center").to_string(),
            HintId::Left => t!("enrollment.hint.left").to_string(),
            HintId::Right => t!("enrollment.hint.right").to_string(),
            HintId::TowardFingertip => t!("enrollment.hint.toward_fingertip").to_string(),
            HintId::TowardWrist => t!("enrollment.hint.toward_wrist").to_string(),
            HintId::BackToCenter => t!("enrollment.hint.back_to_center").to_string(),
            HintId::HoldSteady => t!("enrollment.hint.hold_steady").to_string(),
        }
    }

    pub(crate) fn enrollment_captured(&self, captured: u8, total: u8) -> String {
        t!("enrollment.progress", captured = captured, total = total).to_string()
    }

    pub(crate) fn enrollment_place_finger(&self) -> String {
        t!("enrollment.state.place_finger").to_string()
    }

    pub(crate) fn enrollment_lift_finger(&self) -> String {
        t!("enrollment.state.lift_finger").to_string()
    }

    pub(crate) fn enrollment_overlap(&self) -> String {
        t!("enrollment.state.overlap").to_string()
    }

    pub(crate) fn enrollment_processing(&self) -> String {
        t!("enrollment.state.processing").to_string()
    }

    pub(crate) fn enrollment_disconnected(&self) -> String {
        t!("enrollment.error.disconnected").to_string()
    }

    pub(crate) fn enrollment_failed(&self) -> String {
        t!("enrollment.error.failed").to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static LOCALE_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn selects_supported_system_locales() {
        assert_eq!(select_locale(&[Some("zh_CN.UTF-8")]), "zh-CN");
        assert_eq!(select_locale(&[Some("zh-Hans-CN")]), "zh-CN");
        assert_eq!(select_locale(&[Some("fr_FR:zh_CN")]), "zh-CN");
        assert_eq!(select_locale(&[Some("zh_TW.UTF-8")]), "en");
        assert_eq!(select_locale(&[Some("en_US.UTF-8")]), "en");
        assert_eq!(select_locale(&[Some("C.UTF-8")]), "en");
        assert_eq!(select_locale(&[]), "en");
    }

    #[test]
    fn enrollment_copy_has_english_and_simplified_chinese() {
        let _guard = LOCALE_LOCK.lock().unwrap();

        rust_i18n::set_locale("en");
        assert_eq!(TEXT.enrollment_title(false), "Add Fingerprint");
        assert_eq!(TEXT.enrollment_title(true), "Add switch fingerprint");
        assert_eq!(
            TEXT.enrollment_hint(HintId::Left),
            "Tilt slightly left 5–10°"
        );
        assert_eq!(TEXT.enrollment_captured(2, 6), "Captured (2/6)");

        rust_i18n::set_locale("zh-CN");
        assert_eq!(TEXT.enrollment_title(false), "添加指纹");
        assert_eq!(TEXT.enrollment_title(true), "添加主机切换指纹");
        assert_eq!(TEXT.enrollment_hint(HintId::Left), "向左轻微倾斜 5–10°");
        assert_eq!(TEXT.enrollment_captured(2, 6), "已采集（2/6）");

        rust_i18n::set_locale("en");
    }
}
