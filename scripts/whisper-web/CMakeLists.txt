cmake_minimum_required(VERSION 3.13)
project(vector_whisper CXX C)

set(WHISPER_DIR "" CACHE PATH "whisper.cpp source, with patches/ applied")
option(VW_THREADS "CPU build on pthreads; otherwise the WebGPU build" OFF)

set(WHISPER_BUILD_EXAMPLES OFF CACHE BOOL "" FORCE)
set(WHISPER_BUILD_TESTS OFF CACHE BOOL "" FORCE)
set(WHISPER_BUILD_SERVER OFF CACHE BOOL "" FORCE)
set(WHISPER_WASM_PTHREAD ${VW_THREADS} CACHE BOOL "" FORCE)
set(GGML_OPENMP OFF CACHE BOOL "" FORCE)
set(GGML_WEBGPU_JSPI OFF CACHE BOOL "" FORCE)
if (NOT VW_THREADS)
    set(GGML_WEBGPU ON CACHE BOOL "" FORCE)
endif()
set(CMAKE_C_FLAGS "${CMAKE_C_FLAGS} -msimd128")
set(CMAKE_CXX_FLAGS "${CMAKE_CXX_FLAGS} -msimd128")
add_subdirectory(${WHISPER_DIR} whisper EXCLUDE_FROM_ALL)

add_executable(vector_whisper vector_whisper.cpp)
target_link_libraries(vector_whisper PRIVATE whisper)

set(VW_LINK
    -sMODULARIZE=1
    -sEXPORT_ES6=1
    -sEXPORT_NAME=VectorWhisper
    -sENVIRONMENT=worker
    -sALLOW_MEMORY_GROWTH=1
    -sINITIAL_MEMORY=64MB
    -sSTACK_SIZE=5MB
    -sFILESYSTEM=0
    "-sEXPORTED_RUNTIME_METHODS=['ccall','UTF8ToString','HEAPU8']"
    "-sEXPORTED_FUNCTIONS=['_malloc','_free']"
)
if (VW_THREADS)
    # Shared memory reserves its maximum up front: keep it to what a small model needs.
    list(APPEND VW_LINK -pthread -sPTHREAD_POOL_SIZE=Module.vwThreads -sMAXIMUM_MEMORY=2GB)
else()
    # Only the paths that wait on the GPU unwind; any other indirect call is assumed not to,
    # which keeps the per-op encoding loop out of Asyncify's instrumentation.
    list(APPEND VW_LINK -sMAXIMUM_MEMORY=4GB -sASYNCIFY_STACK_SIZE=1048576 -sASYNCIFY_IGNORE_INDIRECT=1
        "-sASYNCIFY_ADD=['ggml_backend_*','ggml_graph_compute_helper*','whisper_*','ggml_gallocr_*','vw_*']")
endif()
target_link_options(vector_whisper PRIVATE ${VW_LINK})
