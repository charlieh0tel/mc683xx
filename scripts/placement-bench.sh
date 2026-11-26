#!/bin/bash
# Compares two commits' speed across code placements (docs/performance.md,
# Comparing placements): N builds of each, each relinked with its
# functions in another order (rust-lld's --shuffle-sections, the build's
# own flags kept), each run at every place the kernel can map the Python
# module at (ASLR off, the module moved 2 MB at a time through its eight
# places modulo 16 MB), the runs rotated, then an exact permutation test
# on the builds' means.  Opt-in, gates nothing; x86_64 Linux.
#
#   scripts/placement-bench.sh [options] BASE CANDIDATE
#
#   -n BUILDS  builds a side (default 8)
#   -r REPS    runs of each build at each place (default 1)
#   -c CPU     the CPU to pin to, best a performance core whose sibling
#              is idle (default 2)
#   -k CASE    write_register (default), write, code, none: a bare
#              Machine's move.l/addq/bra loop with that hook (none: no
#              hook); or "kernel ARGS...", benches/examples/iterations.rs
#              with ARGS (e.g. "kernel demo mandelbrot 30"), one place
#
# BASE and CANDIDATE are git revisions.  The Python cases need `make
# python`'s .venv (its interpreter and maturin).  Work goes under
# target/placement-bench/.
set -eu

builds=8 reps=1 cpu=2 case=write_register
while getopts n:r:c:k: option; do
    case $option in
    n) builds=$OPTARG ;;
    r) reps=$OPTARG ;;
    c) cpu=$OPTARG ;;
    k) case=$OPTARG ;;
    *) sed -n '2,25p' "$0" >&2; exit 2 ;;
    esac
done
shift $((OPTIND - 1))
[ $# -eq 2 ] || { sed -n '2,25p' "$0" >&2; exit 2; }

root=$(cd "$(dirname "$0")/.." && pwd)
work=$root/target/placement-bench
venv=$root/.venv
mkdir -p "$work"
kernel=no
case $case in kernel*) kernel=yes ;; esac
if [ $kernel = no ] && [ ! -x "$venv/bin/maturin" ]; then
    echo "placement-bench: run \`make python\` first, for $venv" >&2
    exit 1
fi

