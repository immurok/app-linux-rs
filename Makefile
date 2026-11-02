CC ?= gcc
# Binaries go to /usr/local/bin (root-owned). The daemon runs as a dedicated
# system user, and anything under a user-writable ~/.local/bin can be swapped
# out at will — which would make the privilege separation pointless. The same
# reasoning applies to pkexec's exec.path, which used to point at ~/.local/bin.
PREFIX ?= /usr/local
BIN_DIR = $(PREFIX)/bin
SYSTEMD_SYSTEM_DIR = /etc/systemd/system
TMPFILES_DIR = /etc/tmpfiles.d
DBUS_POLICY_DIR = /etc/dbus-1/system.d
POLKIT_RULES_DIR = /etc/polkit-1/rules.d
# Leftovers from the old user-level daemon; cleaned up on both install and
# uninstall.
LEGACY_BIN = $(HOME)/.local/bin
LEGACY_USER_UNIT_DIR = $(HOME)/.config/systemd/user
# Where cargo lives varies: rustup puts it in ~/.cargo/bin, distribution
# packages (Fedora/Arch dnf/pacman) in /usr/bin. Prefer rustup (usually
# newer), fall back to PATH, then to a bare `cargo` so the error stays
# readable.
CARGO := $(shell \
    if [ -x "$(HOME)/.cargo/bin/cargo" ]; then echo "$(HOME)/.cargo/bin/cargo"; \
    elif command -v cargo >/dev/null 2>&1; then command -v cargo; \
    else echo cargo; fi)
# The GUI needs the GTK4 + libadwaita development headers. Without them, skip
# immurok-gui and build the rest anyway, so daemon/CLI users are not dragged
# into installing a graphical toolchain.
HAS_GTK_DEV := $(shell pkg-config --exists gtk4 libadwaita-1 2>/dev/null && echo 1)
CARGO_EXCLUDE := $(if $(HAS_GTK_DEV),,--exclude immurok-gui)
POLKIT_DIR = /usr/share/polkit-1/actions
# Two overrides the old layout needed for polkit (back when the socket lived
# in /run/user/<uid>, which ProtectHome=yes hides entirely). Obsolete since
# the socket moved to /run/immurok; `immurok-pam-helper migrate-daemon`
# removes them.
LEGACY_POLKIT_OVERRIDE_DIR = /etc/systemd/system/polkit.service.d
LEGACY_POLKIT_HELPER_OVERRIDE_DIR = /etc/systemd/system/polkit-agent-helper@.service.d

PAM_DIR := $(shell \
    if [ -d /usr/lib64/security ]; then echo /usr/lib64/security; \
    elif [ -d /lib/aarch64-linux-gnu/security ]; then echo /lib/aarch64-linux-gnu/security; \
    elif [ -d /lib/x86_64-linux-gnu/security ]; then echo /lib/x86_64-linux-gnu/security; \
    elif [ -d /lib/security ]; then echo /lib/security; \
    else echo /usr/lib/security; fi)

# install / uninstall must run as YOUR OWN user. They shell out to sudo only
# for the root half (scripts/install-root.sh / uninstall-root.sh); the other
# half is user-level — `systemctl --user`, GUI autostart, cleanup under
# $(HOME). Wrap the whole make in sudo and that half breaks silently:
# `systemctl --user` finds no session bus ($XDG_RUNTIME_DIR is unset) and
# $(HOME) becomes /root, so the root half installs while the user half does
# nothing. It also leaves target/ root-owned, which makes the next
# unprivileged `cargo build` fail on permissions.
# This is a parse-time $(error) rather than a check inside a recipe, so it
# fires before anything — including build — runs.
ifneq (,$(filter install uninstall,$(MAKECMDGOALS)))
ifeq (0,$(shell id -u))
$(error Do not run `make $(MAKECMDGOALS)` as root or under sudo. Run `make $(MAKECMDGOALS)` as yourself; it will prompt for a password when it needs one. See the comment above this check)
endif
endif

