// Vector Web's binding to whisper.cpp: one context with its state, plain C exports.
#include "whisper.h"
#include "ggml-backend.h"

#include <emscripten/emscripten.h>

// The model streams from the worker's file handle, so its weights never sit in wasm memory
// on the GPU build.
EM_JS(size_t, vw_js_read, (void * out, size_t n), { return Module.vwRead(HEAPU8, out, n); });
EM_JS(int, vw_js_eof, (), { return Module.vwEof() ? 1 : 0; });
EM_JS(void, vw_js_log, (int level, const char * text), { Module.vwLog && Module.vwLog(level, UTF8ToString(text)); });

static whisper_context * g_ctx = nullptr;

static size_t loader_read(void *, void * output, size_t read_size) { return vw_js_read(output, read_size); }
static bool loader_eof(void *) { return vw_js_eof() != 0; }
static void loader_close(void *) {}

static void on_log(ggml_log_level level, const char * text, void *) { vw_js_log((int) level, text); }

extern "C" {

EMSCRIPTEN_KEEPALIVE int vw_init(int use_gpu, int flash_attn) {
    whisper_log_set(on_log, nullptr);
    if (g_ctx) { whisper_free(g_ctx); g_ctx = nullptr; }
    whisper_model_loader loader = { nullptr, loader_read, loader_eof, loader_close };
    whisper_context_params cparams = whisper_context_default_params();
    cparams.use_gpu = use_gpu != 0;
    cparams.flash_attn = flash_attn != 0;
    g_ctx = whisper_init_with_params(&loader, cparams);
    return g_ctx ? 1 : 0;
}

/// GPU devices ggml registered (WebGPU with shader-f16); 0 means it runs on the CPU.
EMSCRIPTEN_KEEPALIVE int vw_gpu_devices(void) {
    int n = 0;
    for (size_t i = 0; i < ggml_backend_dev_count(); ++i) {
        enum ggml_backend_dev_type t = ggml_backend_dev_type(ggml_backend_dev_get(i));
        if (t == GGML_BACKEND_DEVICE_TYPE_GPU || t == GGML_BACKEND_DEVICE_TYPE_IGPU) n++;
    }
    return n;
}

/// One pass over 16 kHz mono `pcm`: greedy when `beam` is 0, else beam search of that width.
/// `audio_ctx` above 0 encodes only that many frames, which only ACFT-tuned models tolerate.
EMSCRIPTEN_KEEPALIVE int vw_full(const float * pcm, int n_samples, int beam, float temperature, int translate, int n_threads, int audio_ctx) {
    if (!g_ctx) return -1;
    // whisper_full keeps the last mel when given no samples: never transcribe that again.
    if (n_samples <= 0) return -2;
    whisper_full_params p = whisper_full_default_params(beam > 0 ? WHISPER_SAMPLING_BEAM_SEARCH : WHISPER_SAMPLING_GREEDY);
    if (beam > 0) {
        p.beam_search.beam_size = beam;
        p.beam_search.patience = 1.0f;
        p.temperature = temperature;
        p.temperature_inc = 0.0f;
    } else {
        p.greedy.best_of = 1;
    }
    p.print_realtime = false;
    p.print_progress = false;
    p.print_timestamps = true;
    p.language = "auto";
    p.token_timestamps = true;
    p.max_len = 30;
    p.split_on_word = true;
    p.translate = translate != 0;
    p.suppress_nst = true;
    p.no_context = true;
    p.single_segment = n_samples < 16000 * 5;
    p.n_threads = n_threads > 0 ? n_threads : 1;
    if (audio_ctx > 0) {
        p.audio_ctx = audio_ctx;
        p.suppress_blank = false;
    }
    whisper_reset_timings(g_ctx);
    return whisper_full(g_ctx, p, pcm, n_samples);
}

EMSCRIPTEN_KEEPALIVE int vw_lang_id(void) { return whisper_full_lang_id(g_ctx); }
EMSCRIPTEN_KEEPALIVE const char * vw_lang_str(int id) { return whisper_lang_str(id); }
EMSCRIPTEN_KEEPALIVE int vw_n_segments(void) { return whisper_full_n_segments(g_ctx); }
EMSCRIPTEN_KEEPALIVE const char * vw_segment_text(int i) { return whisper_full_get_segment_text(g_ctx, i); }
EMSCRIPTEN_KEEPALIVE double vw_segment_t0(int i) { return (double) whisper_full_get_segment_t0(g_ctx, i); }

/// Mean probability over every token of segment `i`, special tokens included.
EMSCRIPTEN_KEEPALIVE float vw_segment_p(int i) {
    const int n = whisper_full_n_tokens(g_ctx, i);
    if (n <= 0) return 0.0f;
    float sum = 0.0f;
    for (int t = 0; t < n; ++t) sum += whisper_full_get_token_p(g_ctx, i, t);
    return sum / (float) n;
}

EMSCRIPTEN_KEEPALIVE void vw_print_timings(void) { if (g_ctx) whisper_print_timings(g_ctx); }

EMSCRIPTEN_KEEPALIVE void vw_release(void) {
    if (g_ctx) { whisper_free(g_ctx); g_ctx = nullptr; }
}

}
