CC ?= gcc
LOCAL_BIN = $(HOME)/.local/bin
SYSTEMD_DIR = $(HOME)/.config/systemd/user
# cargo 位置因发行版而异：rustup 装在 ~/.cargo/bin，
# 发行版包（Fedora/Arch dnf/pacman）装在 /usr/bin。
# 优先 rustup（通常更新），回退到 PATH，最后裸 cargo（让报错可读）。
CARGO := $(shell \
    if [ -x "$(HOME)/.cargo/bin/cargo" ]; then echo "$(HOME)/.cargo/bin/cargo"; \
    elif command -v cargo >/dev/null 2>&1; then command -v cargo; \
    else echo cargo; fi)
POLKIT_DIR = /usr/share/polkit-1/actions
POLKIT_OVERRIDE_DIR = /etc/systemd/system/polkit.service.d
POLKIT_HELPER_OVERRIDE_DIR = /etc/systemd/system/polkit-agent-helper@.service.d

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
	@# ── Binaries and scripts ──
	install -Dm755 target/release/immurok-daemon $(LOCAL_BIN)/immurok-daemon
	install -Dm755 target/release/immurok-cli $(LOCAL_BIN)/immurok-cli
	install -Dm755 target/release/imk $(LOCAL_BIN)/imk
	install -Dm755 scripts/immurok-auth-dialog $(LOCAL_BIN)/immurok-auth-dialog
	install -Dm755 scripts/immurok-pam-helper $(LOCAL_BIN)/immurok-pam-helper
	install -Dm755 scripts/ble-notify-helper.py $(LOCAL_BIN)/ble-notify-helper.py
	@# ── PAM module ──
	sudo install -Dm755 pam/pam_immurok.so $(PAM_DIR)/pam_immurok.so
	@# ── PAM service configs (add pam_immurok.so if not already present) ──
	-sudo $(LOCAL_BIN)/immurok-pam-helper add sudo 2>/dev/null
	-sudo $(LOCAL_BIN)/immurok-pam-helper add polkit-1 2>/dev/null
	-sudo $(LOCAL_BIN)/immurok-pam-helper add gdm-password 2>/dev/null
	@# ── Polkit policy + systemd overrides ──
	sed 's|@HELPER_PATH@|$(LOCAL_BIN)/immurok-pam-helper|' scripts/com.immurok.pam-helper.policy.in | sudo tee $(POLKIT_DIR)/com.immurok.pam-helper.policy > /dev/null
	@# Allow polkitd and polkit-agent-helper to access /run/user
	@# (ProtectHome=yes blocks /run/user by default)
	sudo mkdir -p $(POLKIT_OVERRIDE_DIR) $(POLKIT_HELPER_OVERRIDE_DIR)
	printf '[Service]\nBindPaths=/run/user\n' | sudo tee $(POLKIT_OVERRIDE_DIR)/immurok.conf > /dev/null
	printf '[Service]\nProtectHome=no\n' | sudo tee $(POLKIT_HELPER_OVERRIDE_DIR)/immurok.conf > /dev/null
	sudo systemctl daemon-reload
	-sudo systemctl restart polkit 2>/dev/null
	@# ── User service ──
	install -Dm644 immurok-daemon.service $(SYSTEMD_DIR)/immurok-daemon.service
	systemctl --user daemon-reload
	-systemctl --user stop immurok-daemon.service 2>/dev/null
	systemctl --user enable --now immurok-daemon.service
	@# ── User data directory ──
	mkdir -p $(HOME)/.immurok
	@# ── Ensure ~/.local/bin is on PATH (idempotent) ──
	@if echo "$$PATH" | tr ':' '\n' | grep -qx "$(LOCAL_BIN)"; then \
		echo "✓ $(LOCAL_BIN) 已在 PATH 中"; \
	else \
		for rc in $(HOME)/.bashrc $(HOME)/.zshrc; do \
			[ -f "$$rc" ] || continue; \
			if grep -q '# added by immurok install' "$$rc"; then \
				echo "✓ $$rc 已配置 PATH（跳过）"; \
			else \
				printf '\n# added by immurok install\nexport PATH="%s:$$PATH"\n' "$(LOCAL_BIN)" >> "$$rc"; \
				echo "✓ 已把 $(LOCAL_BIN) 写入 $$rc"; \
			fi; \
		done; \
		echo "⚠️  新开终端，或运行 'source ~/.bashrc'（zsh 用 ~/.zshrc）后生效"; \
	fi
	@echo "=== Done ==="

uninstall:
	@echo "=== immurok uninstall ==="
	@# ── Stop and disable service ──
	-systemctl --user disable --now immurok-daemon.service 2>/dev/null
	@# ── Remove PAM service configs ──
	-sudo $(LOCAL_BIN)/immurok-pam-helper remove sudo 2>/dev/null
	-sudo $(LOCAL_BIN)/immurok-pam-helper remove polkit-1 2>/dev/null
	-sudo $(LOCAL_BIN)/immurok-pam-helper remove gdm-password 2>/dev/null
	@# ── Remove binaries and scripts ──
	rm -f $(LOCAL_BIN)/immurok-daemon $(LOCAL_BIN)/immurok-cli $(LOCAL_BIN)/imk
	rm -f $(LOCAL_BIN)/immurok-auth-dialog $(LOCAL_BIN)/immurok-pam-helper
	rm -f $(LOCAL_BIN)/ble-notify-helper.py
	rm -f $(SYSTEMD_DIR)/immurok-daemon.service
	@# ── Remove PATH line added by install ──
	-@for rc in $(HOME)/.bashrc $(HOME)/.zshrc; do \
		[ -f "$$rc" ] || continue; \
		if grep -q '# added by immurok install' "$$rc"; then \
			sed -i '/# added by immurok install/,+1d' "$$rc"; \
			echo "✓ 已从 $$rc 移除 PATH 配置"; \
		fi; \
	done
	@# ── Remove PAM module ──
	-sudo rm -f $(PAM_DIR)/pam_immurok.so 2>/dev/null
	@# ── Remove polkit policy + systemd overrides ──
	-sudo rm -f $(POLKIT_DIR)/com.immurok.pam-helper.policy 2>/dev/null
	-sudo rm -f $(POLKIT_OVERRIDE_DIR)/immurok.conf 2>/dev/null
	-sudo rmdir $(POLKIT_OVERRIDE_DIR) 2>/dev/null
	-sudo rm -f $(POLKIT_HELPER_OVERRIDE_DIR)/immurok.conf 2>/dev/null
	-sudo rmdir $(POLKIT_HELPER_OVERRIDE_DIR) 2>/dev/null
	-sudo systemctl daemon-reload 2>/dev/null
	-sudo systemctl restart polkit 2>/dev/null
	systemctl --user daemon-reload
	@# ── Remove runtime sockets ──
	-rm -rf /run/user/$$(id -u)/immurok 2>/dev/null
	@echo "=== Done (user data in ~/.immurok preserved) ==="

clean:
	$(CARGO) clean
	$(MAKE) -C pam clean
