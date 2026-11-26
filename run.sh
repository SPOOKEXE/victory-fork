#!/usr/bin/env bash
# Builds the layer for Windows and runs the title under Proton.
#
#   ./run.sh [--build-only] [--no-build] [--release] [--safe] [--no-trace] [--time SECONDS] [-- title arguments]
#
#   --release   the plain release build (default: the diagnostics build)
#   --safe      without the speed switches below
#   --no-trace  without the frame graph
#   --time      stop the title after this many seconds
#
# Each run writes its log to $VICTORY_HOME/logs, its frame graph to
# $VICTORY_HOME/traces (gzip Chrome trace: open in ui.perfetto.dev), and on
# a crash, a GPU fault (Xid), a stall or a device loss a bundle to
# $VICTORY_HOME/crashes/<run>.tar.gz: the reason, the kernel's GPU lines,
# vkd3d's fault report, the log's errors and tail, minidumps, the trace.
#
# VICTORY_GAME_DIR  the dump (default: the first folder beside the repository
#                   that holds the title's executable)
# PROTON_DIR        a Proton folder (default: Proton - Experimental)
# VICTORY_HOME      prefix, caches, saves and logs (default: ~/.local/share/victory)
#
# Any VICTORY_* variable already set wins over the defaults here; an empty
# one takes the default back out.
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
profile=diag; build=1; run=1; fast=1; limit=0; tracing=1
while (($#)); do
    case $1 in
        --build-only) run=0 ;;
        --no-build) build=0 ;;
        --release) profile=release ;;
        --safe) fast=0 ;;
        --no-trace) tracing=0 ;;
        --time) limit=${2:?--time needs seconds}; shift ;;
        --) shift; break ;;
        *) sed -n '2,6p' "$0" >&2; exit 2 ;;
    esac
    shift
done

die() { echo "run.sh: $*" >&2; exit 1; }

home=${VICTORY_HOME:-$HOME/.local/share/victory}
target=x86_64-pc-windows-gnu
export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-$HOME/.cache/victory-target}

native=$CARGO_TARGET_DIR/native

