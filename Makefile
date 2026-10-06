PREFIX ?= /usr
LIBDIR ?= $(PREFIX)/lib64
CARGO ?= cargo
CXX ?= c++
CPPFLAGS ?=
CXXFLAGS ?= -O2 -Wall -Wextra -Werror
BACKEND ?= $(PREFIX)/libexec/dnf-binfmt

.PHONY: all test install clean build/binfmt.so
all: target/release/dnf-binfmt build/binfmt.so

target/release/dnf-binfmt: Cargo.toml $(wildcard src/*.rs)
	$(CARGO) build --release --offline

build/binfmt.so: plugin/binfmt.cpp
	mkdir -p build
	$(CXX) $(CPPFLAGS) $(CXXFLAGS) -std=c++20 -fPIC -shared -DBINFMT_BACKEND='"$(BACKEND)"' $< -o $@ -l:libdnf5-cli.so.3 -l:libdnf5.so.2

test:
	$(CARGO) test --offline
	python3 -m unittest discover -s tests -p 'test_*.py'

install: all
	install -Dm755 target/release/dnf-binfmt $(DESTDIR)$(BACKEND)
	install -Dm755 build/binfmt.so $(DESTDIR)$(LIBDIR)/dnf5/plugins/binfmt.so
	install -Dm644 helpers/session_bus.py $(DESTDIR)$(PREFIX)/libexec/dnf-binfmt-session-bus
	install -Dm644 LICENSE $(DESTDIR)$(PREFIX)/share/licenses/dnf-binfmt/LICENSE

clean:
	$(CARGO) clean
	rm -rf build
