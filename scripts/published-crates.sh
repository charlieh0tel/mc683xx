#!/bin/sh
# Prints the publishable workspace crates whose version crates.io
# already has, from its sparse index, one per line.  Fails on any
# answer but 200 or 404.
#
# usage: published-crates.sh
set -eu

index=https://index.crates.io
file=$(mktemp)
trap 'rm -f "$file"' EXIT

cargo metadata --no-deps --format-version 1 --locked |
	jq -r '.packages[] | select(.publish != []) | "\(.name) \(.version)"' |
	while read -r name version; do
		case ${#name} in
		1) path=1/$name ;;
		2) path=2/$name ;;
		3) path=3/$(printf %.1s "$name")/$name ;;
		*) path=$(printf %.2s "$name")/$(printf %s "$name" | cut -c3-4)/$name ;;
		esac
		status=$(curl -sS -o "$file" -w '%{http_code}' "$index/$path")
		case $status in
		200)
			if jq -s -e --arg version "$version" \
				'any(.[]; .vers == $version)' "$file" >/dev/null; then
				echo "$name"
			fi
			;;
		404) ;;
		*)
			echo "$index/$path: HTTP $status" >&2
			exit 1
			;;
		esac
	done
