#include "RealtimeAudio.h"
#include <stdatomic.h>
#include <stdlib.h>
#include <string.h>
#include <math.h>
#include <sched.h>

_Static_assert(ATOMIC_INT_LOCK_FREE == 2 && ATOMIC_LLONG_LOCK_FREE == 2 && ATOMIC_BOOL_LOCK_FREE == 2,
               "Audio callback requires lock-free atomics");

struct PlayerRenderer {
    _Atomic(uint32_t) gainBits;
    _Atomic(uint32_t) peakBits;
    _Atomic(unsigned long long) callbacks, frames, nonzeroFrames, invalidBuffers;
    float currentGain, rampStep;
    bool renderOutput;
    _Atomic(unsigned int) references;
    _Atomic(bool) recording, recordBusy;
    _Atomic(unsigned long long) recordWrite, recordRead, recordDropped;
    uint32_t recordCapacity;
    float *recordSamples;
};
static uint32_t bits(float f) { uint32_t n; memcpy(&n, &f, sizeof(n)); return n; }
static float number(uint32_t n) { float f; memcpy(&f, &n, sizeof(f)); return f; }

PlayerRenderer *player_renderer_create(double sampleRate, bool renderOutput) {
    if (!isfinite(sampleRate) || sampleRate < 8000 || sampleRate > 384000) return NULL;
    PlayerRenderer *r = calloc(1, sizeof(*r));
    if (!r) return NULL;
    atomic_init(&r->gainBits, bits(0.5f));
    atomic_init(&r->peakBits, bits(0));
    atomic_init(&r->callbacks, 0); atomic_init(&r->frames, 0);
    atomic_init(&r->nonzeroFrames, 0); atomic_init(&r->invalidBuffers, 0);
    r->rampStep = (float)(1.0 / (sampleRate * 0.005));
    r->renderOutput = renderOutput;
    atomic_init(&r->references, 1);
    atomic_init(&r->recording, false); atomic_init(&r->recordBusy, false);
    atomic_init(&r->recordWrite, 0); atomic_init(&r->recordRead, 0); atomic_init(&r->recordDropped, 0);
    r->recordCapacity = 4096;
    while (r->recordCapacity < sampleRate * 4) r->recordCapacity *= 2;
    r->recordSamples = malloc((size_t)r->recordCapacity * 2 * sizeof(float));
    if (!r->recordSamples) { free(r); return NULL; }
    // Touch storage before I/O; allocation and initial page faults stay out of the callback.
    volatile float *pages = r->recordSamples;
    for (size_t i = 0; i < (size_t)r->recordCapacity * 2; i++) pages[i] = 0;
    return r;
}
void player_renderer_retain(PlayerRenderer *r) { atomic_fetch_add(&r->references, 1); }
void player_renderer_destroy(PlayerRenderer *r) {
    if (atomic_fetch_sub(&r->references, 1) == 1) { free(r->recordSamples); free(r); }
}
void player_record_stop(PlayerRenderer *r) {
    atomic_store(&r->recording, false);
    while (atomic_load(&r->recordBusy)) sched_yield();
}
bool player_record_start(PlayerRenderer *r) {
    if (!r->renderOutput || atomic_load(&r->recording)) return false;
    player_record_stop(r);
    atomic_store(&r->recordRead, atomic_load(&r->recordWrite));
    atomic_store(&r->recordDropped, 0);
    atomic_store(&r->recording, true);
    return true;
}
uint64_t player_record_written(PlayerRenderer *r) { return atomic_load_explicit(&r->recordWrite, memory_order_acquire); }
uint64_t player_record_dropped(PlayerRenderer *r) { return atomic_load(&r->recordDropped); }
uint32_t player_record_read(PlayerRenderer *r, float *stereo, uint32_t maxFrames, uint64_t throughFrame) {
    uint64_t read = atomic_load_explicit(&r->recordRead, memory_order_relaxed);
    uint64_t write = player_record_written(r);
    if (write > throughFrame) write = throughFrame;
    if (write <= read) return 0;
    uint32_t count = (uint32_t)fmin((double)(write - read), maxFrames);
    for (uint32_t f = 0; f < count; f++) {
        uint32_t index = (uint32_t)(read + f) & (r->recordCapacity - 1);
        stereo[f * 2] = r->recordSamples[index * 2];
        stereo[f * 2 + 1] = r->recordSamples[index * 2 + 1];
    }
    atomic_store_explicit(&r->recordRead, read + count, memory_order_release);
    return count;
}
void player_renderer_set_gain(PlayerRenderer *r, float gain) {
    if (!isfinite(gain)) gain = 0;
    atomic_store_explicit(&r->gainBits, bits(fmaxf(0, fminf(1, gain))), memory_order_relaxed);
}
PlayerRenderStats player_renderer_stats(PlayerRenderer *r) {
    return (PlayerRenderStats){
        atomic_load_explicit(&r->callbacks, memory_order_relaxed),
        atomic_load_explicit(&r->frames, memory_order_relaxed),
        atomic_load_explicit(&r->nonzeroFrames, memory_order_relaxed),
        atomic_load_explicit(&r->invalidBuffers, memory_order_relaxed),
        number(atomic_load_explicit(&r->peakBits, memory_order_relaxed))
    };
}

