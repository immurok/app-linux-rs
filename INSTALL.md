# Linux 安装指南

immurok Linux 端在 **Arch / Fedora 38+ / Debian 12+ (含 Ubuntu 22.04+) / KDE & GNOME** 上验证过。本文给出三套发行版的依赖与安装步骤，以及常见问题排查。

## 系统要求

- Linux 内核 5.10+
- 蓝牙适配器（BLE 4.2+）
- `systemd` user services（绝大多数主流桌面发行版默认满足）
- PAM 1.4+、polkit 0.120+
- 桌面环境（GNOME / KDE / 其它跑 GTK4 的 DE）

> ⚠️ 本项目**不**支持 musl libc 发行版（Alpine / Void musl）—— PAM 模块依赖 glibc。

## 1. 安装依赖

### Arch / Manjaro / EndeavourOS

```bash
sudo pacman -S --needed rust gcc pkgconf dbus pam bluez bluez-utils \
  gtk4 libadwaita python-gobject polkit libcanberra

# python-dbus-fast 在 AUR
yay -S python-dbus-fast
# 或不装 AUR，用 pip：
pip install --user dbus-fast
```

### Fedora 38+

```bash
sudo dnf install rust cargo gcc pkgconf-pkg-config dbus-devel pam-devel \
  bluez bluez-libs \
  gtk4 libadwaita python3-gobject \
  python3-dbus-fast polkit libcanberra-gtk3
```

### Debian 12+ / Ubuntu 22.04+

```bash
sudo apt install gcc pkg-config libdbus-1-dev libpam0g-dev bluez \
  libgtk-4-1 libadwaita-1-0 python3-gi \
  policykit-1 libcanberra-gtk-module

# Rust：apt 的版本通常太旧，建议用 rustup
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"

# dbus-fast：apt 不一定有，用 pip
pip install --user dbus-fast
# 或 Debian 12+ 强制 PEP 668，用 pipx：
sudo apt install pipx && pipx install dbus-fast
```

> Ubuntu 24.04+ apt 自带 `python3-dbus-fast`，可以省去 pip。

## 2. 构建

```bash
git clone <repo-url> imPress-v1
cd imPress-v1/app-linux-rs

make check-deps   # 先预检：缺哪个系统组件一次列清 + 给本发行版安装命令
make              # 等价 make build pam（build 会自动先跑 check-deps）
```

> `make check-deps` 检查 cargo / C 编译器 / PAM 开发头（构建必需，缺则 `make` 直接报错）
> 以及 `dbus_fast` / PyGObject+Gtk4 / bluez（运行必需，缺只 warn —— 能编译但 daemon
> 跑起来会缺 BLE / 授权弹窗功能）。`make` 已把它设成 `build` 的前置，缺包不会再拖到
> 编译或 daemon 运行到一半才以晦涩方式炸出来。

产物：
- `target/release/immurok-daemon` —— 主守护进程
- `target/release/immurok-cli` —— 配置 / 配对 CLI
- `target/release/imk` —— Agent 命令包装器
- `pam/pam_immurok.so` —— PAM 模块

> 首次 `cargo build` 会下载依赖、编译 ~200 个 crate，10–30 分钟（取决于机器）。

## 3. 安装

```bash
make install
```

这一步会做的事：

| 文件 | 路径 | 需要 sudo |
|------|------|-----------|
| `immurok-daemon` / `imk` / `immurok-cli` | `~/.local/bin/` | 否 |
| `immurok-auth-dialog` / `immurok-pam-helper` / `ble-notify-helper.py` | `~/.local/bin/` | 否 |
| `pam_immurok.so` | `/usr/lib64/security/`（Fedora）/ `/lib/x86_64-linux-gnu/security/`（Debian）/ `/usr/lib/security/`（Arch） | 是 |
| PAM service 配置 | `/etc/pam.d/sudo` / `/etc/pam.d/polkit-1` / `/etc/pam.d/gdm-password` | 是 |
| polkit policy | `/usr/share/polkit-1/actions/com.immurok.pam-helper.policy` | 是 |
| systemd polkit overrides | `/etc/systemd/system/polkit.service.d/immurok.conf` | 是 |
| systemd user service | `~/.config/systemd/user/immurok-daemon.service` | 否 |

> 没装 GDM / 没有 `/etc/pam.d/gdm-password` 是常见情况（KDE / Debian + SDDM）。Makefile 已容错，会跳过这条；如需指纹解锁登录屏，自己 `sudo immurok-pam-helper add sddm` 类似处理。

`make install` 完成后，daemon 应该已经在跑：

```bash
systemctl --user status immurok-daemon
```

## 4. 首次配置

### 4.1 与设备配对

```bash
# 开机 / 按住设备按键进入配对模式（设备 LED 慢闪蓝）
immurok-cli pair
# 在 30s 内按设备按键确认
```

### 4.2 注册指纹

