#include <CoreAudio/AudioServerPlugIn.h>
#include <CoreFoundation/CoreFoundation.h>
#include <dispatch/dispatch.h>
#include <assert.h>
#include <stdio.h>
#include <math.h>
#include <unistd.h>
#include "BridgeRing.h"

static AudioServerPlugInDriverRef driver;
static dispatch_semaphore_t configured;
static OSStatus changed(AudioServerPlugInHostRef h, AudioObjectID o, UInt32 n, const AudioObjectPropertyAddress *a) { return noErr; }
static OSStatus copyStorage(AudioServerPlugInHostRef h, CFStringRef key, CFPropertyListRef *out) { *out = NULL; return noErr; }
static OSStatus writeStorage(AudioServerPlugInHostRef h, CFStringRef key, CFPropertyListRef value) { return noErr; }
static OSStatus deleteStorage(AudioServerPlugInHostRef h, CFStringRef key) { return noErr; }
static OSStatus configure(AudioServerPlugInHostRef h, AudioObjectID id, UInt64 config, void *info) {
    OSStatus result = (*driver)->PerformDeviceConfigurationChange(driver,id,config,info);
    assert(result == noErr); dispatch_semaphore_signal(configured); return result;
}
static AudioServerPlugInHostInterface host = {changed,copyStorage,writeStorage,deleteStorage,configure};

int main(int argc, const char **argv) {
    assert(argc == 2);
    CFURLRef url = CFURLCreateFromFileSystemRepresentation(NULL,(const UInt8 *)argv[1],strlen(argv[1]),true);
    CFBundleRef bundle = CFBundleCreate(NULL,url); assert(bundle && CFBundleLoadExecutable(bundle));
    void *(*factory)(CFAllocatorRef,CFUUIDRef) = CFBundleGetFunctionPointerForName(bundle,CFSTR("AudioBridge_Create"));
    assert(factory); driver = factory(NULL,kAudioServerPlugInTypeUUID); assert(driver);
    configured = dispatch_semaphore_create(0);
    assert((*driver)->Initialize(driver,&host) == noErr);
    AudioObjectPropertyAddress p = {kAudioPlugInPropertyDeviceList,kAudioObjectPropertyScopeGlobal,0};
    UInt32 size = 0; AudioObjectID device = 0;
    assert((*driver)->GetPropertyDataSize(driver,1,0,&p,0,NULL,&size) == noErr && size == sizeof(device));
    assert((*driver)->GetPropertyData(driver,1,0,&p,0,NULL,sizeof(device),&size,&device) == noErr && device == 2);
    p.mSelector = kAudioDevicePropertyDeviceUID;
    CFStringRef uid = NULL;
    assert((*driver)->GetPropertyData(driver,2,0,&p,0,NULL,sizeof(uid),&size,&uid) == noErr);
    assert(CFEqual(uid,CFSTR("se.lbsa71.AudioBridge.endpoint.v1"))); CFRelease(uid);
    UInt32 rates[] = {44100,48000,88200,96000,176400,192000};
    UInt32 channels[] = {1,2,4,6,8,16,32};
    float *sent = malloc(512 * 32 * sizeof(float)), *received = malloc(512 * 32 * sizeof(float));
    for (size_t r=0;r<6;r++) for (size_t c=0;c<7;c++) {
        uint64_t config = ((uint64_t)rates[r]<<32)|channels[c];
        assert((*driver)->PerformDeviceConfigurationChange(driver,2,config,NULL) == noErr);
        p.mSelector = kAudioStreamPropertyVirtualFormat;
        AudioStreamBasicDescription f;
        assert((*driver)->GetPropertyData(driver,3,0,&p,0,NULL,sizeof(f),&size,&f) == noErr);
        assert(f.mSampleRate == rates[r] && f.mChannelsPerFrame == channels[c] && f.mBytesPerFrame == 4*channels[c]);
        assert((*driver)->GetPropertyData(driver,3,0,&p,0,NULL,1,&size,&f) == kAudioHardwareBadPropertySizeError);
        assert((*driver)->StartIO(driver,2,1) == noErr);
        AudioServerPlugInIOCycleInfo cycle = {0};
        for (uint32_t n=0;n<512*channels[c];n++) sent[n] = (float)((int)n%97-48)/100;
        cycle.mOutputTime.mSampleTime = 100000;
        cycle.mInputTime.mSampleTime = 100000+BRIDGE_LATENCY_FRAMES;
        assert((*driver)->DoIOOperation(driver,2,4,1,kAudioServerPlugInIOOperationWriteMix,512,&cycle,sent,NULL) == noErr);
        // Two independent readers must receive identical data without consuming it.
        for (UInt32 client=2;client<4;client++) {
            memset(received,0,512*channels[c]*4);
            assert((*driver)->DoIOOperation(driver,2,3,client,kAudioServerPlugInIOOperationReadInput,512,&cycle,received,NULL) == noErr);
            assert(memcmp(sent,received,512*channels[c]*4) == 0);
        }
        cycle.mInputTime.mSampleTime += BRIDGE_RING_FRAMES;
        assert((*driver)->DoIOOperation(driver,2,3,2,kAudioServerPlugInIOOperationReadInput,512,&cycle,received,NULL) == noErr);
        for (uint32_t n=0;n<512*channels[c];n++) assert(received[n] == 0);
        assert((*driver)->StopIO(driver,2,1) == noErr);
    }
    // Async configuration protocol and unsupported formats.
    p.mSelector = kAudioDevicePropertyNominalSampleRate; double rate=48000;
    assert((*driver)->SetPropertyData(driver,2,0,&p,0,NULL,sizeof(rate),&rate) == noErr);
    assert(dispatch_semaphore_wait(configured,dispatch_time(DISPATCH_TIME_NOW,NSEC_PER_SEC)) == 0);
    rate=12345;
    assert((*driver)->SetPropertyData(driver,2,0,&p,0,NULL,sizeof(rate),&rate) == kAudioDeviceUnsupportedFormatError);
    double sample1,sample2; UInt64 host1,host2,seed1,seed2;
    assert((*driver)->GetZeroTimeStamp(driver,2,1,&sample1,&host1,&seed1) == noErr);
    usleep(20000);
    assert((*driver)->GetZeroTimeStamp(driver,2,1,&sample2,&host2,&seed2) == noErr);
    assert(sample2 > sample1 && host2 > host1 && seed1 == seed2);
    assert(fabs((host2-host1)/(sample2-sample1))>0);
    free(sent); free(received);
    puts("PASS: loaded bundle, device enumeration, 42 rate/channel configurations, exact loopback, independent readers, stale-frame silence, format rejection and monotonic clock.");
    return 0;
}