# build SIDE REVISION: the side's builds, each linked in its own order.
build() {
    side=$1 tree=$work/$1/tree
    rm -rf "$tree"
    mkdir -p "$tree"
    # -m: files dated now, so cargo rebuilds what changed since the last run
    git -C "$root" archive "$2" | tar -x -m -C "$tree"
    for n in $(seq 1 "$builds"); do
        link=(-C "link-arg=-Wl,--shuffle-sections=*=$n")
        out=$work/$side/build-$n
        rm -rf "$out"
        mkdir -p "$out"
        if [ $kernel = yes ]; then
            (cd "$tree" && CARGO_TARGET_DIR=$work/$side/target cargo rustc --quiet \
                --release -p mc683xx-bench --example iterations -- "${link[@]}")
            # the newest build of it, under its hashed name
            newest=
            for file in "$work/$side/target/release/examples"/iterations-*; do
                case $file in *.d) continue ;; esac
                if [ -z "$newest" ] || [ "$file" -nt "$newest" ]; then newest=$file; fi
            done
            cp "$newest" "$out/iterations"
        else
            (cd "$tree" && CARGO_TARGET_DIR=$work/$side/target "$venv/bin/maturin" build \
                --release --quiet -m crates/python/Cargo.toml -o "$out/wheel" -- "${link[@]}") \
                >"$out/build.log" 2>&1 || { cat "$out/build.log" >&2; exit 1; }
            "$venv/bin/python" -m zipfile -e "$out"/wheel/*.whl "$out/site"
        fi
        echo "built $side $n" >&2
    done
}

build base "$1"
build candidate "$2"

exec "${PYTHON:-python3}" - "$work" "$builds" "$reps" "$cpu" "$kernel" "$venv/bin/python" "$case" <<'EOF'
import itertools, os, shutil, statistics, subprocess, sys, time

work, builds, reps, cpu, kernel, python, case = sys.argv[1:]
builds, reps, cpu = int(builds), int(reps), int(cpu)
kernel = kernel == "yes"
siblings = open(f"/sys/devices/system/cpu/cpu{cpu}/topology/thread_siblings_list").read().strip()
sibling = next((int(c) for c in siblings.replace("-", ",").split(",") if int(c) != cpu), cpu)
perf = shutil.which("perf")
slots = [0] if kernel else list(range(8))

# The probe: the module moved by SLOT x 2 MB of mappings made before
# the import, the run timed alone.
PROBE = r'''
import mmap, os, struct, sys, time
slot = int(os.environ["SLOT"])
hold = mmap.mmap(-1, slot * 0x200000) if slot else None
import mc683xx
case, count = sys.argv[1], 3_000_000
machine = mc683xx.Machine()
machine.map(0, 0x10000)
# move.l d0,(a0); addq.l #1,d0; bra.s back
machine.poke_bytes(0x1000, struct.pack(">HHH", 0x2080, 0x5280, 0x60FA))
machine.write_register("SR", 0x2700)
machine.write_register("A7", 0x8000)
machine.write_register("A0", 0x6000)
if case == "write":
    machine.add_write_hook(lambda access: None, 0x6000, 0x6003)
elif case == "write_register":
    machine.add_write_hook(lambda access: (access.register("A7"), None)[1], 0x6000, 0x6003)
elif case == "code":
    machine.add_code_hook(lambda instruction: None)
elif case != "none":
    sys.exit(f"no case {case!r}")
start = time.perf_counter()
machine.run_limited(begin=0x1000, instructions=count)
print(count / (time.perf_counter() - start) / 1e6, mc683xx.__file__)
'''
open(f"{work}/probe.py", "w").write(PROBE)

def busy():
    def snap():
        rows = {}
        for line in open("/proc/stat"):
            if line.startswith("cpu") and line[3].isdigit():
                fields = line.split()
                values = list(map(int, fields[1:]))
                rows[int(fields[0][3:])] = (sum(values), values[3] + values[4])
        return rows
    a = snap(); time.sleep(0.5); b = snap()
    return {c: 100 * (1 - (b[c][1] - a[c][1]) / max(1, b[c][0] - a[c][0])) for c in (cpu, sibling)}

def quiet():
    while True:
        load = busy()
        if load[cpu] < 5 and load[sibling] < 10:
            return

def run(side, n, slot):
    out = f"{work}/{side}/build-{n}"
    env = {"PATH": os.environ["PATH"], "PYTHONHASHSEED": "0", "SLOT": str(slot)}
    if kernel:
        command = [f"{out}/iterations"] + case.split()[1:]
    else:
        env["PYTHONPATH"] = f"{out}/site"
        command = [python, "-s", f"{work}/probe.py", case]
    stat = ["perf", "stat", "-x,", "-e", "cycles:u", "-o", f"{work}/perf.txt"] if perf else []
    start = time.perf_counter()
    result = subprocess.run(["taskset", "-c", str(cpu)] + stat + ["setarch", "-R"] + command,
                            env=env, capture_output=True, text=True, check=True)
    elapsed = time.perf_counter() - start
    cycles = None
    if perf:
        # a hybrid processor reports each core type's count
        cycles = sum(int(fields[0]) for fields in (line.split(",") for line in open(f"{work}/perf.txt"))
                     if len(fields) > 2 and fields[0].isdigit())
    if kernel:
        # higher is better throughout: runs a G cycles, or a second without perf
        return (1e9 / cycles if cycles else 1 / elapsed), cycles
    rate, module = result.stdout.split()
    assert module.startswith(out), module
    return float(rate), cycles

sides = ("base", "candidate")
runs = [(side, n) for side in sides for n in range(1, builds + 1)]
rates = {key: {slot: [] for slot in slots} for key in runs}
cycles = {key: [] for key in runs}
for rep in range(reps):
    for slot in slots:
        shift = (rep * len(slots) + slot) % len(runs)
        for side, n in runs[shift:] + runs[:shift]:
            quiet()
            rate, spent = run(side, n, slot)
            rates[(side, n)][slot].append(rate)
            if spent:
                cycles[(side, n)].append(spent)

unit = ("runs/G cycles" if perf else "runs/s") if kernel else "M instructions/s"
print(f"{case}: {unit}, the median of {reps} at each place; higher is faster")
print("build      " + " ".join(f"slot{s:<4d}" for s in slots) + "    mean   worst" + ("  cycles(M)" if perf else ""))
means = {side: [] for side in sides}
worsts = {side: [] for side in sides}
for side, n in runs:
    medians = [statistics.median(rates[(side, n)][s]) for s in slots]
    means[side].append(statistics.mean(medians))
    worsts[side].append(min(medians))
    tail = f"  {statistics.mean(cycles[(side, n)]) / 1e6:9.1f}" if cycles[(side, n)] else ""
    print(f"{side[:4]}-{n:<5d} " + " ".join(f"{m:8.3f}" for m in medians)
          + f" {means[side][-1]:7.3f} {worsts[side][-1]:7.3f}" + tail)

def permutation(a, b):
    observed = abs(statistics.mean(b) - statistics.mean(a))
    pool, total, size = a + b, sum(a + b), len(a)
    hits = count = 0
    for chosen in itertools.combinations(range(len(pool)), size):
        first = sum(pool[i] for i in chosen) / size
        second = (total - first * size) / (len(pool) - size)
        count += 1
        hits += abs(second - first) >= observed - 1e-12
    return hits / count

a, b = means["base"], means["candidate"]
print(f"base       mean {statistics.mean(a):.3f} [{min(a):.3f}-{max(a):.3f}]  worst place {min(worsts['base']):.3f}")
print(f"candidate  mean {statistics.mean(b):.3f} [{min(b):.3f}-{max(b):.3f}]  worst place {min(worsts['candidate']):.3f}")
print(f"candidate {100 * (statistics.mean(b) / statistics.mean(a) - 1):+.2f}%, "
      f"exact permutation p = {permutation(a, b):.4f} over the builds' means")
EOF
