PREFIX ?= /usr/local

.PHONY: build test install uninstall clean

build:
	cargo build --release

test:
	cargo test

install: build
	install -Dm755 target/release/lifeline $(DESTDIR)$(PREFIX)/bin/lifeline
	install -Dm755 target/release/lifeline-daemon $(DESTDIR)$(PREFIX)/bin/lifeline-daemon

uninstall:
	rm -f $(DESTDIR)$(PREFIX)/bin/lifeline $(DESTDIR)$(PREFIX)/bin/lifeline-daemon

clean:
	cargo clean
