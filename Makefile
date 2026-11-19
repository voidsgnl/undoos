PREFIX ?= /usr/local

.PHONY: build test check install uninstall clean

build:
	cargo build --release

test:
	cargo test

check:
	./scripts/check.sh

install: build
	install -Dm755 target/release/lifeline $(DESTDIR)$(PREFIX)/bin/lifeline
	install -Dm755 target/release/lifeline-daemon $(DESTDIR)$(PREFIX)/bin/lifeline-daemon

uninstall:
	rm -f $(DESTDIR)$(PREFIX)/bin/lifeline $(DESTDIR)$(PREFIX)/bin/lifeline-daemon

clean:
	cargo clean
