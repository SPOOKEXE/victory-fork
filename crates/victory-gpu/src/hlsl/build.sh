#!/usr/bin/env bash
# Compiles the HLSL beside this script to dxil/. DXC names the compiler
# (https://github.com/microsoft/DirectXShaderCompiler/releases); b3sum is needed.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"
dxc=${DXC:-dxc}
mkdir -p dxil
for f in *.hlsl; do
    n=${f%.hlsl}
    case $n in
        plane_corners) target=vs_6_0 entry=corner ;;
        plane_*) target=ps_6_0 entry=plane ;;
        *) target=cs_6_0 entry=main ;;
    esac
    "$dxc" -nologo -O3 -Qstrip_debug -Qstrip_reflect -T "$target" -E "$entry" -Fo "dxil/$n.dxil" "$f"
    tr -d '\r' <"$f" | b3sum --no-names >"dxil/$n.sum"
done
