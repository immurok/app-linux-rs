# 跨发行版实测 Checklist

在 Fedora / Debian / Ubuntu 上验证安装、配对、PAM 行为的端到端流程。分 6 个阶段，每步带预期输出和失败诊断。

## 准备

**目标机器**：物理机或 VM 装好 Fedora 38+ / Debian 12+ / Ubuntu 22.04+。BLE 在 VM 里要 USB passthrough，物理机最省事。

**需要的东西**：
1. immurok 设备（如果在其它机器配对过，下面会 `unpair` 重置）
2. 项目源码

```bash
git clone <repo-url> imPress-v1
cd imPress-v1/app-linux-rs
```

依赖装包参见 [INSTALL.md](INSTALL.md) 第 1 节。

## 阶段 1：装包 + 构建（10–30 分钟）

不需要设备。验证依赖装得全 + Rust 能编译。

```bash
make
```

**预期产物**：
- `target/release/immurok-daemon`
- `target/release/imk`
- `target/release/immurok-cli`
- `pam/pam_immurok.so`（约 17 KB）

**常见失败**：

| 错误 | 原因 |
|------|------|
| `pam_appl.h: No such file` | 没装 `pam-devel`（Fedora）/ `libpam0g-dev`（Debian） |
| `cannot find -lpam` | 同上 |
| `error[E0xxx]: ... requires Rust 1.7x+` | Debian apt 的 rust 太老，改用 rustup |

## 阶段 2：安装 + 路径核对（5 分钟）

```bash
make install
```

**期望关键字**：

```
sudo install -Dm755 pam/pam_immurok.so /usr/lib64/security/pam_immurok.so     # Fedora
                                       /lib/x86_64-linux-gnu/security/...     # Debian
OK:ADDED          (或 OK:ALREADY_PRESENT)    ← sudo PAM
OK:CREATED        (或 OK:ALREADY_PRESENT)    ← polkit-1
（gdm-password 可能 FILE_NOT_FOUND，正常）
```

**手动核对四点**，每点失败说明卡在哪一层：

### (1) PAM 模块装对位置

```bash
find /usr/lib* /lib* -name 'pam_immurok.so' 2>/dev/null
```

预期：路径存在，文件 17K 左右。

### (2) sudo PAM 正确插入

```bash
sudo cat /etc/pam.d/sudo
```

Debian 预期：

```
auth        sufficient    pam_immurok.so
@include common-auth
@include common-account
...
```

Fedora 预期：

```
auth        sufficient    pam_immurok.so
auth       include      system-auth
account    include      system-auth
...
```

### (3) polkit-1 PAM

```bash
sudo cat /etc/pam.d/polkit-1
```

- Debian 预期 `include common-auth`
- Fedora 预期 `include password-auth`
- 文件原本就在 → ALREADY_PRESENT，跳过

### (4) daemon 起来

```bash
systemctl --user status immurok-daemon
tail -20 ~/.immurok/logs.txt
```

预期：`active (running)` + 日志中有 `immurok-daemon starting` / `PAM socket server listening` / `SSH agent listening`。

> **Debian 关键点**：sudo PAM 文件不是上面的 `@include` 格式 → `immurok-pam-helper` 的 `@include` 兼容没修好。把当时的 `/etc/pam.d/sudo` 原始内容存档反馈。

## 阶段 3：设备配对（需要设备 + BLE）

```bash
bluetoothctl scan le &
# 应该列到 "immurok IK-1"，记下 MAC（CTRL+C 退出 scan）

immurok-cli status
# 预期：connected=false, pairing=none

# 设备之前在其它机器配过的话先工厂复位
immurok-cli unpair

# 让设备进入配对模式后：
immurok-cli pair
# 30s 内按设备按键
# 预期：Pairing OK，~/.immurok/pairing.bin 写入

immurok-cli status
# 预期：connected=true, verified=true, battery=XX, version=1.4.x
```

**常见失败**：

| 错误 | 处理 |
|------|------|
| `Device not found` | BlueZ 没扫到，`sudo systemctl restart bluetooth` 重试 |
| `dbus_fast: ModuleNotFoundError` | daemon 起不来，按 INSTALL.md 故障排查那节补装 |

## 阶段 4：注册指纹

```bash
immurok-cli fp enroll 0
# 跟着提示触摸 12 次

immurok-cli fp list
# 预期：slot 0: enrolled
```

## 阶段 5：核心功能验证

PAM + AGENT_APPROVE 行为的端到端测试。**每个测试之间间隔 > 10 秒**，避免固件 cooldown 影响判断。

```bash
immurok-cli set sudo true
```

### 5.1 普通 sudo

```bash
sudo -k                       # 清 sudo timestamp
sleep 15                      # 等 daemon pre-auth 窗口（若有）过期
sudo whoami
```

**预期**：
- 终端 spinner "Please verify your fingerprint..."
- 设备 LED 亮，按一次指纹
- 输出用户名

**失败诊断**：
- 直接弹密码框 → PAM 没装好（回阶段 2 核对）
- 设备 LED 不亮 → daemon 收不到 PAM 请求，`tail -20 ~/.immurok/logs.txt | grep AUTH`

### 5.2 `sudo -k` 撤销验证（核心）

