#!/bin/sh
# PS4 port: turns a profile.txt (the probe's or the app's, from /data/rac-rs or the PC's player folder; see ps4/probe/src/prof.rs
# and ps4/app/src/prof*.rs) into a table of where the time went, by function, using the symbols of the ELF / exe that ran.
#   sh ps4/prof.sh profile.txt [build-ps4/probe/probe.elf]
# The app's profiles cover every thread (third column = thread name): lines read "thread  function". THREAD=name keeps one
# thread (its percentages are then of that thread's samples). TOP=N lines (default 60).
set -e
ROOT=$(cd "$(dirname "$0")/.." && pwd)
PROF=$1
ELF=${2:-$ROOT/build-ps4/probe/probe.elf}
BIN="$ROOT/toolchain/rustup/toolchains/1.99.0-x86_64-pc-windows-gnu/lib/rustlib/x86_64-pc-windows-gnu/bin"
SYM=$(mktemp)
# Function symbols, sorted by address, names demangled.
"$BIN/llvm-nm.exe" --defined-only -n -C "$ELF" | awk '$2 ~ /^[tTwW]$/ { a = $1; $1 = ""; $2 = ""; sub(/^  /, ""); print a "\t" $0 }' > "$SYM"
ANCHOR=$(grep -m1 "prof::anchor" "$SYM" | cut -f1)
awk -v anchor_elf="$ANCHOR" -v only="$THREAD" -v top="${TOP:-60}" -F'\t' '
	function hex(s,   i, c, v) { v = 0; s = tolower(s); for (i = 1; i <= length(s); i++) { c = index("0123456789abcdef", substr(s, i, 1)) - 1; v = v * 16 + c } return v }
	function find(x,   lo, hi, mid) {
		if (x < addr[1] || x > end_) return ""
		lo = 1; hi = n
		while (lo < hi) { mid = int((lo + hi + 1) / 2); if (addr[mid] <= x) lo = mid; else hi = mid - 1 }
		return name[lo]
	}
	FILENAME != ARGV[2] { n++; addr[n] = hex($1); name[n] = $2; end_ = addr[n] + 65536; next }
	/^# thread / { print; next }
	/^#/ { next }
	FNR == 1 { split($0, p, " "); off = hex(p[2]) - hex(anchor_elf); next }
	{
		split($0, p, " ")
		if (only != "" && p[3] != only) next
		total++
		f = find(hex(p[1]) - off)
		if (f == "") { out++; c = find(hex(p[2]) - off); if (c == "") c = "?"; f = "[outside the game: called from " c "]" }
		if (p[3] != "" && only == "") f = p[3] "  " f
		cnt[f]++
	}
	END {
		printf "%d samples, %d outside the game code (system library)\n", total, out
		for (f in cnt) printf "%6.2f%%  %7d  %s\n", 100 * cnt[f] / total, cnt[f], f | ("sort -rn | head -" top)
	}
' "$SYM" "$PROF"
rm -f "$SYM"
