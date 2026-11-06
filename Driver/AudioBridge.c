// Audio Server Plug-in interface follows Apple's NullAudio sample.
// See APPLE-SAMPLE-LICENSE.txt. Loopback, properties and configuration below
// intentionally have no application, filesystem, network or recording policy.
#include <CoreAudio/AudioServerPlugIn.h>
#include <CoreFoundation/CFPlugInCOM.h>
#include <dispatch/dispatch.h>
#include <mach/mach_time.h>
#include <math.h>
#include <limits.h>
#include "BridgeRing.h"

#define DEVICE_UID "se.lbsa71.AudioBridge.endpoint.v1"
#define BUNDLE_ID "se.lbsa71.AudioBridge.driver"
enum { Plugin = kAudioObjectPlugInObject, Device = 2, Input = 3, Output = 4 };
static const UInt32 rates[] = {44100, 48000, 88200, 96000, 176400, 192000};
static const UInt32 channelOptions[] = {1, 2, 4, 6, 8, 16, 32};
static AudioServerPlugInHostRef host;
static BridgeRing *ring;
static _Atomic(unsigned int) references = 1, running = 0;
static _Atomic(bool) inputActive = true, outputActive = true;
// One atomic snapshot publishes the rate and channel count together.
static _Atomic(unsigned long long) format = ((uint64_t)44100 << 32) | 2;
static _Atomic(unsigned long long) clockVersion = 0, anchorHost = 0, epoch = 1;
static double ticksPerSecond;
static AudioServerPlugInDriverInterface interface;
static AudioServerPlugInDriverInterface *interfacePointer = &interface;
static AudioServerPlugInDriverRef driver = &interfacePointer;

static bool goodDevice(AudioServerPlugInDriverRef d, AudioObjectID id) { return d == driver && id == Device; }
static bool goodObject(AudioServerPlugInDriverRef d, AudioObjectID id) { return d == driver && id >= Plugin && id <= Output; }
static bool goodRate(double value) {
    for (size_t i = 0; i < sizeof(rates)/sizeof(*rates); i++) if (value == rates[i]) return true;
    return false;
}
static bool goodChannels(UInt32 count) {
    for (size_t i = 0; i < sizeof(channelOptions)/sizeof(*channelOptions); i++) if (count == channelOptions[i]) return true;
    return false;
}
static AudioStreamBasicDescription asbd(uint64_t config) {
    UInt32 channels = (UInt32)config;
    return (AudioStreamBasicDescription){.mSampleRate = config >> 32,
        .mFormatID = kAudioFormatLinearPCM, .mFormatFlags = kAudioFormatFlagsNativeFloatPacked,
        .mBytesPerPacket = 4 * channels, .mFramesPerPacket = 1, .mBytesPerFrame = 4 * channels,
        .mChannelsPerFrame = channels, .mBitsPerChannel = 32};
}

__attribute__((visibility("default"))) void *AudioBridge_Create(CFAllocatorRef allocator, CFUUIDRef type) {
    return type && CFEqual(type, kAudioServerPlugInTypeUUID) ? driver : NULL;
}
static HRESULT query(void *d, REFIID uuid, LPVOID *result) {
    if (d != driver || !result) return E_POINTER;
    *result = NULL;
    CFUUIDRef value = CFUUIDCreateFromUUIDBytes(NULL, uuid);
    bool supported = CFEqual(value, IUnknownUUID) || CFEqual(value, kAudioServerPlugInDriverInterfaceUUID);
    CFRelease(value);
    if (!supported) return E_NOINTERFACE;
    atomic_fetch_add(&references, 1); *result = driver; return S_OK;
}
static ULONG addRef(void *d) { return d == driver ? atomic_fetch_add(&references, 1) + 1 : 0; }
static ULONG release(void *d) {
    if (d != driver) return 0;
    unsigned int count = atomic_load(&references);
    while (count && !atomic_compare_exchange_weak(&references, &count, count - 1)) {}
    return count ? count - 1 : 0;
}
static OSStatus initialize(AudioServerPlugInDriverRef d, AudioServerPlugInHostRef h) {
    if (d != driver || !h) return kAudioHardwareBadObjectError;
    host = h;
    CFPropertyListRef stored = NULL;
    if (host->CopyFromStorage(host, CFSTR("format-v1"), &stored) == noErr && stored) {
        uint64_t saved = 0;
        if (CFGetTypeID(stored) == CFNumberGetTypeID() && CFNumberGetValue(stored,kCFNumberSInt64Type,&saved) &&
            goodRate(saved >> 32) && goodChannels((UInt32)saved)) atomic_store(&format,saved);
        CFRelease(stored);
    }
    if (!ring) ring = bridge_ring_create();
    if (!ring) return kAudioHardwareUnspecifiedError;
    mach_timebase_info_data_t timebase; mach_timebase_info(&timebase);
    ticksPerSecond = 1e9 * timebase.denom / timebase.numer;
    atomic_store(&anchorHost, mach_absolute_time());
    return noErr;
}
static OSStatus createDevice(AudioServerPlugInDriverRef d, CFDictionaryRef desc,
                             const AudioServerPlugInClientInfo *client, AudioObjectID *id) { return kAudioHardwareUnsupportedOperationError; }
