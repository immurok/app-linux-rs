# packaging/pkg-env.sh — 供 Makefile / CI source，算出 nfpm.yaml 需要的变量。
# 用法: set -a; . packaging/pkg-env.sh; set +a
#   VERSION      workspace crate 版本（以 immurok-daemon 为准，六个 crate 同号）
#   NFPM_ARCH    nfpm 的 GOARCH 风格架构名，它会自己翻成 deb amd64/arm64、rpm/arch x86_64/aarch64
#   DEB_TRIPLET  Debian 多架构目录名（PAM 模块落在 /usr/lib/<triplet>/security）
# 必须从仓库根目录 source（Makefile 与 CI 都是）：POSIX sh 里被 source 的文件拿不到
# 自己的路径，所以按 CWD 找 Cargo.toml。PKG_MACHINE 可覆盖 uname -m（测试用）。
[ -f crates/immurok-daemon/Cargo.toml ] || { echo "pkg-env.sh: source me from the repo root" >&2; return 1 2>/dev/null || exit 1; }
VERSION=$(grep -m1 '^version' crates/immurok-daemon/Cargo.toml | cut -d'"' -f2)
case "${PKG_MACHINE:-$(uname -m)}" in
    x86_64)        NFPM_ARCH=amd64; DEB_TRIPLET=x86_64-linux-gnu ;;
    aarch64|arm64) NFPM_ARCH=arm64; DEB_TRIPLET=aarch64-linux-gnu ;;
    *) echo "pkg-env.sh: unsupported machine $(uname -m)" >&2; return 1 2>/dev/null || exit 1 ;;
esac
