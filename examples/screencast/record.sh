#!/bin/bash
# Records the README's screencast: the demo board under mc683xx-run on
# the left, gdb-multiarch attached on the right, in a tmux session this
# script types into, recorded by asciinema and rendered by agg.
#
#   examples/screencast/record.sh [AGG]
#
# From the repository's top, with asciinema, tmux and gdb-multiarch on
# PATH and agg at AGG (agg on PATH by default).  It writes
# examples/screencast/demo.cast and docs/demo.gif.
set -euo pipefail

agg=${1:-agg}
here=examples/screencast
cast=$here/demo.cast
gif=docs/demo.gif
socket=mc683xx-cast
session=cast
port=1234
columns=158
rows=36

tmux() { command tmux -L "$socket" "$@"; }

# Waits until `pane` shows `text`, for at most 30 seconds.
wait_for() {
	local pane=$1 text=$2
	for _ in $(seq 300); do
		tmux capture-pane -p -t "$pane" | grep -qF -- "$text" && return
		sleep 0.1
	done
	echo "record.sh: never saw '$text' in pane $pane" >&2
	tmux capture-pane -p -t "$pane" >&2
	exit 1
}

# Waits until the demo's prompt, `>`, is the console's last line (a
# pane's capture drops trailing blanks).
wait_prompt() {
	for _ in $(seq 300); do
		tmux capture-pane -p -t "$console" | grep -v '^[[:space:]]*$' | tail -1 |
			grep -qx '>' && return
		sleep 0.1
	done
	echo "record.sh: never saw the demo's prompt" >&2
	exit 1
}

# Types `text` into `pane` a key at a time, then Enter unless told not to.
type_in() {
	local pane=$1 text=$2 enter=${3:-yes}
	for ((i = 0; i < ${#text}; i++)); do
		tmux send-keys -t "$pane" -l -- "${text:i:1}"
		sleep 0.06
	done
	[ "$enter" = yes ] && tmux send-keys -t "$pane" Enter
	return 0
}

# A gdb command, typed, then its prompt waited for.
gdb_do() {
	type_in "$gdb" "$1"
	sleep "${2:-1.2}"
}

cargo build --release -q -p mc683xx-run

# Each pane a plain shell in a copy of the demo's files at a path that
# names no one's home (gdb shows a link's target), the firmware's
# sources linked in, the runner just built on its PATH; tmux's server
# goes with the script, however it ends, and the recording with it.
top=/tmp/mc683xx
rm -rf "$top"
mkdir -p "$top/examples/boards" "$top/examples/firmware"
cp examples/boards/demo.toml examples/boards/demo.bin examples/boards/demo.elf "$top/examples/boards/"
ln -s "$PWD/examples/firmware/demo" "$top/examples/firmware/demo"
shell="cd $top && env PATH=$PWD/target/release:$PATH PS1='\$ ' bash --norc --noprofile"
tmux kill-server 2>/dev/null || true
trap 'tmux kill-server 2>/dev/null || true; rm -rf "$top"' EXIT
tmux -f /dev/null new-session -d -s "$session" -x "$columns" -y "$rows" -n demo "$shell"
tmux set -g status off
tmux set -g pane-border-style fg=colour240
tmux set -g pane-active-border-style fg=colour240
tmux split-window -h -t "$session" -l 76 "$shell"
console=$session:0.0
gdb=$session:0.1

asciinema rec --overwrite --quiet --cols "$columns" --rows "$rows" \
	--idle-time-limit 2 --title "mc683xx: the demo board under gdb" \
	-c "tmux -L $socket attach -t $session" "$cast" &
recorder=$!
sleep 1.5

# The board: power-on reset, then it waits for gdb.
type_in "$console" "mc683xx-run examples/boards/demo.toml --gdb 127.0.0.1:$port --bind console=tty"
sleep 1.5
# gdb: attach, and run the firmware.
type_in "$gdb" "gdb-multiarch -q -nx examples/boards/demo.elf"
wait_for "$gdb" "(gdb)"
for setting in "set pagination off" "set confirm off"; do
	tmux send-keys -t "$gdb" -l "$setting" && tmux send-keys -t "$gdb" Enter
done
sleep 0.5
tmux send-keys -t "$gdb" C-l
gdb_do "target remote :$port" 1.5
gdb_do "continue" 1
wait_prompt

# A Mandelbrot set, from the firmware's fixed-point C.
type_in "$console" "m"
sleep 1.5
wait_prompt
sleep 1.5

# A breakpoint: stop, set it, and run into it from the console.
tmux send-keys -t "$gdb" C-c
sleep 1
gdb_do "break primes"
gdb_do "continue" 0.8
type_in "$console" "p"
wait_for "$gdb" "primes ()"
sleep 1
gdb_do "bt" 1.5
gdb_do "next" 1.2
gdb_do "continue" 1.5

# Stop on fault: the board stops at the bus or address error itself,
# before the firmware takes it.
tmux send-keys -t "$gdb" C-c
sleep 1
gdb_do "delete" 0.6
gdb_do "monitor faults stop" 1
gdb_do "continue" 0.8
type_in "$console" "f"
wait_for "$gdb" "SIGBUS"
sleep 1.5
gdb_do "monitor faults" 2
gdb_do "monitor faults take" 1
gdb_do "continue" 2.5

# Let go: the board runs on, then Ctrl-] ends it.
tmux send-keys -t "$gdb" C-c
sleep 1
gdb_do "detach" 2
tmux send-keys -t "$console" C-]
sleep 1.5

tmux kill-server
rm -rf "$top"
trap - EXIT
wait "$recorder" || true
# The recording ends on the session's last screen, not tmux leaving it.
python3 - "$cast" <<'TRIM'
import json, sys
path = sys.argv[1]
lines = open(path).read().splitlines()
events = [json.loads(line) for line in lines[1:]]
end = next(
    (index for index, (_, _, data) in enumerate(events)
     if "\x1b[?1049l" in data or "[server exited]" in data or "[exited]" in data),
    len(events),
)
with open(path, "w") as cast:
    print(lines[0], file=cast)
    for event in events[:end]:
        print(json.dumps(event), file=cast)
TRIM
"$agg" --font-size 14 --theme monokai --idle-time-limit 2 --last-frame-duration 3 "$cast" "$gif"
ls -l "$cast" "$gif"
