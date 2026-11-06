#pragma once
#include <stdint.h>
#include <stdbool.h>
#include <stdatomic.h>
#include <string.h>
#include <stdlib.h>

// Generic, timestamp-addressed loopback. HAL supplies the fully mixed output.
// Independent input readers never consume data from each other. Every sample
// access is atomic: concurrent wraparound cannot create a C data race.
#define BRIDGE_MAX_CHANNELS 32
#define BRIDGE_RING_FRAMES 32768
#define BRIDGE_LATENCY_FRAMES 1024
_Static_assert(ATOMIC_INT_LOCK_FREE == 2 && ATOMIC_LLONG_LOCK_FREE == 2, "Lock-free audio required");
typedef struct {
    _Atomic(unsigned long long) sequence, frame, epoch;
    _Atomic(unsigned int) samples[BRIDGE_MAX_CHANNELS];
} BridgeSlot;
typedef struct { BridgeSlot slots[BRIDGE_RING_FRAMES]; } BridgeRing;

static BridgeRing *bridge_ring_create(void) {
    BridgeRing *r = malloc(sizeof(*r));
    if (!r) return NULL;
    for (uint32_t f = 0; f < BRIDGE_RING_FRAMES; f++) {
        atomic_init(&r->slots[f].sequence, 0);
        atomic_init(&r->slots[f].frame, UINT64_MAX);
        atomic_init(&r->slots[f].epoch, UINT64_MAX);
        for (uint32_t c = 0; c < BRIDGE_MAX_CHANNELS; c++) atomic_init(&r->slots[f].samples[c], 0);
    }
    return r;
}
static void bridge_ring_write(BridgeRing *r, uint64_t epoch, uint64_t frame,
                              uint32_t channels, uint32_t frames, const float *input) {
    for (uint32_t f = 0; f < frames; f++) {
        BridgeSlot *s = &r->slots[(frame + f) % BRIDGE_RING_FRAMES];
        atomic_fetch_add(&s->sequence, 1); // odd while being replaced
        atomic_store(&s->frame, frame + f); atomic_store(&s->epoch, epoch);
        for (uint32_t c = 0; c < channels; c++) {
            uint32_t bits; memcpy(&bits, input + f * channels + c, sizeof(bits));
            atomic_store(&s->samples[c], bits);
        }
        atomic_fetch_add(&s->sequence, 1);
    }
}
static uint32_t bridge_ring_read(BridgeRing *r, uint64_t epoch, int64_t frame,
                                uint32_t channels, uint32_t frames, float *output) {
    uint32_t missing = 0;
    for (uint32_t f = 0; f < frames; f++) {
        int64_t position = frame + f;
        bool valid = position >= 0;
        if (valid) {
            BridgeSlot *s = &r->slots[(uint64_t)position % BRIDGE_RING_FRAMES];
            uint64_t sequence = atomic_load(&s->sequence);
            valid = !(sequence & 1) && atomic_load(&s->frame) == (uint64_t)position && atomic_load(&s->epoch) == epoch;
            if (valid) for (uint32_t c = 0; c < channels; c++) {
                uint32_t bits = atomic_load(&s->samples[c]);
                memcpy(output + f * channels + c, &bits, sizeof(bits));
            }
            valid = valid && sequence == atomic_load(&s->sequence);
        }
        if (!valid) { memset(output + f * channels, 0, channels * sizeof(float)); missing++; }
    }
    return missing;
}
