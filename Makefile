PREFIX ?= /usr/local

.PHONY: build test check install uninstall system-install system-uninstall clean

build:
	cargo build --release

test:
	cargo test

check:
	sh ./scripts/check.sh

install: build
	install -Dm755 target/release/lifeline $(DESTDIR)$(PREFIX)/bin/lifeline
	install -Dm755 target/release/lifeline-daemon $(DESTDIR)$(PREFIX)/bin/lifeline-daemon

uninstall:
	rm -f $(DESTDIR)$(PREFIX)/bin/lifeline $(DESTDIR)$(PREFIX)/bin/lifeline-daemon

system-install: build
	sudo sh ./scripts/install-system-service.sh

system-uninstall:
	sudo sh ./scripts/uninstall-system-service.sh

clean:
	cargo clean