> 背景：daemon 在 AGENT_APPROVE 通过后 arm 一个 **10s** sudo pre-auth
> 窗口（见 socket.rs handle_agent_approve），用来桥接 AGENT_APPROVE →
> sudo PAM 的延迟。所以 `sudo -k` 后必须等 **超过 10s** 才能验证撤销
> 生效，否则会被窗口放过。固件的 CMD_AUTH_REQUEST 不读 cooldown 快道
> （hidkbd.c:4564 无条件 fp_gate_enter），普通 sudo 因此每次都要指纹。

```bash
sudo -k
sleep 15                      # 必须 > 10s，等 daemon pre-auth 窗口过期
sudo whoami                   # 应该再次要按指纹
```

**预期**：`sudo whoami` 设备 LED 亮，要求按指纹。

**失败诊断**：等了 > 10s 还不要指纹 → daemon pre-auth 窗口没正确过期：

```bash
tail -30 ~/.immurok/logs.txt | grep -E "AUTH|pre-auth"
# > 10s 后的 sudo 不应该再看到 "via pre-auth"
```

> 反向验证：`sudo -k` 后 **5s 内**（< 10s）再 sudo，应该看到
> `AUTH approved via pre-auth` 直接通过——这是 pre-auth 窗口的设计行为，
> 不是 bug。

### 5.3 `imk run --agent` 包装的 sudo（一次指纹）

```bash
sleep 15                      # 等上一轮 pre-auth 窗口清空
imk run --agent -- sudo whoami
```

**预期**：
- 弹一个 GTK 对话框显示 `sudo whoami`
- 按 **一次** 指纹通过 AGENT_APPROVE
- 后续 sudo PAM 走 daemon 的 10s pre-auth 窗口，不再要指纹
- 终端输出用户名

```bash
tail -20 ~/.immurok/logs.txt | grep -E "AGENT_APPROVE|AUTH (request|approved)|pre-auth|BLE RX: \[(00|11)"
```

**应该看到**：

```
AGENT_APPROVE for command: sudo whoami
BLE RX: [11]                            ← AGENT_APPROVE 要指纹（你按这一次）
BLE RX: [00]                            ← 指纹匹配
AGENT_APPROVE approved: 10s sudo pre-auth armed
AUTH request: user=..., service=sudo
AUTH approved via pre-auth: ...         ← sudo 走 daemon pre-auth 窗口
```

**关键判断**：sudo 那条是 `AUTH approved via pre-auth`（而不是再来一轮
`BLE TX: cmd=0x33` + `BLE RX: [11]` 要你按第二次）说明 10s 窗口生效。
如果 sudo 又要你按指纹，检查 daemon 版本是否包含 10s pre-auth 改动
（handle_agent_approve 里有 `set_pre_auth(Duration::from_secs(10), ...)`）。

### 5.4 polkit 弹窗（如桌面支持）

```bash
immurok-cli set polkit true
sleep 12

# Fedora GNOME：
pkexec ls /root
# KDE：通过系统设置触发权限提升
```

**预期**：弹 polkit auth 对话框，按指纹通过。

## 阶段 6：卸载验证（可选）

```bash
make uninstall

sudo grep pam_immurok /etc/pam.d/sudo
# 预期：无输出

systemctl --user status immurok-daemon
# 预期：inactive (dead) 或 not-found

ls /usr/lib*/security/pam_immurok.so 2>/dev/null
# 预期：无
```

`~/.immurok/`（含配对、设置、日志）默认保留，完全清除：

```bash
rm -rf ~/.immurok
```

## 失败反馈格式

实测时遇到异常，把这三段贴回来就够定位：

```bash
# 1) 装包 / 构建 / install 出的错
make install 2>&1 | tail -30

# 2) PAM 文件实际状态
sudo cat /etc/pam.d/sudo
sudo cat /etc/pam.d/polkit-1

# 3) daemon 日志
tail -50 ~/.immurok/logs.txt
```

## 各发行版重点关注

| 发行版 | 重点验证 |
|--------|----------|
| **Fedora 38+** | 阶段 2 PAM 模块装在 `/usr/lib64/security/`、polkit-1 模板用 `password-auth` |
| **Debian 12+** | 阶段 2 `@include common-auth` 解析、polkit-1 模板用 `common-auth`、阶段 1 dbus-fast 装包路径 |
| **Ubuntu 22.04+** | 同 Debian。Ubuntu 24.04+ 可省 pip 装 dbus-fast |
| **KDE / SDDM** | libadwaita 装了没；登录屏要手动加 `/etc/pam.d/sddm`（CLI 暂不支持） |
| **GNOME Wayland** | 阶段 5.4 polkit 弹窗在 GNOME 上行为对照 |

## 本次改动的核心命中点

测试时这三点是本轮修改要验证的，跑出符合预期 = 改动成功：

1. **阶段 5.2**：`sudo -k` 后等 **> 10s** 再 `sudo whoami` 重新要指纹（pre-auth 窗口从 5min 收紧到 10s，撤销盲区只剩 10s）
2. **阶段 5.3**：`imk run --agent -- sudo X` 只按 1 次指纹（AGENT_APPROVE 后 daemon arm 10s sudo pre-auth，wrapped sudo 走 `AUTH approved via pre-auth`）
3. **阶段 2 Debian 特化**：`/etc/pam.d/sudo` 的 `@include common-auth` 前正确插入 `auth sufficient pam_immurok.so`