.PHONY: all build pam install uninstall clean check-deps package verify-pkg

all: build pam

# Dependency pre-flight: list everything missing at once, with the install
# command for the running distribution, instead of failing obscurely halfway
# through cargo / the compile / daemon startup. Missing build requirements
# (cargo/cc/PAM headers) fail; missing runtime ones (dbus_fast/Gtk/bluez)
# only warn.
check-deps:
	@CARGO="$(CARGO)" CC="$(CC)" bash scripts/check-deps.sh all

build: check-deps
	$(CARGO) build --release --workspace $(CARGO_EXCLUDE)
	@[ -n "$(HAS_GTK_DEV)" ] || echo "WARNING: gtk4/libadwaita development headers not found - skipped immurok-gui (install libgtk-4-dev libadwaita-1-dev and re-run make)"

pam:
	$(MAKE) -C pam

install: all
	@echo "=== immurok install ==="
	@# Every root step is funnelled into one sudo (see the header comment in
	@# scripts/install-root.sh for why).
	sudo bash scripts/install-root.sh $$(id -un) "$$(pwd)" $(PAM_DIR) $(BIN_DIR)
	@# -- User level: start the session agent (dialogs, notifications and
	@# ~/.ssh/config all belong to it).
	-rm -f $(LEGACY_USER_UNIT_DIR)/immurok-daemon.service
	systemctl --user daemon-reload
	@# enable + restart, NOT enable --now: `--now` is a no-op on a unit that
	@# is already running, so a reinstall left the old process alive on a
	@# binary that had been replaced (observed surviving several consecutive
	@# `make install` runs). `restart` on a stopped unit is equivalent to
	@# `start`, so a first install works the same way.
	systemctl --user enable immurok-session-agent.service
	systemctl --user restart immurok-session-agent.service
	@# -- User level: GUI autostart (resident, so the hotkey opens instantly).
	@# Skipped when the binary is absent.
	@# Retire the resident instance first, or every later `immurok-gui` is
	@# forwarded to the old binary (see scripts/retire-gui.sh).
	@if [ -x $(BIN_DIR)/immurok-gui ]; then \
		bash scripts/retire-gui.sh; \
		mkdir -p $(HOME)/.config/autostart; \
		install -m644 packaging/com.immurok.Settings.autostart.desktop $(HOME)/.config/autostart/com.immurok.Settings.desktop; \
		echo "OK: GUI autostart registered ($(HOME)/.config/autostart/com.immurok.Settings.desktop)"; \
	fi
	-rm -f $(LEGACY_BIN)/immurok-daemon $(LEGACY_BIN)/immurok-cli $(LEGACY_BIN)/imk
	-rm -f $(LEGACY_BIN)/immurok-auth-dialog $(LEGACY_BIN)/immurok-pam-helper
	-rm -f $(LEGACY_BIN)/ble-notify-helper.py
	-@for rc in $(HOME)/.bashrc $(HOME)/.zshrc; do \
		[ -f "$$rc" ] || continue; \
		if grep -q '# added by immurok install' "$$rc"; then \
			sed -i '/# added by immurok install/,+1d' "$$rc"; \
			echo "OK: removed the old PATH entry from $$rc (the binaries live in $(BIN_DIR) now)"; \
		fi; \
	done
	@echo ""
	@echo "=== Done ==="
	@systemctl is-active --quiet immurok-daemon && echo "OK: immurok-daemon is running (as user immurok)" || echo "WARNING: immurok-daemon is not running - check 'systemctl status immurok-daemon'"
	@echo "  Logs: journalctl -u immurok-daemon  or  /var/log/immurok/daemon.log"
	@echo ""
	@# Final gate: fail if any of the three long-lived processes is still on a
	@# binary that has been replaced. New code on disk with old code running is
	@# one of the most expensive classes of problem to chase — it presents as
	@# "my change had no effect" with nothing reported anywhere.
	@bash scripts/verify-fresh.sh $(BIN_DIR)

