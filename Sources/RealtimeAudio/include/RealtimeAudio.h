#pragma once
#include <CoreAudio/CoreAudio.h>
#include <stdbool.h>
#include <stdint.h>

typedef struct PlayerRenderer PlayerRenderer;
typedef struct {
    uint64_t callbacks;
    uint64_t frames;
    uint64_t nonzeroFrames;
    uint64_t invalidBuffers;
    float peak;
} PlayerRenderStats;

// All allocation is confined to create/destroy, outside the audio callback.
PlayerRenderer * _Nullable player_renderer_create(double sampleRate, bool renderOutput);
void player_renderer_destroy(PlayerRenderer * _Nonnull renderer);
// A recorder may outlive HAL teardown while its worker flushes queued samples.
void player_renderer_retain(PlayerRenderer * _Nonnull renderer);
bool player_record_start(PlayerRenderer * _Nonnull renderer);
// Non-real-time only: disables capture and waits for an in-flight callback copy.
void player_record_stop(PlayerRenderer * _Nonnull renderer);
uint64_t player_record_written(PlayerRenderer * _Nonnull renderer);
uint64_t player_record_dropped(PlayerRenderer * _Nonnull renderer);
uint32_t player_record_read(PlayerRenderer * _Nonnull renderer, float * _Nonnull stereo,
    uint32_t maxFrames, uint64_t throughFrame);
// Input-only capture for generic endpoint recording: no gain or sample conversion.
void player_renderer_capture(PlayerRenderer * _Nonnull renderer, const AudioBufferList * _Nullable input);

typedef struct BridgeForwarder BridgeForwarder;
typedef struct { uint64_t underruns, overflows; uint32_t queuedFrames; double ratio; uint64_t signalFrames; float peak; } BridgeMonitorStats;
BridgeForwarder * _Nullable bridge_forwarder_create(PlayerRenderer * _Nonnull recorder);
void bridge_forwarder_destroy(BridgeForwarder * _Nonnull forwarder);
void bridge_monitor_enable(BridgeForwarder * _Nonnull forwarder, bool enabled);
void bridge_monitor_gain(BridgeForwarder * _Nonnull forwarder, float gain);
void bridge_monitor_render(BridgeForwarder * _Nonnull forwarder, AudioBufferList * _Nonnull output);
BridgeMonitorStats bridge_monitor_stats(BridgeForwarder * _Nonnull forwarder);
OSStatus bridge_capture_callback(AudioObjectID device, const AudioTimeStamp * _Nonnull now,
    const AudioBufferList * _Nonnull input, const AudioTimeStamp * _Nonnull inputTime,
    AudioBufferList * _Nonnull output, const AudioTimeStamp * _Nonnull outputTime, void * _Nullable context);
void player_renderer_set_gain(PlayerRenderer * _Nonnull renderer, float gain);
PlayerRenderStats player_renderer_stats(PlayerRenderer * _Nonnull renderer);
void player_renderer_process(PlayerRenderer * _Nonnull renderer, const AudioBufferList * _Nullable input, AudioBufferList * _Nullable output);
OSStatus player_audio_callback(AudioObjectID device, const AudioTimeStamp * _Nonnull now,
    const AudioBufferList * _Nonnull input, const AudioTimeStamp * _Nonnull inputTime,
    AudioBufferList * _Nonnull output, const AudioTimeStamp * _Nonnull outputTime, void * _Nullable context);
