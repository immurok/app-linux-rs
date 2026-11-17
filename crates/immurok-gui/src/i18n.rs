use immurok_client::enroll_hint::HintId;
use immurok_client::status::{ConnParams, SettingKey};
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

    pub(crate) fn nav_device(&self) -> String {
        t!("nav.device").to_string()
    }

    pub(crate) fn nav_features(&self) -> String {
        t!("nav.features").to_string()
    }

    pub(crate) fn nav_keys(&self) -> String {
        t!("nav.keys").to_string()
    }

    pub(crate) fn nav_fingerprints(&self) -> String {
        t!("nav.fingerprints").to_string()
    }

    pub(crate) fn nav_pam(&self) -> String {
        t!("nav.pam").to_string()
    }

    pub(crate) fn nav_logs(&self) -> String {
        t!("nav.logs").to_string()
    }

    pub(crate) fn menu_setup(&self) -> String {
        t!("menu.setup").to_string()
    }

    pub(crate) fn menu_quit(&self) -> String {
        t!("menu.quit").to_string()
    }

    pub(crate) fn menu_tooltip(&self) -> String {
        t!("menu.tooltip").to_string()
    }

    pub(crate) fn firmware_update_wait(&self) -> String {
        t!("firmware.update_wait").to_string()
    }

    pub(crate) fn feature_title(&self) -> String {
        t!("feature.title").to_string()
    }

    pub(crate) fn feature_description(&self) -> String {
        t!("feature.description").to_string()
    }

    pub(crate) fn feature_rows(&self) -> Vec<(SettingKey, String, String)> {
        vec![
            (
                SettingKey::UnlockSudo,
                t!("feature.sudo.title").to_string(),
                t!("feature.sudo.subtitle").to_string(),
            ),
            (
                SettingKey::UnlockPolkit,
                t!("feature.polkit.title").to_string(),
                t!("feature.polkit.subtitle").to_string(),
            ),
            (
                SettingKey::UnlockScreen,
                t!("feature.screen.title").to_string(),
                t!("feature.screen.subtitle").to_string(),
            ),
            (
                SettingKey::LockScreen,
                t!("feature.lock.title").to_string(),
                t!("feature.lock.subtitle").to_string(),
            ),
            (
                SettingKey::SshTakeover,
                t!("feature.ssh.title").to_string(),
                t!("feature.ssh.subtitle").to_string(),
            ),
        ]
    }

    pub(crate) fn feature_save_failed(&self, error: &str) -> String {
        t!("feature.save_failed", error = error).to_string()
    }

    pub(crate) fn sidebar_connecting(&self) -> String {
        t!("sidebar.connecting").to_string()
    }

    pub(crate) fn sidebar_bluetooth_connected(&self) -> String {
        t!("sidebar.bluetooth.connected").to_string()
    }

    pub(crate) fn sidebar_bluetooth_starting(&self) -> String {
        t!("sidebar.bluetooth.starting").to_string()
    }

    pub(crate) fn sidebar_bluetooth_disconnected(&self) -> String {
        t!("sidebar.bluetooth.disconnected").to_string()
    }

    pub(crate) fn sidebar_link_no_longer(&self) -> String {
        t!("sidebar.link.no_longer").to_string()
    }

    pub(crate) fn sidebar_linked(&self) -> String {
        t!("sidebar.link.linked").to_string()
    }

    pub(crate) fn sidebar_not_linked(&self) -> String {
        t!("sidebar.link.not_linked").to_string()
    }

    pub(crate) fn sidebar_shown_once(&self) -> String {
        t!("sidebar.link.shown_once").to_string()
    }

    pub(crate) fn sidebar_tooltip_unpaired(&self) -> String {
        t!("sidebar.tooltip.unpaired").to_string()
    }

    pub(crate) fn sidebar_tooltip_bluetooth_only(&self) -> String {
        t!("sidebar.tooltip.bluetooth_only").to_string()
    }

    pub(crate) fn sidebar_daemon_unavailable(&self) -> String {
        t!("sidebar.daemon_unavailable").to_string()
    }

    pub(crate) fn sidebar_show_params(&self) -> String {
        t!("sidebar.show_params").to_string()
    }

    pub(crate) fn sidebar_reading_params(&self) -> String {
        t!("sidebar.reading_params").to_string()
    }

    pub(crate) fn sidebar_read_params_failed(&self, error: &str) -> String {
        t!("sidebar.read_params_failed", error = error).to_string()
    }

    pub(crate) fn sidebar_read_params_unavailable(&self) -> String {
        t!("sidebar.read_params_unavailable").to_string()
    }

    pub(crate) fn sidebar_firmware(&self, version: &str) -> String {
        t!("sidebar.firmware", version = version).to_string()
    }

    pub(crate) fn conn_params_markup(&self, p: &ConnParams) -> String {
        let verdict = if p.ecc_ok() {
            t!("sidebar.connection.long_enough").to_string()
        } else {
            t!(
                "sidebar.connection.too_short",
                minimum = ConnParams::ECC_MIN_TIMEOUT_MS,
                requested = 6000
            )
            .to_string()
        };
        t!(
            "sidebar.connection.markup",
            title = t!("sidebar.connection.title"),
            interval = t!("sidebar.connection.interval", value = p.interval_ms()),
            latency = t!(
                "sidebar.connection.latency",
                value = p.latency,
                effective = p.effective_interval_ms()
            ),
            timeout = t!("sidebar.connection.timeout", value = p.timeout_ms()),
            verdict = verdict
        )
        .to_string()
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
