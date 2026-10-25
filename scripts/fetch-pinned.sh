#!/bin/sh
# Fetches one commit of a Git repository into a directory and checks it
# out: `make singlestep-data` and `make musashi`, into the ignored
# external/.  Does nothing if the directory already holds that commit.
#
# usage: fetch-pinned.sh URL COMMIT DIRECTORY
set -eu

url=$1
commit=$2
directory=$3

if [ -e "$directory" ]; then
	if [ "$(git -C "$directory" rev-parse HEAD 2>/dev/null)" = "$commit" ]; then
		exit 0
	fi
	echo "$directory exists, but not at $commit; remove it to fetch again" >&2
	exit 1
fi

mkdir -p "$directory"
git -C "$directory" init -q
git -C "$directory" fetch -q --depth 1 "$url" "$commit"
git -C "$directory" checkout -q FETCH_HEAD
test "$(git -C "$directory" rev-parse HEAD)" = "$commit"