static OSStatus destroyDevice(AudioServerPlugInDriverRef d, AudioObjectID id) { return kAudioHardwareUnsupportedOperationError; }
static OSStatus clientChanged(AudioServerPlugInDriverRef d, AudioObjectID id, const AudioServerPlugInClientInfo *client) {
    return goodDevice(d,id) && client ? noErr : kAudioHardwareBadObjectError;
}
static OSStatus performConfiguration(AudioServerPlugInDriverRef d, AudioObjectID id, UInt64 config, void *info) {
    if (!goodDevice(d,id)) return kAudioHardwareBadObjectError;
    if (!goodRate(config >> 32) || !goodChannels((UInt32)config)) return kAudioDeviceUnsupportedFormatError;
    // HAL serializes configuration changes and quiesces I/O around this call.
    atomic_fetch_add(&clockVersion, 1);
    CFNumberRef stored = CFNumberCreate(NULL,kCFNumberSInt64Type,&config);
    host->WriteToStorage(host,CFSTR("format-v1"),stored); CFRelease(stored);
    atomic_store(&format, config); atomic_store(&anchorHost, mach_absolute_time()); atomic_fetch_add(&epoch, 1);
    atomic_fetch_add(&clockVersion, 1);
    AudioObjectPropertyAddress rateChanged = {kAudioDevicePropertyNominalSampleRate, kAudioObjectPropertyScopeGlobal, 0};
    host->PropertiesChanged(host, Device, 1, &rateChanged);
    AudioObjectPropertyAddress changed[] = {
        {kAudioStreamPropertyVirtualFormat, kAudioObjectPropertyScopeGlobal, 0},
        {kAudioStreamPropertyPhysicalFormat, kAudioObjectPropertyScopeGlobal, 0}};
    host->PropertiesChanged(host, Input, 2, changed); host->PropertiesChanged(host, Output, 2, changed);
    AudioObjectPropertyAddress layout[] = {
        {kAudioDevicePropertyPreferredChannelLayout, kAudioObjectPropertyScopeInput, 0},
        {kAudioDevicePropertyPreferredChannelLayout, kAudioObjectPropertyScopeOutput, 0}};
    host->PropertiesChanged(host, Device, 2, layout);
    return noErr;
}
static OSStatus abortConfiguration(AudioServerPlugInDriverRef d, AudioObjectID id, UInt64 action, void *info) {
    return goodDevice(d,id) ? noErr : kAudioHardwareBadObjectError;
}

