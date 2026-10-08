#!/usr/bin/env bash
# Builds ggml's test-backend-ops against the patched ggml of build.sh (run that first), as a
# page that runs it in a worker: each case on WebGPU is checked against ggml's CPU backend.
# Open it in the browsers that matter, Safari above all (it takes the vec4 kernels):
#   scripts/whisper-web/test-kernels.sh && npx http-server -p 8799 "$VW_CACHE/kerneltest/www"
# then http://localhost:8799/?preset=mm (Q8_0 and F16 weights), mmall or whisper.
set -euo pipefail

LLAMA_COMMIT=d7a695ef6 # test-backend-ops.cpp from the day whisper.cpp synced ggml

HERE="$(cd "$(dirname "$0")" && pwd)"
CACHE="${VW_CACHE:-$HOME/.cache/vector-web-whisper}"
KT="$CACHE/kerneltest"
[ -d "$CACHE/src/ggml" ] || { echo "[kernel-tests] run build.sh first" >&2; exit 1; }
# shellcheck disable=SC1091
source "$CACHE/emsdk/emsdk_env.sh" >/dev/null 2>&1

if [ ! -d "$CACHE/llama.cpp" ]; then
    git clone -q --filter=blob:none --no-checkout https://github.com/ggml-org/llama.cpp "$CACHE/llama.cpp"
fi
mkdir -p "$KT/www"
git -C "$CACHE/llama.cpp" show "$LLAMA_COMMIT:tests/test-backend-ops.cpp" > "$KT/test-backend-ops.cpp"

cat > "$KT/CMakeLists.txt" <<'EOF'
cmake_minimum_required(VERSION 3.13)
project(kerneltest CXX C)
set(GGML_DIR "" CACHE PATH "patched ggml")
set(GGML_WEBGPU ON CACHE BOOL "" FORCE)
set(GGML_WEBGPU_JSPI OFF CACHE BOOL "" FORCE)
set(GGML_OPENMP OFF CACHE BOOL "" FORCE)
set(CMAKE_C_FLAGS "${CMAKE_C_FLAGS} -msimd128")
set(CMAKE_CXX_FLAGS "${CMAKE_CXX_FLAGS} -msimd128")
add_subdirectory(${GGML_DIR} ggml EXCLUDE_FROM_ALL)
add_executable(test-backend-ops test-backend-ops.cpp)
target_compile_features(test-backend-ops PRIVATE cxx_std_17)
target_link_libraries(test-backend-ops PRIVATE ggml)
target_link_options(test-backend-ops PRIVATE -sMODULARIZE=1 -sEXPORT_ES6=1 -sENVIRONMENT=web,worker
    -sALLOW_MEMORY_GROWTH=1 -sMAXIMUM_MEMORY=4GB -sSTACK_SIZE=8MB -sASYNCIFY_STACK_SIZE=4194304
    -sINVOKE_RUN=0 "-sEXPORTED_RUNTIME_METHODS=['callMain']")
EOF

emcmake cmake -S "$KT" -B "$KT/build" -G Ninja -DCMAKE_BUILD_TYPE=Release -DGGML_DIR="$CACHE/src/ggml" \
    -DEMDAWNWEBGPU_DIR="$(ls -d "$CACHE"/emdawnwebgpu-* | head -1)" >/dev/null
cmake --build "$KT/build" --target test-backend-ops -j "$(sysctl -n hw.ncpu 2>/dev/null || nproc)" >/dev/null
cp "$KT/build/test-backend-ops.js" "$KT/build/test-backend-ops.wasm" "$KT/www/"

cat > "$KT/www/worker.js" <<'EOF'
import factory from './test-backend-ops.js';
onmessage = async ({ data }) => {
    const M = await factory({ print: postMessage, printErr: postMessage, onExit: (code) => postMessage(`EXIT ${code}`) });
    M.callMain(data);
};
EOF
cat > "$KT/www/index.html" <<'EOF'
<!doctype html><meta charset="utf-8"><title>ggml WebGPU kernels</title>
<pre id="summary"></pre><pre id="out"></pre>
<script>
const PRESETS = {
    mm: ['test', '-o', 'MUL_MAT', '-p', 'type_a=(q8_0|f16),type_b=f32'],
    mmall: ['test', '-o', 'MUL_MAT'],
    whisper: ['test', '-o', 'MUL_MAT,SOFT_MAX,NORM,UNARY,ADD,MUL,CPY,CONT,GET_ROWS,IM2COL,SCALE'],
};
const args = PRESETS[new URLSearchParams(location.search).get('preset') || 'mm'];
const out = document.getElementById('out');
const summary = document.getElementById('summary');
let ok = 0, fail = 0;
const w = new Worker('worker.js', { type: 'module' });
w.onmessage = ({ data }) => {
    const line = String(data).replace(/\x1b\[[0-9;]*m/g, '');
    if (/: OK$/.test(line)) ok++;
    if (/FAIL/.test(line)) fail++;
    out.textContent += line + '\n';
    summary.textContent = `${args.join(' ')}\n${ok} OK, ${fail} FAIL${/^EXIT/.test(line) ? ' — finished' : ''}`;
};
w.postMessage(args);
</script>
EOF
echo "[kernel-tests] $KT/www"
