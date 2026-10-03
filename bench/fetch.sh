#!/usr/bin/env bash
set -u
dir="$(cd "$(dirname "$0")" && pwd)/programs/third_party"
mkdir -p "$dir"
get() {
  [ -s "$dir/$1" ] && return 0
  curl -sfL -m 30 "$2" -o "$dir/$1" && [ -s "$dir/$1" ] && echo "fetched $1" || { rm -f "$dir/$1"; echo "could not fetch $1" >&2; }
}
M=https://raw.githubusercontent.com/matslina/bfoptimization/master/progs
K=https://raw.githubusercontent.com/kostya/benchmarks/master/brainfuck
get mandelbrot.b   $M/mandelbrot.b
get hanoi.b        $M/hanoi.b
get long.b         $M/long.b
get factor.b       $M/factor.b
get mandel_kostya.b $K/mandel.b
get bench_kostya.b  $K/bench.b
