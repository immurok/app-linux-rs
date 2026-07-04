//! 共享的 PAM 配置检查（CLI / daemon / TUI 复用）。

use std::path::Path;

/// `/etc/pam.d/{service}` 是否已含 immurok 的 auth 行。文件不存在返回 false。
pub fn pam_line_present(service: &str) -> bool {
    pam_line_present_in(Path::new("/etc/pam.d"), service)
}

/// 目录可注入版本，供测试。
pub fn pam_line_present_in(dir: &Path, service: &str) -> bool {
    match std::fs::read_to_string(dir.join(service)) {
        Ok(contents) => contents.contains("pam_immurok.so"),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_present_and_absent() {
        let dir = std::env::temp_dir().join(format!("immurok_pam_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("sudo"), "auth sufficient pam_immurok.so\n").unwrap();
        std::fs::write(dir.join("polkit-1"), "auth include common-auth\n").unwrap();

        assert!(pam_line_present_in(&dir, "sudo"));
        assert!(!pam_line_present_in(&dir, "polkit-1"));
        assert!(!pam_line_present_in(&dir, "nonexistent"));

        std::fs::remove_dir_all(&dir).ok();
    }
}
