CC ?= gcc
# 二进制进 /usr/local/bin（root 所有）：daemon 以专用系统用户运行，用户
# 可写的 ~/.local/bin 里的二进制随时能被替换，那样特权分离就白做了。
# 同样的道理适用于 pkexec 的 exec.path —— 它以前指向 ~/.local/bin。
PREFIX ?= /usr/local
BIN_DIR = $(PREFIX)/bin
SYSTEMD_SYSTEM_DIR = /etc/systemd/system
TMPFILES_DIR = /etc/tmpfiles.d
DBUS_POLICY_DIR = /etc/dbus-1/system.d
POLKIT_RULES_DIR = /etc/polkit-1/rules.d
# 旧版（用户级 daemon）留下的东西，装/卸时都要清理
LEGACY_BIN = $(HOME)/.local/bin
LEGACY_USER_UNIT_DIR = $(HOME)/.config/systemd/user
# cargo 位置因发行版而异：rustup 装在 ~/.cargo/bin，
# 发行版包（Fedora/Arch dnf/pacman）装在 /usr/bin。
# 优先 rustup（通常更新），回退到 PATH，最后裸 cargo（让报错可读）。
CARGO := $(shell \
    if [ -x "$(HOME)/.cargo/bin/cargo" ]; then echo "$(HOME)/.cargo/bin/cargo"; \
    elif command -v cargo >/dev/null 2>&1; then command -v cargo; \
    else echo cargo; fi)
POLKIT_DIR = /usr/share/polkit-1/actions
# 旧版给 polkit 开的两个 override（socket 还在 /run/user/<uid> 的年代，
# ProtectHome=yes 会把它整个挡掉）。socket 搬到 /run/immurok 后不再需要，
# 由 immurok-pam-helper migrate-daemon 负责删除。
LEGACY_POLKIT_OVERRIDE_DIR = /etc/systemd/system/polkit.service.d
LEGACY_POLKIT_HELPER_OVERRIDE_DIR = /etc/systemd/system/polkit-agent-helper@.service.d

PAM_DIR := $(shell \
    if [ -d /usr/lib64/security ]; then echo /usr/lib64/security; \
    elif [ -d /lib/aarch64-linux-gnu/security ]; then echo /lib/aarch64-linux-gnu/security; \
    elif [ -d /lib/x86_64-linux-gnu/security ]; then echo /lib/x86_64-linux-gnu/security; \
    elif [ -d /lib/security ]; then echo /lib/security; \
    else echo /usr/lib/security; fi)

.PHONY: all build pam install uninstall clean check-deps

all: build pam

# 依赖预检：缺的系统组件一次列清 + 给当前发行版的安装命令，
# 不用等 cargo/编译/daemon 跑到一半才以晦涩方式炸出来。
# 构建项（cargo/cc/PAM 头）缺则 fail；运行项（dbus_fast/Gtk/bluez）缺只 warn。
check-deps:
	@CARGO="$(CARGO)" CC="$(CC)" bash scripts/check-deps.sh all

build: check-deps
	$(CARGO) build --release --workspace

pam:
	$(MAKE) -C pam

install: all
	@echo "=== immurok install ==="
	@# root 步骤全部收敛到一次 sudo（原因见 scripts/install-root.sh 顶部注释）
	sudo bash scripts/install-root.sh $$(id -un) "$$(pwd)" $(PAM_DIR) $(BIN_DIR)
	@# ── 用户级：启动会话代理（弹窗/通知/~/.ssh/config 都归它）──
	-rm -f $(LEGACY_USER_UNIT_DIR)/immurok-daemon.service
	systemctl --user daemon-reload
	systemctl --user enable --now immurok-session-agent.service
	-rm -f $(LEGACY_BIN)/immurok-daemon $(LEGACY_BIN)/immurok-cli $(LEGACY_BIN)/imk
	-rm -f $(LEGACY_BIN)/immurok-auth-dialog $(LEGACY_BIN)/immurok-pam-helper
	-rm -f $(LEGACY_BIN)/ble-notify-helper.py
	-@for rc in $(HOME)/.bashrc $(HOME)/.zshrc; do \
		[ -f "$$rc" ] || continue; \
		if grep -q '# added by immurok install' "$$rc"; then \
			sed -i '/# added by immurok install/,+1d' "$$rc"; \
			echo "✓ 已从 $$rc 移除旧的 PATH 配置（二进制已在 $(BIN_DIR)）"; \
		fi; \
	done
	@echo ""
	@echo "=== Done ==="
	@systemctl is-active --quiet immurok-daemon && echo "✓ immurok-daemon 运行中（用户 immurok）" || echo "⚠️  immurok-daemon 未运行，看 'systemctl status immurok-daemon'"
	@echo "  日志: journalctl -u immurok-daemon  或  /var/log/immurok/daemon.log"

# 卸载。默认保留 /var/lib/immurok（配对数据）；PURGE=1 才连它一起删。
uninstall:
	@echo "=== immurok uninstall ==="
	sudo bash scripts/uninstall-root.sh $(PAM_DIR) $(BIN_DIR) $(if $(PURGE),--purge-state,--keep-state)
	@# ── 用户级 ──
	-systemctl --user disable --now immurok-session-agent.service 2>/dev/null
	-systemctl --user disable --now immurok-daemon.service 2>/dev/null
	-rm -f $(LEGACY_USER_UNIT_DIR)/immurok-daemon.service
	-systemctl --user daemon-reload 2>/dev/null
	-rm -f $(LEGACY_BIN)/immurok-daemon $(LEGACY_BIN)/immurok-cli $(LEGACY_BIN)/imk
	-rm -f $(LEGACY_BIN)/immurok-auth-dialog $(LEGACY_BIN)/immurok-pam-helper
	-rm -f $(LEGACY_BIN)/ble-notify-helper.py
	-@for rc in $(HOME)/.bashrc $(HOME)/.zshrc; do \
		[ -f "$$rc" ] || continue; \
		if grep -q '# added by immurok install' "$$rc"; then \
			sed -i '/# added by immurok install/,+1d' "$$rc"; \
			echo "✓ 已从 $$rc 移除 PATH 配置"; \
		fi; \
	done
	@echo "=== Done$(if $(PURGE), (含 /var/lib/immurok), （/var/lib/immurok 保留，PURGE=1 可一并删除）) ==="

clean:
	$(CARGO) clean
	$(MAKE) -C pam clean
