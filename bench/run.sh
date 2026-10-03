#!/usr/bin/env bash
set -u
ROOT=$(cd "$(dirname "$0")/.." && pwd); cd "$ROOT"
GBFC=${GBFC:-$ROOT/target/release/gbfc}
BASELINE=${BASELINE:-}
RUNS=${RUNS:-3}
TIMEOUT=${TIMEOUT:-120}
FUEL=${FUEL:-4000000000}
W=$(mktemp -d); trap 'rm -rf "$W"' EXIT
TP=bench/programs/third_party

[ -x "$GBFC" ] || { echo "compiler not found: $GBFC (run: cargo build --release)" >&2; exit 1; }
bash bench/fetch.sh >&2

now() { date +%s%N; }
fmt() {
  awk -v n="$1" 'BEGIN{ if(n=="T") print ">'"$TIMEOUT"' s"; else if(n<1e6) printf "%.2f ms", n/1e6; else if(n<1e9) printf "%.1f ms", n/1e6; else printf "%.2f s", n/1e9 }'
}

rep() { head -c "$2" < /dev/zero | tr '\0' "$1"; }
echo ',[.,]' > "$W/cat.b"
yes 'The quick brown fox jumps over the lazy dog 0123456789' | head -c $((8 << 20)) > "$W/cat.in"
{ printf '>>>'; rep + 65; printf '<<<'; rep + 200; printf '[>'; rep + 200; printf '[>'; rep + 100; printf '[>.<-]<-]<-]\n'; } > "$W/print4m.b"
echo 179424691 > "$W/factor.in"
: > "$W/empty.in"

PROGRAMS=(
  "mandelbrot|$TP/mandelbrot.b|$W/empty.in|"
  "mandel (kostya)|$TP/mandel_kostya.b|$W/empty.in|"
  "long|$TP/long.b|$W/empty.in|"
  "hanoi|$TP/hanoi.b|$W/empty.in|"
  "bench (kostya)|$TP/bench_kostya.b|$W/empty.in|"
  "print 4M bytes|$W/print4m.b|$W/empty.in|"
  "factor 179424691|$TP/factor.b|$W/factor.in|"
  "cat 8 MiB|$W/cat.b|$W/cat.in|"
)

HDR="| program | "; SEP="|---|"
[ -n "$BASELINE" ] && { HDR+="v0.1.0 | "; SEP+="---:|"; }
HDR+="-O0 | -O1 --fuel 0 | -O1 --fuel $FUEL | compile | output |"; SEP+="---:|---:|---:|---:|:---:|"
echo "$HDR"; echo "$SEP"

for entry in "${PROGRAMS[@]}"; do
  IFS='|' read -r name src input _ <<< "$entry"
  [ -s "$src" ] || { echo "| $name | (program not available) |"; continue; }

  exes=(); ctime=""
  if [ -n "$BASELINE" ]; then
    "$BASELINE" "$src" -o "$W/e_base" 2>/dev/null && exes+=("$W/e_base") || exes+=("")
  fi
  "$GBFC" -O0 "$src" -o "$W/e_o0" 2>/dev/null;          exes+=("$W/e_o0")
  "$GBFC" -O1 --fuel 0 "$src" -o "$W/e_o1" 2>/dev/null; exes+=("$W/e_o1")
  cs=$(now); "$GBFC" --fuel "$FUEL" "$src" -o "$W/e_def" 2>/dev/null; ce=$(now); ctime=$((ce - cs)); exes+=("$W/e_def")

  best=(); hash=()
  for k in "${!exes[@]}"; do best[$k]=""; hash[$k]=""; done
  for _ in $(seq "$RUNS"); do
    for k in "${!exes[@]}"; do
      [ -z "${exes[$k]}" ] && continue
      [ "${best[$k]}" = T ] && continue
      s=$(now); timeout "$TIMEOUT" "${exes[$k]}" < "$input" > "$W/out" 2>/dev/null; rc=$?; e=$(now)
      if [ $rc -eq 124 ]; then best[$k]=T; hash[$k]=timeout; continue; fi
      d=$((e - s)); { [ -z "${best[$k]}" ] || [ "$d" -lt "${best[$k]}" ]; } && best[$k]=$d
      hash[$k]=$(md5sum < "$W/out" | cut -c1-12)
    done
  done

  row="| $name | "; ref=""; ok="✓"
  for k in "${!exes[@]}"; do
    if [ -z "${exes[$k]}" ]; then row+="n/a | "; continue; fi
    row+="$(fmt "${best[$k]}") | "
    [ -z "$ref" ] && ref="${hash[$k]}"
    [ "${hash[$k]}" = "$ref" ] || ok="✗ MISMATCH"
    [ "${hash[$k]}" = timeout ] && ok="timeout"
  done
  echo "${row}$(fmt "$ctime") | $ok |"
done
