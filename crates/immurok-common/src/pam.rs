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

/// 厂商 PAM 目录。PAM 的查找顺序是 /etc/pam.d 优先、缺失才落到这里，所以
/// 判断一个 include 目标在不在，两个目录都得看。
pub const PAM_VENDOR_DIR: &str = "/usr/lib/pam.d";

/// immurok-pam-helper 自己创建的配置带这个标记。
const IMMUROK_MARKER: &str = "Created by immurok";

/// 我们自己写的那份配置坏没坏：include 指向的栈在 /etc/pam.d 和厂商目录里
/// 都找不到。返回第一个找不到的目标。
///
/// 为什么要单独判这个：这种配置 PAM 只报 "unable to open config"，auth 和
/// account 两条栈一起打不开 —— 指纹 Approved 了 pam_acct_mgmt() 依然失败，
/// polkit 当成认证失败反复弹框，变成死循环。0.9.1–0.9.3 在 Arch 上就是这个
/// 下场（include 了 Arch 并不存在的 password-auth）。坏配置里 pam_immurok 行
/// 是在的，[`pam_line_present_in`] 一律算「已装好」，所以必须另外判一次，
/// 否则 `immurok-cli pam repair` 会对一台正在死循环的机器说「无需修复」。
///
/// 只认带 immurok 标记的文件：用户自己维护的 /etc/pam.d/* 不该由我们论断。
pub fn broken_include_in(dir: &Path, vendor_dir: &Path, service: &str) -> Option<String> {
    let contents = std::fs::read_to_string(dir.join(service)).ok()?;
    if !contents.contains(IMMUROK_MARKER) {
        return None;
    }
    contents
        .lines()
        .filter_map(include_target)
        .find(|t| !dir.join(t).is_file() && !vendor_dir.join(t).is_file())
}

pub fn broken_include(service: &str) -> Option<String> {
    broken_include_in(Path::new("/etc/pam.d"), Path::new(PAM_VENDOR_DIR), service)
}

/// 一行配置里的 include 目标：`<type> include|substack <target>`，以及
/// Debian 的 `@include <target>`。其余行（含注释、模块行）返回 None。
fn include_target(line: &str) -> Option<String> {
    let line = line.trim();
    if line.starts_with('#') {
        return None;
    }
    let mut f = line.split_whitespace();
    match f.next()? {
        "@include" => f.next().map(str::to_string),
        "auth" | "account" | "password" | "session" => match f.next()? {
            "include" | "substack" => f.next().map(str::to_string),
            _ => None,
        },
        _ => None,
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

    fn tmpdir(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("immurok_pam_broken_{tag}_{}", std::process::id()));
        std::fs::remove_dir_all(&d).ok();
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// 0.9.1–0.9.3 在 Arch 上留下的那份配置：include 了并不存在的
    /// password-auth，但 pam_immurok 行在，所以必须被判成坏。
    #[test]
    fn detects_the_arch_loop_config() {
        let etc = tmpdir("etc");
        let vendor = tmpdir("vendor");
        std::fs::write(etc.join("system-auth"), "auth required pam_unix.so\n").unwrap();
        std::fs::write(
            etc.join("polkit-1"),
            "#%PAM-1.0\n# Created by immurok\nauth sufficient pam_immurok.so timeout=10\n\
             auth include password-auth\naccount include password-auth\n",
        )
        .unwrap();

        assert!(pam_line_present_in(&etc, "polkit-1"), "坏配置里 auth 行还在");
        assert_eq!(
            broken_include_in(&etc, &vendor, "polkit-1").as_deref(),
            Some("password-auth")
        );

        std::fs::remove_dir_all(&etc).ok();
        std::fs::remove_dir_all(&vendor).ok();
    }

    #[test]
    fn healthy_and_foreign_configs_are_not_broken() {
        let etc = tmpdir("ok_etc");
        let vendor = tmpdir("ok_vendor");
        std::fs::write(etc.join("system-auth"), "auth required pam_unix.so\n").unwrap();
        // 健康：include 目标就在 /etc/pam.d
        std::fs::write(
            etc.join("polkit-1"),
            "# Created by immurok\nauth sufficient pam_immurok.so\nauth include system-auth\n",
        )
        .unwrap();
        assert_eq!(broken_include_in(&etc, &vendor, "polkit-1"), None);

        // include 目标只在厂商目录 → PAM 查找会落到那里，不算坏
        std::fs::write(vendor.join("vendor-stack"), "auth required pam_unix.so\n").unwrap();
        std::fs::write(
            etc.join("sudo"),
            "# Created by immurok\nauth sufficient pam_immurok.so\nauth include vendor-stack\n",
        )
        .unwrap();
        assert_eq!(broken_include_in(&etc, &vendor, "sudo"), None);

        // 用户自己维护的文件（无标记）即使 include 坏了也不由我们论断
        std::fs::write(
            etc.join("login"),
            "auth sufficient pam_immurok.so\nauth include nowhere\n",
        )
        .unwrap();
        assert_eq!(broken_include_in(&etc, &vendor, "login"), None);

        // 文件不存在
        assert_eq!(broken_include_in(&etc, &vendor, "nonexistent"), None);

        std::fs::remove_dir_all(&etc).ok();
        std::fs::remove_dir_all(&vendor).ok();
    }

    #[test]
    fn parses_include_forms() {
        assert_eq!(include_target("auth include password-auth").as_deref(), Some("password-auth"));
        assert_eq!(include_target("  session   substack   common-session ").as_deref(), Some("common-session"));
        assert_eq!(include_target("@include common-auth").as_deref(), Some("common-auth"));
        assert_eq!(include_target("# auth include password-auth"), None);
        assert_eq!(include_target("auth sufficient pam_immurok.so timeout=10"), None);
        assert_eq!(include_target("-auth [success=2 default=ignore] pam_systemd_home.so"), None);
        assert_eq!(include_target(""), None);
    }
}
