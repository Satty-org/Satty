ifeq ($(PREFIX),)
    PREFIX := /usr/local
endif

SOURCEDIRS:=src $(wildcard src/*)
SOURCEFILES:=$(foreach d,$(SOURCEDIRS),$(wildcard $(d)/*.rs))

BINDIR:=$(PREFIX)/bin

BASHDIR:=$(PREFIX)/share/bash-completion/completions
ZSHDIR:=$(PREFIX)/share/zsh/site-functions
FISHDIR:=$(PREFIX)/share/fish/vendor_completions.d
ELVDIR:=$(PREFIX)/share/elvish/lib
NUDIR:=$(PREFIX)/share/nushell/completions
FIGDIR:=$(PREFIX)/share/fig/autocomplete

build: target/debug/tensaku

build-release: target/release/tensaku

force-build:
	cargo build --features ci-release

force-build-release:
	cargo build --release --features ci-release

target/debug/tensaku: $(SOURCEFILES) Cargo.lock Cargo.toml
	cargo build --features ci-release

target/release/tensaku: $(SOURCEFILES) Cargo.lock Cargo.toml
	cargo build --release --features ci-release

clean:
	cargo clean

install: target/release/tensaku
	install -s -Dm755 target/release/tensaku -t $(BINDIR)
	install -Dm644 dev.tensaku.Tensaku.desktop $(PREFIX)/share/applications/dev.tensaku.Tensaku.desktop
	install -Dm644 assets/tensaku.svg $(PREFIX)/share/icons/hicolor/scalable/apps/dev.tensaku.Tensaku.svg
	install -Dm644 LICENSE $(PREFIX)/share/licenses/tensaku/LICENSE
	install -Dm644 NOTICE $(PREFIX)/share/licenses/tensaku/NOTICE
	install -Dm644 completions/_tensaku $(ZSHDIR)/_tensaku
	install -Dm644 completions/tensaku.bash $(BASHDIR)/tensaku
	install -Dm644 completions/tensaku.fish $(FISHDIR)/tensaku.fish
	install -Dm644 completions/tensaku.elv $(ELVDIR)/tensaku.elv
	install -Dm644 completions/tensaku.nu $(NUDIR)/tensaku.nu
	install -Dm644 completions/tensaku.ts $(FIGDIR)/tensaku.ts
	install -Dm644 man/tensaku.1 ${PREFIX}/share/man/man1

uninstall:
	rm ${BINDIR}/tensaku
	rmdir -p ${PREFIX}/bin || true

	rm ${PREFIX}/share/applications/dev.tensaku.Tensaku.desktop
	rmdir -p ${PREFIX}/share/applications || true

	rm ${PREFIX}/share/icons/hicolor/scalable/apps/dev.tensaku.Tensaku.svg
	rmdir -p ${PREFIX}/share/icons/hicolor/scalable/apps || true

	rm ${PREFIX}/share/licenses/tensaku/LICENSE
	rm ${PREFIX}/share/licenses/tensaku/NOTICE
	rmdir -p ${PREFIX}/share/licenses/tensaku || true

	rm ${PREFIX}/share/man/man1/tensaku.1

	rm $(ZSHDIR)/_tensaku
	rmdir -p $(ZSHDIR) || true

	rm $(BASHDIR)/tensaku
	rmdir -p $(BASHDIR) || true

	rm $(FISHDIR)/tensaku.fish
	rmdir -p $(FISHDIR) || true

	rm $(ELVDIR)/tensaku.elv
	rmdir -p $(ELVDIR) || true

	rm $(NUDIR)/tensaku.nu
	rmdir -p $(NUDIR) || true

	rm $(FIGDIR)/tensaku.ts
	rmdir -p $(FIGDIR) || true

package: clean build-release
	$(eval TMP := $(shell mktemp -d))
	echo "Temporary folder ${TMP}"

	# install to tmp
	PREFIX=${TMP} make install

	# create package
	$(eval LATEST_TAG := $(shell git describe --tags --abbrev=0))
	tar -czvf tensaku-${LATEST_TAG}-x86_64.tar.gz -C ${TMP} .

	# clean up
	rm -rf $(TMP)

fix:
	cargo fmt --all
	cargo clippy --fix --allow-dirty --all-targets --all-features -- -D warnings

STARTPATTERN:=» tensaku --help
ENDPATTERN=```

# sed command adds command line help to README.md
# within startpattern and endpattern:
#   when startpattern is found, print it and read stdin
#   when endpattern is found, print it
#   everything else, delete
#
# The double -e is needed because r command cannot be terminated with semicolon.
# -i is tricky to use for both BSD/busybox sed AND GNU sed at the same time, so use mv instead.
update-readme: target/release/tensaku
	target/release/tensaku --help 2>&1 | sed -e '/${STARTPATTERN}/,/${ENDPATTERN}/{ /${STARTPATTERN}/p;r /dev/stdin' -e '/${ENDPATTERN}/p; d; }' README.md > README.md.new
	mv README.md.new README.md