// One property implementation supplies both sizing and values; short scalar
// buffers are rejected, list reads return the prefix that fits.
static OSStatus property(AudioObjectID id, const AudioObjectPropertyAddress *a,
                         UInt32 qualifierSize, const void *qualifier, UInt32 capacity, UInt32 *size, void *data) {
    if (!a || !size) return kAudioHardwareIllegalOperationError;
    uint64_t config = atomic_load(&format);
    UInt32 scalar = 0; Float64 rate = config >> 32; CFStringRef string = NULL;
    AudioObjectID objects[2]; UInt32 count = 0;
    const void *value = NULL; UInt32 bytes = 0; bool list = false;
    AudioStreamBasicDescription streamFormat = asbd(config);
    AudioValueRange ranges[sizeof(rates)/sizeof(*rates)];
    AudioStreamRangedDescription formats[(sizeof(rates)/sizeof(*rates)) * (sizeof(channelOptions)/sizeof(*channelOptions))];
    AudioChannelLayout layout = {.mChannelLayoutTag = kAudioChannelLayoutTag_DiscreteInOrder | (UInt32)config};
    UInt32 stereo[] = {1, (UInt32)config > 1 ? 2 : 1};
    switch (a->mSelector) {
        case kAudioObjectPropertyBaseClass: scalar = kAudioObjectClassID; break;
        case kAudioObjectPropertyClass: scalar = id == Plugin ? kAudioPlugInClassID : id == Device ? kAudioDeviceClassID : kAudioStreamClassID; break;
        case kAudioObjectPropertyOwner: scalar = id == Plugin ? kAudioObjectUnknown : id == Device ? Plugin : Device; break;
        case kAudioObjectPropertyName:
            string = id == Plugin ? CFSTR("Audio Bridge Driver") : id == Device ? CFSTR("Audio Bridge") : id == Input ? CFSTR("Audio Bridge Loopback") : CFSTR("Audio Bridge Output"); break;
        case kAudioObjectPropertyManufacturer: string = CFSTR("Local Audio Tools"); break;
        case kAudioObjectPropertyOwnedObjects:
            if (id == Plugin) objects[count++] = Device;
            if (id == Device) {
                if (a->mScope != kAudioObjectPropertyScopeOutput) objects[count++] = Input;
                if (a->mScope != kAudioObjectPropertyScopeInput) objects[count++] = Output;
            }
            value = objects; bytes = count * sizeof(AudioObjectID); list = true; break;
        default: goto specific;
    }
    goto emit;
specific:
    if (id == Plugin) switch (a->mSelector) {
        case kAudioPlugInPropertyBundleID: string = CFSTR(BUNDLE_ID); break;
        case kAudioPlugInPropertyDeviceList: objects[0] = Device; value = objects; bytes = sizeof(AudioObjectID); list = true; break;
        case kAudioPlugInPropertyBoxList: value = objects; bytes = 0; list = true; break;
        case kAudioPlugInPropertyTranslateUIDToDevice:
            if (!data) { scalar = kAudioObjectUnknown; break; }
            if (qualifierSize != sizeof(CFStringRef) || !qualifier) return kAudioHardwareBadPropertySizeError;
            scalar = *(CFStringRef *)qualifier && CFEqual(*(CFStringRef *)qualifier, CFSTR(DEVICE_UID)) ? Device : kAudioObjectUnknown; break;
        case kAudioPlugInPropertyResourceBundle: string = CFSTR(""); break;
        default: return kAudioHardwareUnknownPropertyError;
    }
    else if (id == Device) switch (a->mSelector) {
        case kAudioDevicePropertyDeviceUID: string = CFSTR(DEVICE_UID); break;
        case kAudioDevicePropertyModelUID: string = CFSTR("se.lbsa71.AudioBridge.model.v1"); break;
        case kAudioDevicePropertyTransportType: scalar = kAudioDeviceTransportTypeVirtual; break;
        case kAudioDevicePropertyClockDomain: scalar = 0; break;
        case kAudioDevicePropertyDeviceIsAlive: scalar = 1; break;
        case kAudioDevicePropertyDeviceIsRunning: scalar = atomic_load(&running) > 0; break;
        case kAudioDevicePropertyDeviceCanBeDefaultDevice: scalar = 1; break;
        // Keep system alerts off the endpoint unless explicitly routed there.
        case kAudioDevicePropertyDeviceCanBeDefaultSystemDevice: scalar = 0; break;
        case kAudioDevicePropertyIsHidden: scalar = 0; break;
        case kAudioDevicePropertyLatency: scalar = a->mScope == kAudioObjectPropertyScopeInput ? BRIDGE_LATENCY_FRAMES : 0; break;
        case kAudioDevicePropertySafetyOffset: scalar = 0; break;
        case kAudioDevicePropertyZeroTimeStampPeriod: scalar = 512; break;
        case kAudioDevicePropertyRelatedDevices: objects[0] = Device; value = objects; bytes = sizeof(AudioObjectID); list = true; break;
        case kAudioObjectPropertyControlList: value = objects; bytes = 0; list = true; break;
        case kAudioDevicePropertyStreams:
            if (a->mScope != kAudioObjectPropertyScopeOutput) objects[count++] = Input;
            if (a->mScope != kAudioObjectPropertyScopeInput) objects[count++] = Output;
            value = objects; bytes = count * sizeof(AudioObjectID); list = true; break;
        case kAudioDevicePropertyNominalSampleRate: value = &rate; bytes = sizeof(rate); break;
        case kAudioDevicePropertyAvailableNominalSampleRates:
            for (size_t n = 0; n < sizeof(rates)/sizeof(*rates); n++) ranges[n] = (AudioValueRange){rates[n], rates[n]};
            value = ranges; bytes = sizeof(ranges); list = true; break;
        case kAudioDevicePropertyPreferredChannelsForStereo: value = stereo; bytes = sizeof(stereo); break;
        case kAudioDevicePropertyPreferredChannelLayout: value = &layout; bytes = offsetof(AudioChannelLayout, mChannelDescriptions); break;
        default: return kAudioHardwareUnknownPropertyError;
    }
    else if (id == Input || id == Output) switch (a->mSelector) {
        case kAudioStreamPropertyIsActive: scalar = atomic_load(id == Input ? &inputActive : &outputActive); break;
        case kAudioStreamPropertyDirection: scalar = id == Input; break;
        case kAudioStreamPropertyTerminalType: scalar = kAudioStreamTerminalTypeLine; break;
        case kAudioStreamPropertyStartingChannel: scalar = 1; break;
        case kAudioStreamPropertyLatency: scalar = 0; break;
        case kAudioStreamPropertyVirtualFormat:
        case kAudioStreamPropertyPhysicalFormat: value = &streamFormat; bytes = sizeof(streamFormat); break;
        case kAudioStreamPropertyAvailableVirtualFormats:
        case kAudioStreamPropertyAvailablePhysicalFormats:
            for (size_t c = 0; c < sizeof(channelOptions)/sizeof(*channelOptions); c++)
                for (size_t n = 0; n < sizeof(rates)/sizeof(*rates); n++) {
                    formats[count].mFormat = asbd(((uint64_t)rates[n] << 32) | channelOptions[c]);
                    formats[count++].mSampleRateRange = (AudioValueRange){rates[n],rates[n]};
                }
            value = formats; bytes = sizeof(formats); list = true; break;
        default: return kAudioHardwareUnknownPropertyError;
    }
    else return kAudioHardwareBadObjectError;
emit:
    if (string) { value = &string; bytes = sizeof(string); }
    else if (!value) { value = &scalar; bytes = sizeof(scalar); }
    if (!data) { *size = bytes; return noErr; }
    if (capacity < bytes && !list) return kAudioHardwareBadPropertySizeError;
    if (capacity < bytes) {
        UInt32 unit = value == formats ? sizeof(AudioStreamRangedDescription) : value == ranges ? sizeof(AudioValueRange) : sizeof(AudioObjectID);
        bytes = capacity / unit * unit;
    }
    if (string) CFRetain(string);
    if (bytes) memcpy(data, value, bytes);
    *size = bytes; return noErr;
}
static Boolean hasProperty(AudioServerPlugInDriverRef d, AudioObjectID id, pid_t pid, const AudioObjectPropertyAddress *a) {
    if (!goodObject(d,id)) return false;
    UInt32 size;
    // Translate properties need a real qualifier only when retrieving their value.
    CFStringRef uid = CFSTR(DEVICE_UID);
    return property(id, a, sizeof(uid), &uid, 0, &size, NULL) == noErr;
}
static OSStatus settable(AudioServerPlugInDriverRef d, AudioObjectID id, pid_t pid, const AudioObjectPropertyAddress *a, Boolean *out) {
    if (!out || !a) return kAudioHardwareIllegalOperationError;
    if (!hasProperty(d,id,pid,a)) return kAudioHardwareUnknownPropertyError;
    *out = (id == Device && a->mSelector == kAudioDevicePropertyNominalSampleRate) ||
        ((id == Input || id == Output) && (a->mSelector == kAudioStreamPropertyVirtualFormat ||
         a->mSelector == kAudioStreamPropertyPhysicalFormat || a->mSelector == kAudioStreamPropertyIsActive));
    return noErr;
}
static OSStatus getSize(AudioServerPlugInDriverRef d, AudioObjectID id, pid_t pid, const AudioObjectPropertyAddress *a,
                        UInt32 qs, const void *q, UInt32 *size) {
    return goodObject(d,id) ? property(id,a,qs,q,0,size,NULL) : kAudioHardwareBadObjectError;
}
static OSStatus getData(AudioServerPlugInDriverRef d, AudioObjectID id, pid_t pid, const AudioObjectPropertyAddress *a,
                        UInt32 qs, const void *q, UInt32 capacity, UInt32 *size, void *data) {
    if (!data) return kAudioHardwareIllegalOperationError;
    return goodObject(d,id) ? property(id,a,qs,q,capacity,size,data) : kAudioHardwareBadObjectError;
}
static OSStatus setData(AudioServerPlugInDriverRef d, AudioObjectID id, pid_t pid, const AudioObjectPropertyAddress *a,
                        UInt32 qs, const void *q, UInt32 size, const void *data) {
    if (!goodObject(d,id) || !a || !data) return kAudioHardwareBadObjectError;
    Boolean canSet = false;
    OSStatus status = settable(d,id,pid,a,&canSet);
    if (status || !canSet) return status ? status : kAudioHardwareUnsupportedOperationError;
    uint64_t requested = atomic_load(&format);
    if (a->mSelector == kAudioStreamPropertyIsActive) {
        if (size != sizeof(UInt32)) return kAudioHardwareBadPropertySizeError;
        atomic_store(id == Input ? &inputActive : &outputActive, *(const UInt32 *)data != 0);
        host->PropertiesChanged(host,id,1,a); return noErr;
    }
    if (a->mSelector == kAudioDevicePropertyNominalSampleRate) {
        if (size != sizeof(Float64)) return kAudioHardwareBadPropertySizeError;
        double rate = *(const Float64 *)data;
        if (!goodRate(rate)) return kAudioDeviceUnsupportedFormatError;
        requested = ((uint64_t)rate << 32) | (UInt32)requested;
    } else {
        if (size != sizeof(AudioStreamBasicDescription)) return kAudioHardwareBadPropertySizeError;
        const AudioStreamBasicDescription *f = data;
        if (!goodRate(f->mSampleRate) || !goodChannels(f->mChannelsPerFrame) ||
            f->mFormatID != kAudioFormatLinearPCM || f->mFormatFlags != kAudioFormatFlagsNativeFloatPacked ||
            f->mBitsPerChannel != 32 || f->mFramesPerPacket != 1 || f->mBytesPerFrame != 4 * f->mChannelsPerFrame ||
            f->mBytesPerPacket != f->mBytesPerFrame) return kAudioDeviceUnsupportedFormatError;
        requested = ((uint64_t)f->mSampleRate << 32) | f->mChannelsPerFrame;
    }
    if (requested != atomic_load(&format)) {
        dispatch_async(dispatch_get_global_queue(QOS_CLASS_USER_INITIATED, 0), ^{
            host->RequestDeviceConfigurationChange(host,Device,requested,NULL);
        });
    }
    return noErr;
}
static OSStatus startIO(AudioServerPlugInDriverRef d, AudioObjectID id, UInt32 client) {
    if (!goodDevice(d,id) || !ring) return kAudioHardwareBadObjectError;
    atomic_fetch_add(&running,1); return noErr;
}
static OSStatus stopIO(AudioServerPlugInDriverRef d, AudioObjectID id, UInt32 client) {
    if (!goodDevice(d,id)) return kAudioHardwareBadObjectError;
    unsigned int count = atomic_load(&running);
    while (count && !atomic_compare_exchange_weak(&running,&count,count-1)) {}
    return noErr;
}
static OSStatus zeroTime(AudioServerPlugInDriverRef d, AudioObjectID id, UInt32 client, Float64 *sample, UInt64 *time, UInt64 *seed) {
    if (!goodDevice(d,id) || !sample || !time || !seed) return kAudioHardwareIllegalOperationError;
    uint64_t version = atomic_load(&clockVersion);
    if (version & 1) return kAudioHardwareNotRunningError;
    uint64_t anchor = atomic_load(&anchorHost), config = atomic_load(&format), generation = atomic_load(&epoch);
    uint64_t now = mach_absolute_time();
    double ticks = ticksPerSecond / (config >> 32);
    double frames = floor(((double)(now - anchor) / ticks) / 512) * 512;
    if (version != atomic_load(&clockVersion)) return kAudioHardwareNotRunningError;
    *sample = frames; *time = anchor + (UInt64)(frames * ticks); *seed = generation; return noErr;
}
static OSStatus willIO(AudioServerPlugInDriverRef d, AudioObjectID id, UInt32 client, UInt32 operation, Boolean *will, Boolean *inPlace) {
    if (!goodDevice(d,id) || !will || !inPlace) return kAudioHardwareIllegalOperationError;
    *will = operation == kAudioServerPlugInIOOperationReadInput || operation == kAudioServerPlugInIOOperationWriteMix;
    *inPlace = true; return noErr;
}
static OSStatus beginEndIO(AudioServerPlugInDriverRef d, AudioObjectID id, UInt32 client, UInt32 op, UInt32 frames, const AudioServerPlugInIOCycleInfo *info) {
    return goodDevice(d,id) ? noErr : kAudioHardwareBadObjectError;
}
static OSStatus doIO(AudioServerPlugInDriverRef d, AudioObjectID id, AudioObjectID stream, UInt32 client,
                     UInt32 operation, UInt32 frames, const AudioServerPlugInIOCycleInfo *info, void *buffer, void *secondary) {
    if (!goodDevice(d,id) || !ring || !buffer || !info || frames > BRIDGE_RING_FRAMES) return kAudioHardwareIllegalOperationError;
    UInt32 channels = (UInt32)atomic_load(&format);
    uint64_t generation = atomic_load(&epoch);
    if (operation == kAudioServerPlugInIOOperationWriteMix && stream == Output) {
        double position = info->mOutputTime.mSampleTime;
        if (!isfinite(position) || position < 0 || position > 0x1p52) return kAudioHardwareIllegalOperationError;
        if (atomic_load(&outputActive)) bridge_ring_write(ring,generation,(uint64_t)position,channels,frames,buffer);
    } else if (operation == kAudioServerPlugInIOOperationReadInput && stream == Input) {
        double position = info->mInputTime.mSampleTime;
        if (!isfinite(position) || fabs(position) > 0x1p52) return kAudioHardwareIllegalOperationError;
        if (atomic_load(&inputActive)) bridge_ring_read(ring,generation,(int64_t)position - BRIDGE_LATENCY_FRAMES,channels,frames,buffer);
        else memset(buffer,0,frames * channels * sizeof(float));
    } else return kAudioHardwareUnsupportedOperationError;
    return noErr;
}
static AudioServerPlugInDriverInterface interface = {
    NULL, query, addRef, release, initialize, createDevice, destroyDevice, clientChanged, clientChanged,
    performConfiguration, abortConfiguration, hasProperty, settable, getSize, getData, setData,
    startIO, stopIO, zeroTime, willIO, beginEndIO, doIO, beginEndIO
};
