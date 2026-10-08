// The patches' state-reuse invariants: whatever a state ran before, a transcription matches what a
// fresh state gives, token for token. Built and run by test-native.sh.
#include "whisper.h"

#include <cmath>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <string>
#include <vector>

static std::vector<float> read_wav(const char * path) {
    FILE * f = fopen(path, "rb");
    if (!f) { perror(path); exit(2); }
    std::vector<unsigned char> b;
    unsigned char buf[65536];
    size_t n;
    while ((n = fread(buf, 1, sizeof buf, f)) > 0) b.insert(b.end(), buf, buf + n);
    fclose(f);
    std::vector<float> out;
    for (size_t p = 12; p + 8 <= b.size();) {
        uint32_t sz;
        memcpy(&sz, &b[p + 4], 4);
        if (!memcmp(&b[p], "data", 4)) {
            for (size_t i = 0; i + 1 < sz && p + 8 + i + 1 < b.size(); i += 2) {
                int16_t s;
                memcpy(&s, &b[p + 8 + i], 2);
                out.push_back(s / 32768.0f);
            }
            break;
        }
        p += 8 + sz + (sz & 1);
    }
    return out;
}

struct clip { std::string name; std::vector<float> pcm; };

enum mode { GREEDY, BEAM, TEMPERATURE };

// every token's id and probability: equal only if decoding saw exactly the same inputs
static std::string run(whisper_context * ctx, whisper_state * st, const clip & c, mode m, bool acft) {
    whisper_full_params p = whisper_full_default_params(m == BEAM ? WHISPER_SAMPLING_BEAM_SEARCH : WHISPER_SAMPLING_GREEDY);
    p.print_realtime = p.print_progress = p.print_timestamps = false;
    p.language = "auto";
    p.token_timestamps = true;
    p.max_len = 30;
    p.split_on_word = true;
    p.suppress_nst = true;
    p.no_context = true;
    p.n_threads = 4;
    if (m == BEAM) { p.beam_search.beam_size = 5; p.temperature_inc = 0.0f; }
    if (m == TEMPERATURE) { p.temperature = 0.6f; p.temperature_inc = 0.0f; }
    if (acft) {
        p.audio_ctx = std::min(1500, (int) ceil(c.pcm.size() / 320.0) + 32);
        p.suppress_blank = false;
    }
    if (whisper_full_with_state(ctx, st, p, c.pcm.data(), (int) c.pcm.size()) != 0) return "FAILED";
    std::string out = whisper_lang_str(whisper_full_lang_id_from_state(st));
    char tok[64];
    for (int i = 0; i < whisper_full_n_segments_from_state(st); i++) {
        out += " |";
        for (int j = 0; j < whisper_full_n_tokens_from_state(st, i); j++) {
            const whisper_token_data d = whisper_full_get_token_data_from_state(st, i, j);
            snprintf(tok, sizeof tok, " %d:%.7f", d.id, d.p);
            out += tok;
        }
    }
    return out;
}

int main(int argc, char ** argv) {
    if (argc < 4) {
        fprintf(stderr, "usage: test-native model.bin [-acft] a.wav b.wav [more.wav...]\n");
        return 2;
    }
    whisper_log_set([](enum ggml_log_level, const char *, void *) {}, nullptr);
    bool acft = false;
    std::vector<clip> clips;
    for (int i = 2; i < argc; i++) {
        if (!strcmp(argv[i], "-acft")) { acft = true; continue; }
        clips.push_back({ strrchr(argv[i], '/') ? strrchr(argv[i], '/') + 1 : argv[i], read_wav(argv[i]) });
    }
    // over 30 s, so whisper_full seeks through more than one window
    clip all { "all-joined", {} };
    for (const auto & c : clips) all.pcm.insert(all.pcm.end(), c.pcm.begin(), c.pcm.end());
    while (all.pcm.size() < 16000 * 40) all.pcm.insert(all.pcm.end(), clips[0].pcm.begin(), clips[0].pcm.end());
    clips.push_back(all);
    // one sample a ULP away: a cache keyed on the audio must not take it for the original
    clip nudged = clips[0];
    nudged.name += "+1ulp";
    nudged.pcm[nudged.pcm.size() / 2] = nextafterf(nudged.pcm[nudged.pcm.size() / 2], 1.0f);
    clips.push_back(nudged);

    whisper_context_params cp = whisper_context_default_params();
    cp.use_gpu = getenv("TEST_GPU") != nullptr;
    cp.flash_attn = true;
    whisper_context * ctx = whisper_init_from_file_with_params_no_state(argv[1], cp);
    if (!ctx) { fprintf(stderr, "cannot load %s\n", argv[1]); return 2; }

    const mode modes[] = { GREEDY, BEAM, TEMPERATURE };
    const char * mode_names[] = { "greedy", "beam", "temperature" };
    std::vector<std::vector<std::string>> fresh(clips.size(), std::vector<std::string>(3));
    for (size_t i = 0; i < clips.size(); i++) {
        for (int m = 0; m < 3; m++) {
            whisper_state * st = whisper_init_state(ctx);
            fresh[i][m] = run(ctx, st, clips[i], modes[m], acft);
            whisper_free_state(st);
        }
    }

    int failures = 0;
    auto check = [&](whisper_state * st, size_t i, int m, const char * how) {
        const std::string got = run(ctx, st, clips[i], modes[m], acft);
        const bool ok = got == fresh[i][m] && got != "FAILED";
        printf("%s %-16s %-11s %s\n", ok ? "PASS" : "FAIL", clips[i].name.c_str(), mode_names[m], how);
        failures += !ok;
    };

    // one state through every clip twice over, then the retries Vector makes on a single state
    whisper_state * st = whisper_init_state(ctx);
    for (int round = 0; round < 2; round++) {
        for (size_t i = 0; i < clips.size(); i++) {
            check(st, i, GREEDY, round ? "reused, second round" : "reused");
        }
    }
    for (size_t i = 0; i < clips.size(); i++) {
        check(st, i, GREEDY, "before its retries");
        check(st, i, BEAM, "retry on the same state");
        check(st, i, TEMPERATURE, "second retry");
    }
    whisper_free_state(st);
    whisper_free(ctx);

    printf("%d failure(s)\n", failures);
    return failures ? 1 : 0;
}
