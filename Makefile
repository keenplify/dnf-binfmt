PREFIX ?= /usr
LIBDIR ?= $(PREFIX)/lib64
CARGO ?= cargo
CXX ?= c++
CC ?= cc
X86_AS ?= $(if $(filter x86_64,$(shell uname -m)),as,x86_64-linux-gnu-as)
X86_LD ?= $(if $(filter x86_64,$(shell uname -m)),ld,x86_64-linux-gnu-ld)
CPPFLAGS ?=
CXXFLAGS ?= -O2 -Wall -Wextra -Werror
BACKEND ?= $(PREFIX)/libexec/dnf-binfmt

.PHONY: all test install clean build/binfmt.so
all: target/release/dnf-binfmt build/binfmt.so build/runtime/rootfs.erofs build/runtime/native/dnf-binfmt-mmap.so

build/runtime/native/dnf-binfmt-mmap.so: helpers/mmap_compat.c
	mkdir -p $(@D)
	$(CC) -shared -fPIC -O2 -Wall -Wextra -Werror -Wl,-z,relro,-z,now $< -o $@

build/runtime/guest/usr/lib64/dnf-binfmt-mmap.so: helpers/mmap_compat_x86_64.S
	mkdir -p $(@D)
	$(X86_AS) $< -o build/runtime/mmap.o
	$(X86_LD) -shared -z noexecstack -z relro -z now build/runtime/mmap.o -o $@

build/runtime/rootfs.erofs: build/runtime/guest/usr/lib64/dnf-binfmt-mmap.so
	mkfs.erofs -b4096 -zlz4 --all-root $@ build/runtime/guest

target/release/dnf-binfmt: Cargo.toml $(wildcard src/*.rs)
	$(CARGO) build --release --offline

build/binfmt.so: plugin/binfmt.cpp
	mkdir -p build
	$(CXX) $(CPPFLAGS) $(CXXFLAGS) -std=c++20 -fPIC -shared -DBINFMT_BACKEND='"$(BACKEND)"' $< -o $@ -l:libdnf5-cli.so.3 -l:libdnf5.so.2

test: all
	$(CARGO) test --offline
	python3 -m unittest discover -s tests -p 'test_*.py'

install: all
	install -Dm755 target/release/dnf-binfmt $(DESTDIR)$(BACKEND)
	install -Dm755 build/binfmt.so $(DESTDIR)$(LIBDIR)/dnf5/plugins/binfmt.so
	install -Dm644 helpers/session_bus.py $(DESTDIR)$(PREFIX)/libexec/dnf-binfmt-session-bus
	install -Dm755 build/runtime/native/dnf-binfmt-mmap.so $(DESTDIR)$(PREFIX)/libexec/dnf-binfmt-runtime/native/dnf-binfmt-mmap.so
	install -Dm644 build/runtime/rootfs.erofs $(DESTDIR)$(PREFIX)/libexec/dnf-binfmt-runtime/rootfs.erofs
	install -Dm644 LICENSE $(DESTDIR)$(PREFIX)/share/licenses/dnf-binfmt/LICENSE

clean:
	$(CARGO) clean
	rm -rf build