typedef struct { float *data; uint32_t stride, frames; } Channel;
static bool stereo(const AudioBufferList *list, Channel channels[2]) {
    if (!list || list->mNumberBuffers < 1 || list->mNumberBuffers > 2) return false;
    uint32_t channel = 0;
    for (uint32_t b = 0; b < list->mNumberBuffers; b++) {
        const AudioBuffer *buffer = &list->mBuffers[b];
        uint32_t n = buffer->mNumberChannels;
        if (!buffer->mData || !n || n > 2 || channel + n > 2 ||
            buffer->mDataByteSize % (n * sizeof(float))) return false;
        for (uint32_t c = 0; c < n; c++) {
            channels[channel++] = (Channel){(float *)buffer->mData + c, n,
                buffer->mDataByteSize / (n * sizeof(float))};
        }
    }
    return channel == 2 && channels[0].frames == channels[1].frames;
}

static void record_output(PlayerRenderer *r, Channel destination[2]) {
    atomic_store(&r->recordBusy, true);
    if (atomic_load(&r->recording)) {
        uint64_t write = atomic_load_explicit(&r->recordWrite, memory_order_relaxed);
        uint64_t read = atomic_load_explicit(&r->recordRead, memory_order_acquire);
        uint32_t count = destination[0].frames;
        if (count > r->recordCapacity - (write - read)) {
            atomic_fetch_add(&r->recordDropped, count);
            atomic_store(&r->recording, false);
        } else {
            for (uint32_t f = 0; f < count; f++) {
                uint32_t index = (uint32_t)(write + f) & (r->recordCapacity - 1);
                r->recordSamples[index * 2] = destination[0].data[f * destination[0].stride];
                r->recordSamples[index * 2 + 1] = destination[1].data[f * destination[1].stride];
            }
            atomic_store_explicit(&r->recordWrite, write + count, memory_order_release);
        }
    }
    atomic_store(&r->recordBusy, false);
}

void player_renderer_process(PlayerRenderer *r, const AudioBufferList *input, AudioBufferList *output) {
    // Silence first: a changed/unsupported layout must never pass uninitialized samples.
    if (output) for (uint32_t b = 0; b < output->mNumberBuffers; b++) {
        if (output->mBuffers[b].mData)
            memset(output->mBuffers[b].mData, 0, output->mBuffers[b].mDataByteSize);
    }
    atomic_fetch_add_explicit(&r->callbacks, 1, memory_order_relaxed);
    Channel source[2], destination[2];
    if (!stereo(input, source) || !stereo(output, destination) ||
        source[0].frames != destination[0].frames) {
        atomic_fetch_add_explicit(&r->invalidBuffers, 1, memory_order_relaxed);
        if (atomic_exchange(&r->recording, false)) atomic_fetch_add(&r->recordDropped, 1);
        return;
    }
    float target = number(atomic_load_explicit(&r->gainBits, memory_order_relaxed));
    float peak = 0;
    uint64_t nonzero = 0;
    for (uint32_t frame = 0; frame < source[0].frames; frame++) {
        float delta = target - r->currentGain;
        r->currentGain += fmaxf(-r->rampStep, fminf(r->rampStep, delta));
        bool audible = false;
        for (uint32_t c = 0; c < 2; c++) {
            float sample = source[c].data[frame * source[c].stride];
            if (!isfinite(sample)) sample = 0;
            peak = fmaxf(peak, fabsf(sample));
            audible |= fabsf(sample) > 0.00001f;
            if (r->renderOutput) destination[c].data[frame * destination[c].stride] = sample * r->currentGain;
        }
        nonzero += audible;
    }
    atomic_store_explicit(&r->peakBits, bits(peak), memory_order_relaxed);
    atomic_fetch_add_explicit(&r->frames, source[0].frames, memory_order_relaxed);
    atomic_fetch_add_explicit(&r->nonzeroFrames, nonzero, memory_order_relaxed);
    record_output(r, destination);
}
OSStatus player_audio_callback(AudioObjectID device, const AudioTimeStamp *now,
    const AudioBufferList *input, const AudioTimeStamp *inputTime,
    AudioBufferList *output, const AudioTimeStamp *outputTime, void *context) {
    player_renderer_process(context, input, output);
    return noErr;
}

void player_renderer_capture(PlayerRenderer *r, const AudioBufferList *input) {
    atomic_fetch_add_explicit(&r->callbacks, 1, memory_order_relaxed);
    Channel source[2];
    if (!stereo(input, source)) {
        atomic_fetch_add(&r->invalidBuffers, 1);
        if (atomic_exchange(&r->recording, false)) atomic_fetch_add(&r->recordDropped, 1);
        return;
    }
    float peak = 0; uint64_t nonzero = 0;
    for (uint32_t f = 0; f < source[0].frames; f++) {
        bool signal = false;
        for (uint32_t c = 0; c < 2; c++) {
            float sample = source[c].data[f * source[c].stride];
            if (isfinite(sample)) { peak = fmaxf(peak, fabsf(sample)); signal |= fabsf(sample) > 0.00001f; }
        }
        nonzero += signal;
    }
    atomic_store(&r->peakBits, bits(peak));
    atomic_fetch_add(&r->frames, source[0].frames); atomic_fetch_add(&r->nonzeroFrames, nonzero);
    record_output(r, source); // raw endpoint samples, including any diagnostic non-finite values
}