if ((build)); then
    command -v x86_64-w64-mingw32-gcc >/dev/null || die "mingw-w64-gcc is needed to build"
    (cd "$root"
     rustup target list --installed | grep -qx "$target" || rustup target add "$target"
     # The layer's own shaders, if they or the compiler's copies are missing or older.
     hlsl=crates/victory-gpu/src/hlsl
     stale=0
     for f in "$hlsl"/*.hlsl; do
         o=$hlsl/dxil/$(basename "$f" .hlsl).dxil
         [[ -f $o && ! $f -nt $o ]] || stale=1
     done
     if ((stale)); then
         dxc=${DXC:-$(command -v dxc || echo "$HOME/.cache/victory-dxc/dxc/bin/dxc")}
         if [[ ! -x $dxc ]]; then
             t=$HOME/.cache/victory-dxc
             mkdir -p "$t/dxc"
             curl -sSfL -o "$t/dxc.tar.gz" https://github.com/microsoft/DirectXShaderCompiler/releases/download/v1.9.2609/linux_dxc_2026_09_28.x86_x64.tar.gz
             echo "96faadc7f5c282d2ffda49804beb4c3ee38127bc252b723234e3c5cdf7aa39a1  $t/dxc.tar.gz" | sha256sum -c --quiet
             tar xf "$t/dxc.tar.gz" -C "$t/dxc"
             chmod +x "$dxc"
         fi
         command -v b3sum >/dev/null || cargo install -q b3sum
         PATH=$PATH:$HOME/.cargo/bin DXC=$dxc "$hlsl/build.sh"
     fi
     args=(--profile "$profile" -p victory --target "$target")
     [[ $profile == diag ]] && args+=(--features diagnostics)
     cargo build "${args[@]}")
    # The native library (native/): a Wine builtin, so it carries Wine's mark.
    mkdir -p "$native"/x86_64-{unix,windows}
    gcc -O2 -shared -fPIC -o "$native/x86_64-unix/victory_native.so" "$root/native/victory_native.c"
    dll=$native/x86_64-windows/victory_native.dll
    x86_64-w64-mingw32-gcc -O2 -shared -nostdlib -e DllMain -o "$dll" "$root/native/victory_native_dll.c"
    printf 'Wine builtin DLL\0%015d' 0 | tr 0 '\0' | dd of="$dll" bs=1 seek=64 conv=notrunc status=none
fi
((run)) || exit 0

exe=$CARGO_TARGET_DIR/$target/$profile/victory.exe
[[ -f $exe ]] || die "$exe is not built"

title=
for dir in ${VICTORY_GAME_DIR:+"$VICTORY_GAME_DIR"} "$root"/../*/; do
    for f in "${dir%/}"/*scarlett*.exe; do
        [[ -f $f ]] && { title=$(realpath "$f"); break 2; }
    done
done
[[ $title ]] || die "no title executable found; set VICTORY_GAME_DIR"

proton=${PROTON_DIR:-}
if [[ -z $proton ]]; then
    for lib in "$HOME/.local/share/Steam" "$HOME/.steam/root"; do
        [[ -x "$lib/steamapps/common/Proton - Experimental/proton" ]] &&
            { proton="$lib/steamapps/common/Proton - Experimental"; break; }
    done
fi
[[ -x $proton/proton ]] || die "no Proton found; set PROTON_DIR"

mkdir -p "$home"/{prefix,cache,data,logs,vkd3d-cache,driver-cache}
win() { printf 'Z:%s' "${1//\//\\}"; }
# Proton's script makes the prefix; the title is then started by Proton's Wine
# itself, because the script's launcher drops the program's console output.
export WINEPREFIX=$home/prefix/pfx
wine=$proton/files/bin
if [[ ! -f $home/prefix/ready || $(<"$home/prefix/ready") != "$(<"$proton/version")" ]]; then
    STEAM_COMPAT_DATA_PATH=$home/prefix STEAM_COMPAT_CLIENT_INSTALL_PATH=$HOME/.local/share/Steam \
        SteamAppId=0 SteamGameId=0 "$proton/proton" waitforexitandrun cmd /c exit
    "$wine/wineserver" -k 2>/dev/null || true
    cp "$proton/version" "$home/prefix/ready"
fi
export NO_COLOR=1 DXVK_LOG_LEVEL=${DXVK_LOG_LEVEL:-warn}
export WINEDEBUG=${WINEDEBUG--all}
export WINEDLLOVERRIDES="dxgi,d3d12,d3d12core=n;mscoree,mshtml=${WINEDLLOVERRIDES:+;$WINEDLLOVERRIDES}"
export LD_LIBRARY_PATH="$proton/files/lib/x86_64-linux-gnu:$proton/files/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
# The driver's compiled pipelines: hundreds of compute programs take seconds
# each, and the driver's default folder is trimmed to a size they exceed.
export __GL_SHADER_DISK_CACHE=1 __GL_SHADER_DISK_CACHE_PATH=$home/driver-cache
export __GL_SHADER_DISK_CACHE_SKIP_CLEANUP=1 __GL_SHADER_DISK_CACHE_SIZE=21474836480
# Without this extension vkd3d-proton bounds root constant buffers; with it
# they are raw addresses, and a read past a buffer's end is a page fault of
# the card, which ends the run some minutes in.
export VKD3D_DISABLE_EXTENSIONS=${VKD3D_DISABLE_EXTENSIONS-VK_EXT_descriptor_heap}
export VKD3D_SHADER_CACHE_PATH=$home/vkd3d-cache
export VICTORY_CACHE_DIR=${VICTORY_CACHE_DIR:-$(win "$home/cache")}
export VICTORY_DATA_DIR=${VICTORY_DATA_DIR:-$(win "$home/data")}
export WINEDLLPATH=$native${WINEDLLPATH:+:$WINEDLLPATH}
export VICTORY_NATIVE_LIBRARY=$(win "$native/x86_64-windows/victory_native.dll")
export VICTORY_GAME_DIR=$(win "$(dirname "$title")")

# A vkd3d-proton of one's own (native/build-vkd3d.sh), loaded from beside the program.
for f in d3d12 d3d12core; do
    [[ -f $home/vkd3d/$f.dll ]] && cp -u "$home/vkd3d/$f.dll" "$(dirname "$exe")/"
done

default() { [[ -v $1 ]] || export "$1=$2"; }
if ((fast)); then
    default VICTORY_ASYNC_SUBMIT 1
    default VICTORY_QUEUE2 1
    default VICTORY_INDIRECT_DIRECT 1
    default VICTORY_TEXTURE_NO_WAIT 1
    default VICTORY_COPY_PLAIN 1
    default VICTORY_BACK_OWN_VIEW 1
    default VICTORY_WIDE_REFRESH 1
    default VICTORY_WIDE_MERGE 1
    default VICTORY_REFRESH_ONCE 1
    default VICTORY_VIEW_KNOWN 1
    default VICTORY_JOIN_COPIES 1
    default VICTORY_CARD_REHASH_RUNS 1
    default VICTORY_CARD_HASH_WIDE 1
    default VICTORY_MERGE_GAP 64
    default VICTORY_MERGE_RUNS 4096
    # More lists in the air before the flights are settled (256 settled
    # about once a frame while walking, holding the submit lock meanwhile).
    default VICTORY_LATE_WAIT_LISTS 512
    # A walk of a ray-tracing structure nothing has built may never end: the
    # card is taken from the process after some seconds of it.
    default VICTORY_SHADER_LOOP_LIMIT 16384
fi

stamp=$(date +%Y%m%d-%H%M%S)
since=$(date '+%Y-%m-%d %H:%M:%S')
log=$home/logs/$stamp.log
mkdir -p "$home/logs" "$home/traces" "$home/crashes"
ln -sfn "$log" "$home/logs/latest.log"
echo "log            $log"
trace=$home/traces/$stamp.json.gz
if ((tracing)); then
    default VICTORY_TRACE "$(win "$trace")"
    ln -sfn "$trace" "$home/traces/latest.json.gz"
    echo "frame graph    $trace"
fi
# vkd3d reports the address of a GPU fault and the allocation it hit, into
# a file of its own (under Wine its log goes to WINEDEBUG, which is off).
default VKD3D_CONFIG fault
vkd3d_log=$home/logs/$stamp.vkd3d.log
default VKD3D_LOG_FILE "$(win "$vkd3d_log")"

# The frame graph is written a gzip member a second, the array left open;
# one member reads everywhere, and the array is closed for strict readers.
repack() {
    [[ -s $1 ]] || return 0
    { gzip -dc "$1" 2>/dev/null | sed '$ s/,$//'; echo ']'; } | gzip -6 > "$2.part" && mv "$2.part" "$2"
}
# What a crash leaves, in one archive (once a run).
bundle() {
    local why=$1 dir=$home/crashes/$stamp
    mkdir "$dir" 2>/dev/null || return 0
    printf 'why: %s\nlog: %s\ntrace: %s\nwhen: %s\n' "$why" "$log" "$trace" "$(date)" > "$dir/summary.txt"
    journalctl -k --since "$since" 2>/dev/null | grep -E 'NVRM|Xid' > "$dir/kernel.txt" || true
    [[ -s $vkd3d_log ]] && cp "$vkd3d_log" "$dir/vkd3d.log"
    grep -h -n -A80 'Reporting faults' "$log" "$vkd3d_log" > "$dir/vkd3d-fault.txt" 2>/dev/null || true
    grep -nE 'WARN|ERROR|err:|Received fault|DEVICE_LOST|device removed|panicked|no frame begun|Breakpad' "$log" 2>/dev/null \
        | grep -v 'texture picked by index changed' | tail -n 500 > "$dir/errors.txt" || true
    tail -n 4000 "$log" > "$dir/log-tail.txt" 2>/dev/null || true
    find "$WINEPREFIX" "$home/data" -newermt "$since" \( -iname '*.dmp' -o -iname '*.mdmp' \) -size -512M 2>/dev/null \
        | while read -r f; do cp "$f" "$dir/"; done
    ((tracing)) && repack "$trace" "$dir/trace.json.gz"
    tar -czf "$dir.tar.gz" -C "$home/crashes" "$stamp" && rm -rf "$dir"
    echo "crash bundle   $dir.tar.gz ($why)" >&2
}
# Watches the run: the first GPU fault, stall, device loss or title crash
# makes the bundle, a few seconds later so the reports are in the log.
running=$home/crashes/.running-$stamp
touch "$running"
watch_crash() {
    local why=
    while [[ -e $running && -z $why ]]; do
        sleep 3
        why=$(journalctl -k --since "$since" 2>/dev/null | grep -m1 'NVRM: Xid' || true)
        [[ -n $why ]] || why=$(grep -m1 -E 'no frame begun after|BacktraceBreakpad|DEVICE_LOST|device removed|panicked at|Received fault in address' "$log" 2>/dev/null || true)
    done
    [[ -n $why ]] || return 0
    sleep 15
    bundle "${why:0:300}"
}
watch_crash &
command=("$wine/wine" "$exe" run "$(win "$title")" --execute "$@")
((limit)) && command=(timeout --kill-after=20 "$limit" "${command[@]}")
cd "$home"
# Nothing of the prefix outlives the run: a left-over process keeps the title's
# memory and the log open.
# (Not while a later run of the same prefix is going: its marker says so.)
stop() {
    rm -f "$running"
    compgen -G "$home/crashes/.running-*" >/dev/null || "$wine/wineserver" -k 2>/dev/null || true
}
trap stop EXIT
trap 'exit 130' INT TERM
status=0
"${command[@]}" > >(tee "$log") 2>&1 || status=$?
rm -f "$running"
wait 2>/dev/null || true
if ((status != 0 && status != 124 && status != 130)); then
    xid=$(journalctl -k --since "$since" 2>/dev/null | grep -m1 'NVRM: Xid' || true)
    bundle "the title exited with status $status${xid:+; first GPU fault: ${xid:0:240}}"
fi
((tracing)) && repack "$trace" "$trace"
exit "$status"
