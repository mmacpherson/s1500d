PREFIX    ?= /usr
DESTDIR   ?=
BINDIR    ?= $(PREFIX)/bin
SYSCONFDIR ?= /etc
SYSTEMD_DIR ?= $(PREFIX)/lib/systemd/system
UDEV_DIR  ?= $(PREFIX)/lib/udev/rules.d
SHAREDIR  ?= $(PREFIX)/share
LICENSEDIR ?= $(SHAREDIR)/licenses/s1500d

.PHONY: build release install uninstall clean fmt clippy check

build:
	cargo build

release:
	cargo build --release

install:
	install -Dm0755 target/release/s1500d $(DESTDIR)$(BINDIR)/s1500d
	install -Dm0644 contrib/s1500d.service $(DESTDIR)$(SYSTEMD_DIR)/s1500d.service
	install -Dm0644 contrib/99-scansnap.rules $(DESTDIR)$(UDEV_DIR)/99-scansnap.rules
	install -Dm0644 contrib/config.toml $(DESTDIR)$(SYSCONFDIR)/s1500d/config.toml
	install -Dm0755 contrib/handler-example.sh $(DESTDIR)$(SHAREDIR)/s1500d/handler-example.sh
	install -Dm0755 contrib/handler-scan-to-pdf.sh $(DESTDIR)$(SHAREDIR)/s1500d/handler-scan-to-pdf.sh
	install -Dm0644 LICENSE-MIT $(DESTDIR)$(LICENSEDIR)/LICENSE-MIT
	install -Dm0644 LICENSE-APACHE $(DESTDIR)$(LICENSEDIR)/LICENSE-APACHE

uninstall:
	rm -f $(DESTDIR)$(BINDIR)/s1500d
	rm -f $(DESTDIR)$(SYSTEMD_DIR)/s1500d.service
	rm -f $(DESTDIR)$(UDEV_DIR)/99-scansnap.rules
	rm -rf $(DESTDIR)$(SYSCONFDIR)/s1500d
	rm -rf $(DESTDIR)$(SHAREDIR)/s1500d
	rm -rf $(DESTDIR)$(LICENSEDIR)

clean:
	cargo clean

fmt:
	cargo fmt

clippy:
	cargo clippy --all-targets -- -D warnings

check: fmt clippy
	cargo build
