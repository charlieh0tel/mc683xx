#!/bin/sh
# Installs the wasm-bindgen CLI `make wasm` and `make npm-package` bind
# with into a directory, from the release's prebuilt x86_64 Linux
# binary, checked against its SHA-256: for CI's `wasm` job and the
# release's `npm` job, which put the directory on PATH.  Its version is
# crates/wasm/Cargo.toml's exact wasm-bindgen; a change to one is a
# change to both, and to the checksum.
#
# usage: install-wasm-bindgen.sh DIRECTORY
set -eu

version=0.2.130
# The release's wasm-bindgen-0.2.130-x86_64-unknown-linux-musl.tar.gz.
sha256=9687f3bb551b9f66f37ed32a40d7913a00f7962db653054e56857bc306c3227f
name=wasm-bindgen-$version-x86_64-unknown-linux-musl
directory=$1

grep -q "^wasm-bindgen = \"=$version\"$" crates/wasm/Cargo.toml || {
	echo "crates/wasm/Cargo.toml does not pin wasm-bindgen =$version" >&2
	exit 1
}

mkdir -p "$directory"
archive=$directory/$name.tar.gz
curl -fsSL -o "$archive" \
	"https://github.com/wasm-bindgen/wasm-bindgen/releases/download/$version/$name.tar.gz"
echo "$sha256  $archive" | sha256sum -c -
tar -xzf "$archive" -C "$directory" --strip-components=1 "$name/wasm-bindgen"
rm "$archive"
"$directory/wasm-bindgen" --version
