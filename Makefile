ifeq ($(PREFIX),)
    PREFIX := /usr/local
endif

SOURCEDIRS:=src $(wildcard src/*)
SOURCEFILES:=$(foreach d,$(SOURCEDIRS),$(wildcard $(d)/*.rs))

build: target/debug/satty

build-release: target/release/satty

force-build:
	cargo build

force-build-release:
	cargo build --release

target/debug/satty: $(SOURCEFILES) Cargo.lock Cargo.toml
	cargo build

target/release/satty: $(SOURCEFILES) Cargo.lock Cargo.toml
	cargo build --release

clean:
	cargo clean

install: target/release/satty
	install -s -Dm755 target/release/satty -t ${PREFIX}/bin/
	install -Dm644 satty.desktop ${PREFIX}/share/applications/satty.desktop
	install -Dm644 assets/satty.svg ${PREFIX}/share/icons/hicolor/scalable/apps/satty.svg

	install -Dm644 LICENSE ${PREFIX}/share/licenses/satty/LICENSE

uninstall:
	rm ${PREFIX}/bin/satty
	rmdir -p ${PREFIX}/bin || true

	rm ${PREFIX}/share/applications/satty.desktop
	rmdir -p ${PREFIX}/share/applications || true

	rm ${PREFIX}/share/icons/hicolor/scalable/apps/satty.svg
	rmdir -p ${PREFIX}/share/icons/hicolor/scalable/apps || true

	rm ${PREFIX}/share/licenses/satty/LICENSE
	rmdir -p ${PREFIX}/share/licenses/satty || true

	
package: clean build-release
	$(eval TMP := $(shell mktemp -d))
	echo "Temporary folder ${TMP}"
	
	# install to tmp
	PREFIX=${TMP} make install
	
	# create package
	$(eval LATEST_TAG := $(shell git describe --tags --abbrev=0))
	tar -czvf satty-${LATEST_TAG}-x86_64.tar.gz -C ${TMP} .
	
	# clean up
	rm -rf $(TMP)

fix:
	cargo fmt --all
	cargo clippy --fix --allow-dirty --all-targets --all-features -- -D warnings

STARTPATTERN:=» satty --help
ENDPATTERN=```

# sed command adds command line help to README.md
# within startpattern and endpattern: 
#   when startpattern is found, print it and read stdin
#   when endpattern is found, print it
#   everything else, delete
#
# The double -e is needed because r command cannot be terminated with semicolon.
# -i is tricky to use for both BSD/busybox sed AND GNU sed at the same time, so use mv instead.
update-readme: target/release/satty
	target/release/satty --help 2>&1 | sed -e '/${STARTPATTERN}/,/${ENDPATTERN}/{ /${STARTPATTERN}/p;r /dev/stdin' -e '/${ENDPATTERN}/p; d; }' README.md > README.md.new
	mv README.md.new README.md


