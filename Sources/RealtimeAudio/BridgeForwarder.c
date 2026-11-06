#include "RealtimeAudio.h"
#include <stdatomic.h>
#include <stdlib.h>
#include <string.h>
#include <math.h>
#include <sched.h>

#define CAPACITY 16384
#define TARGET 1024
struct BridgeForwarder {
    PlayerRenderer *recorder;
    _Atomic(bool) enabled, writing, reset;
    _Atomic(unsigned long long) write, read, underruns, overflows, ratioBits;
    _Atomic(unsigned int) gainBits, peakBits;
    _Atomic(unsigned long long) signalFrames;
    float samples[CAPACITY * 2];
    double fraction, ratio;
    float gain;
    bool primed;
};
static unsigned int bits(float value) { unsigned int b; memcpy(&b,&value,4); return b; }
static float value(unsigned int bits) { float f; memcpy(&f,&bits,4); return f; }
BridgeForwarder *bridge_forwarder_create(PlayerRenderer *recorder) {
    BridgeForwarder *r = calloc(1,sizeof(*r)); if (!r) return NULL;
    r->recorder = recorder; player_renderer_retain(recorder); r->ratio = 1;
    atomic_init(&r->enabled,false); atomic_init(&r->writing,false); atomic_init(&r->reset,false);
    atomic_init(&r->write,0); atomic_init(&r->read,0); atomic_init(&r->underruns,0); atomic_init(&r->overflows,0);
    atomic_init(&r->ratioBits,0); atomic_init(&r->gainBits,bits(0.5f));
    atomic_init(&r->peakBits,0); atomic_init(&r->signalFrames,0);
    volatile float *pages = r->samples;
    for (int i=0;i<CAPACITY*2;i++) pages[i]=0;
    return r;
}
void bridge_forwarder_destroy(BridgeForwarder *r) { player_renderer_destroy(r->recorder); free(r); }
// Called only with the monitoring render callback stopped. Input may continue.
void bridge_monitor_enable(BridgeForwarder *r, bool enabled) {
    atomic_store(&r->enabled,false);
    while (atomic_load(&r->writing)) sched_yield();
    atomic_store(&r->read,0); atomic_store(&r->write,0); atomic_store(&r->reset,false);
    r->fraction=0; r->ratio=1; r->gain=0; r->primed=false;
    atomic_store(&r->enabled,enabled);
}
void bridge_monitor_gain(BridgeForwarder *r, float gain) {
    atomic_store(&r->gainBits,bits(isfinite(gain) ? fminf(1,fmaxf(0,gain)) : 0));
}
static bool stereo(const AudioBufferList *list, float *channels[2], uint32_t strides[2], uint32_t *frames) {
    if (!list || list->mNumberBuffers < 1 || list->mNumberBuffers > 2) return false;
    uint32_t channel=0, length=0;
    for (uint32_t b=0;b<list->mNumberBuffers;b++) {
        const AudioBuffer *buffer=&list->mBuffers[b]; uint32_t count=buffer->mNumberChannels;
        if (!buffer->mData || !count || count>2 || channel+count>2 || buffer->mDataByteSize%(count*4)) return false;
        uint32_t n=buffer->mDataByteSize/(count*4);
        if (channel && n!=length) return false; length=n;
        for (uint32_t c=0;c<count;c++) { channels[channel]=(float *)buffer->mData+c; strides[channel++]=count; }
    }
    *frames=length; return channel==2;
}
OSStatus bridge_capture_callback(AudioObjectID device, const AudioTimeStamp *now,
    const AudioBufferList *input, const AudioTimeStamp *inputTime,
    AudioBufferList *output, const AudioTimeStamp *outputTime, void *context) {
    BridgeForwarder *r=context;
    // This client contributes only silence to the virtual device's output mix.
    if (output) for (UInt32 b=0;b<output->mNumberBuffers;b++) if (output->mBuffers[b].mData)
        memset(output->mBuffers[b].mData,0,output->mBuffers[b].mDataByteSize);
    player_renderer_capture(r->recorder,input);
    atomic_store(&r->writing,true);
    if (atomic_load(&r->enabled)) {
        float *channels[2]; uint32_t strides[2], frames;
        if (stereo(input,channels,strides,&frames)) {
            uint64_t write=atomic_load_explicit(&r->write,memory_order_relaxed);
            uint64_t read=atomic_load_explicit(&r->read,memory_order_acquire);
            if (frames > CAPACITY-(write-read)) { atomic_fetch_add(&r->overflows,1); atomic_store(&r->reset,true); }
            else {
                for (uint32_t f=0;f<frames;f++) for (uint32_t c=0;c<2;c++) {
                    float sample=channels[c][f*strides[c]];
                    r->samples[((write+f)%CAPACITY)*2+c]=isfinite(sample) ? sample : 0;
                }
                atomic_store_explicit(&r->write,write+frames,memory_order_release);
            }
        }
    }
    atomic_store(&r->writing,false); return noErr;
}
void bridge_monitor_render(BridgeForwarder *r, AudioBufferList *output) {
    atomic_store(&r->peakBits,0);
    for (UInt32 b=0;b<output->mNumberBuffers;b++) if (output->mBuffers[b].mData)
        memset(output->mBuffers[b].mData,0,output->mBuffers[b].mDataByteSize);
    if (!atomic_load(&r->enabled)) return;
    float *channels[2]; uint32_t strides[2], frames;
    if (!stereo(output,channels,strides,&frames)) return;
    uint64_t read=atomic_load_explicit(&r->read,memory_order_relaxed);
    uint64_t write=atomic_load_explicit(&r->write,memory_order_acquire);
    if (atomic_exchange(&r->reset,false)) { read=write; r->fraction=0; r->primed=false; }
    if (!r->primed) {
        if (write-read < TARGET) { atomic_store(&r->read,read); return; }
        r->primed=true;
    }
    // Adaptive rate correction applies ONLY to listening. WAVs use the untouched
    // virtual input clock. Keep monitoring bounded when hardware clocks differ.
    double correction=fmax(-0.002,fmin(0.002,((double)(write-read)-TARGET)*0.000001));
    r->ratio += (1+correction-r->ratio)*0.01;
    unsigned long long ratioBits; memcpy(&ratioBits,&r->ratio,8); atomic_store(&r->ratioBits,ratioBits);
    float target=value(atomic_load(&r->gainBits));
    float peak=0; uint64_t signalFrames=0;
    for (uint32_t f=0;f<frames;f++) {
        if (read+1>=write) { atomic_fetch_add(&r->underruns,1); r->primed=false; r->fraction=0; break; }
        r->gain += fmaxf(-0.004f,fminf(0.004f,target-r->gain));
        bool signal=false;
        for (uint32_t c=0;c<2;c++) {
            float a=r->samples[(read%CAPACITY)*2+c], b=r->samples[((read+1)%CAPACITY)*2+c];
            channels[c][f*strides[c]]=(a+(b-a)*(float)r->fraction)*r->gain;
            float magnitude=fabsf(channels[c][f*strides[c]]);
            peak=fmaxf(peak,magnitude); signal |= magnitude>0;
        }
        signalFrames+=signal;
        r->fraction+=r->ratio; uint32_t advance=(uint32_t)r->fraction;
        read+=advance; r->fraction-=advance;
    }
    atomic_store_explicit(&r->read,read,memory_order_release);
    atomic_fetch_add(&r->signalFrames,signalFrames); atomic_store(&r->peakBits,bits(peak));
}
BridgeMonitorStats bridge_monitor_stats(BridgeForwarder *r) {
    unsigned long long bits=atomic_load(&r->ratioBits); double ratio; memcpy(&ratio,&bits,8);
    uint64_t read=atomic_load(&r->read), write=atomic_load(&r->write);
    return (BridgeMonitorStats){atomic_load(&r->underruns),atomic_load(&r->overflows),(uint32_t)(write>=read ? write-read : 0),ratio,atomic_load(&r->signalFrames),value(atomic_load(&r->peakBits))};
}
