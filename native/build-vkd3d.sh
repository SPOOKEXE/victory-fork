#!/usr/bin/env bash
# Builds the vkd3d-proton commit run.sh prefers to Proton's own: Proton's
# translator overflows its stack on four of the title's programs and compiles
# the big ones a third slower; with the allocator's counters of
# vkd3d-victory-stats.patch. Needs mingw-w64, glslang, widl, ninja and uv.
set -euo pipefail
commit=71cae09
home=${VICTORY_HOME:-$HOME/.local/share/victory}
src=${VKD3D_SOURCE:-$HOME/.cache/victory-vkd3d}
[[ -d $src/.git ]] || git clone -q https://github.com/HansKristian-Work/vkd3d-proton.git "$src"
cd "$src"
git checkout -q "$commit"
git submodule update --init --recursive -q
# The allocator's counters for the vkd3d view (F2): applied once.
patch=$(dirname "$(readlink -f "$0")")/vkd3d-victory-stats.patch
git apply --check "$patch" 2>/dev/null && git apply "$patch"
tools=$src/tools-bin
mkdir -p "$tools" "$home/vkd3d"
command -v x86_64-w64-mingw32-widl >/dev/null || ln -sf "$(command -v widl)" "$tools/x86_64-w64-mingw32-widl"
PATH=$tools:$PATH
[[ -d b64 ]] || uvx meson setup --cross-file build-win64.txt --buildtype release -Denable_tests=false b64
ninja -C b64
for f in d3d12 d3d12core; do
    x86_64-w64-mingw32-strip -o "$home/vkd3d/$f.dll" "b64/libs/$f/$f.dll"
done
echo "built into $home/vkd3d"