# Uninstall. Keeps /var/lib/immurok (the pairing data) by default; only
# PURGE=1 removes that too.
uninstall:
	@echo "=== immurok uninstall ==="
	@# Retire the resident GUI first: it keeps running after its binary is
	@# deleted.
	-bash scripts/retire-gui.sh
	sudo bash scripts/uninstall-root.sh $(PAM_DIR) $(BIN_DIR) $(if $(PURGE),--purge-state,--keep-state)
	@# -- User level --
	-systemctl --user disable --now immurok-session-agent.service 2>/dev/null
	-systemctl --user disable --now immurok-daemon.service 2>/dev/null
	-rm -f $(LEGACY_USER_UNIT_DIR)/immurok-daemon.service
	-systemctl --user daemon-reload 2>/dev/null
	-rm -f $(LEGACY_BIN)/immurok-daemon $(LEGACY_BIN)/immurok-cli $(LEGACY_BIN)/imk
	-rm -f $(LEGACY_BIN)/immurok-auth-dialog $(LEGACY_BIN)/immurok-pam-helper
	-rm -f $(LEGACY_BIN)/ble-notify-helper.py
	-rm -f $(HOME)/.config/autostart/com.immurok.Settings.desktop
	-@for rc in $(HOME)/.bashrc $(HOME)/.zshrc; do \
		[ -f "$$rc" ] || continue; \
		if grep -q '# added by immurok install' "$$rc"; then \
			sed -i '/# added by immurok install/,+1d' "$$rc"; \
			echo "OK: removed the PATH entry from $$rc"; \
		fi; \
	done
	@echo "=== Done$(if $(PURGE), (including /var/lib/immurok), (/var/lib/immurok kept; PURGE=1 removes it as well)) ==="

# -- Distribution packages ------------------------------------------------
# Builds deb / rpm / archlinux packages into dist/ (design doc:
# docs/superpowers/specs/2026-09-21-linux-packaging-design.md).
# Needs nfpm: https://github.com/goreleaser/nfpm/releases — unpack it and put
# the binary on PATH. The GUI is part of the package, so packaging requires
# the gtk4/libadwaita development headers.
DIST_DIR = dist
NFPM ?= nfpm

package: build pam
	@[ -n "$(HAS_GTK_DEV)" ] || { echo "FAILED: package needs immurok-gui: install gtk4/libadwaita dev headers"; exit 1; }
	@command -v $(NFPM) >/dev/null 2>&1 || { echo "FAILED: nfpm not found: https://github.com/goreleaser/nfpm/releases (put the binary in ~/.local/bin)"; exit 1; }
	bash packaging/stage.sh $(DIST_DIR)/stage
	@set -e; set -a; . packaging/pkg-env.sh; set +a; \
	for f in deb rpm archlinux; do $(NFPM) package -f packaging/nfpm.yaml -p $$f -t $(DIST_DIR)/; done
	@ls -1 $(DIST_DIR)/*.deb $(DIST_DIR)/*.rpm $(DIST_DIR)/*.pkg.tar.zst

# Install / verify / remove the packages in dist/ inside a distribution
# container — a local reproduction of CI's verify job.
#   make verify-pkg DISTRO=debian:12   (ubuntu:24.04 / fedora:43 / archlinux:latest)
# Note: binaries built on this host (Arch, new glibc) will not start inside
# debian:12 (GLIBC_2.39). CI builds in a debian:12 container; locally,
# debian:12 is only useful for checking dependency names and file placement.
DISTRO ?= debian:12
verify-pkg:
	docker run --rm -v "$$(pwd)/$(DIST_DIR):/dist:ro" -v "$$(pwd)/packaging/verify.sh:/verify.sh:ro" \
		$(DISTRO) sh /verify.sh /dist

clean:
	$(CARGO) clean
	$(MAKE) -C pam clean
	rm -rf $(DIST_DIR)