```bash
immurok-cli fp enroll 0        # slot 0
# 按提示连续触摸传感器 12 次
immurok-cli fp list            # 查看已注册槽位
```

支持 5 个 slot（0–4）。删除：`immurok-cli fp delete 0`。

### 4.3 启用功能

```bash
immurok-cli set sudo true
immurok-cli set polkit true
immurok-cli set screen true        # 屏幕解锁
immurok-cli set lock true          # 长按设备按键锁屏（可选）
immurok-cli settings               # 查看所有设置
```

## 5. 验证

```bash
sudo -k && sudo whoami
# 应该弹 GTK 对话框或直接走指纹（10s 内有 cooldown 不会再弹）
```

如果走通了，触摸传感器后终端立刻出现 `katsu`，没有密码提示。

`imk run --agent` 测试：

```bash
imk run --agent -- sudo systemctl restart NetworkManager
# 会弹一次 GTK 对话框显示包裹的命令，按指纹通过
```

## 6. 故障排查

### `make install` 失败：`ERROR:NO_AUTH_LINE`

PAM 配置文件的 `auth` 行格式没识别。当前 helper 支持 `^auth` 和 `^@include` 两种写法。若你的发行版用其它（罕见），手动编辑 `/etc/pam.d/sudo` 在所有 auth 行之前加：

```
auth        sufficient    pam_immurok.so
```

### `pam_immurok.so` 找不到

PAM 模块装错目录。检查发行版的标准位置：

```bash
find /usr/lib* /lib* -name 'pam_*.so' 2>/dev/null | head -5
# 把第一个目录作为目标，cp 过去
sudo cp pam/pam_immurok.so /usr/lib64/security/   # 用上面命令找到的路径
```

### sudo 不弹指纹对话框，直接要密码

- daemon 没在跑：`systemctl --user start immurok-daemon`
- 设备没连接：`immurok-cli status`，应该看到 `connected=true verified=true`
- PAM 没加 immurok：`sudo grep pam_immurok /etc/pam.d/sudo`，没结果就 `immurok-cli pam install sudo`（这是 `sudo immurok-pam-helper add sudo` 的包装）

### polkit 弹窗不出现

```bash
# 检查 polkit override 是否生效
systemctl show polkit | grep BindPaths
# 应该看到 BindPaths=/run/user

# polkitd 跑不通常因为 ProtectHome=yes 挡了 /run/user 访问
# Makefile 已经写了 override，但需要 systemctl daemon-reload + restart
sudo systemctl daemon-reload && sudo systemctl restart polkit
```

### BLE 找不到设备

```bash
bluetoothctl scan le         # 应该列到 "immurok IK-1"
journalctl --user -u immurok-daemon | grep BLE
```

设备日志在 `~/.immurok/logs.txt`，**不**在 journal（daemon 单独写文件）。

### Debian / Ubuntu 上 `dbus-fast` 导入失败

```bash
python3 -c 'import dbus_fast'   # 应该不报错
# 报 ModuleNotFoundError：
pip install --user dbus-fast
# pipx 装的话要把脚本路径加到 daemon 用户的 PATH
```

注意 `ble-notify-helper.py` 用 `#!/usr/bin/python3`，会用系统 python（不是 venv），所以 `pip install --user` 装到 `~/.local/lib/python3.X/site-packages` 系统 python 能找到。

### Wayland 下 GTK 对话框不抢焦点

故意的 —— 对话框不抢键盘焦点免得打断你正在敲的命令。指纹通过后自动关闭。如果你想 click 不到 Cancel 按钮，焦点切到对话框窗口（Alt+Tab）。

## 7. 卸载

```bash
cd app-linux-rs
make uninstall
```

会停掉服务、移掉 PAM 配置、移掉 polkit policy + override，但**保留** `~/.immurok/`（含配对密钥、设置、日志）。

完全清除：

```bash
rm -rf ~/.immurok
```

## 不同桌面环境的额外说明

### GNOME（Fedora / Ubuntu Desktop）

开箱即用。屏幕解锁监听 `org.gnome.ScreenSaver` D-Bus 信号。

### KDE（Fedora KDE / Kubuntu）

- 装 `libadwaita`（KDE 不默认装），不然对话框起不来
- 屏幕锁监听 freedesktop `org.freedesktop.ScreenSaver`，KDE 兼容这套
- 登录屏指纹解锁要装 `sddm` 的 PAM。目前 `immurok-cli pam install` 只白名单了 `sudo/gdm-password/polkit-1`，加 sddm 需要手动编辑 `/etc/pam.d/sddm`，在第一行 auth 之前插入：`auth sufficient pam_immurok.so`

### Sway / Hyprland 等 Wlroots

GTK4 对话框能起来。屏幕锁要看你用的 lockscreen（swaylock / hyprlock），它们一般不发 D-Bus 信号，所以指纹解锁屏可能不工作 —— 走密码栈就是了。
