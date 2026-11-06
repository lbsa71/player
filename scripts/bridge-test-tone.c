// Output-only integration probe. Never changes the Mac's default device.
#include <AudioToolbox/AudioToolbox.h>
#include <CoreAudio/CoreAudio.h>
#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>
#include <string.h>
static UInt64 frame;
static void fill(void *context, AudioQueueRef queue, AudioQueueBufferRef buffer) {
    float *samples = buffer->mAudioData;
    for (unsigned i=0;i<512;i++,frame++) {
        samples[i*2] = ((int)(frame%997)-498)/32768.0f;
        samples[i*2+1] = -samples[i*2];
    }
    buffer->mAudioDataByteSize=512*2*sizeof(float);
    AudioQueueEnqueueBuffer(queue,buffer,0,NULL);
}
static void check(OSStatus status,const char *step) {
    if (status) { fprintf(stderr,"%s: %d\n",step,(int)status); exit(1); }
}
int main(int argc,char **argv) {
    CFStringRef uid=CFSTR("se.lbsa71.AudioBridge.endpoint.v1");
    AudioObjectPropertyAddress address={kAudioHardwarePropertyTranslateUIDToDevice,kAudioObjectPropertyScopeGlobal,0};
    AudioObjectID device=0; UInt32 size=sizeof(device);
    check(AudioObjectGetPropertyData(kAudioObjectSystemObject,&address,sizeof(uid),&uid,&size,&device),"Find endpoint");
    if (!device) { fprintf(stderr,"Endpoint not installed\n"); return 1; }
    address.mSelector=kAudioDevicePropertyNominalSampleRate;
    Float64 rate=0; size=sizeof(rate);
    if (argc == 3 && strcmp(argv[1],"--set-rate") == 0) {
        Float64 requested = atof(argv[2]);
        check(AudioObjectSetPropertyData(device,&address,0,NULL,sizeof(requested),&requested),"Set endpoint rate");
        for (int n=0;n<100;n++) {
            check(AudioObjectGetPropertyData(device,&address,0,NULL,&size,&rate),"Read endpoint rate");
            if (rate == requested) { printf("Endpoint rate: %.0f Hz\n",rate); return 0; }
            usleep(20000);
        }
        fprintf(stderr,"Endpoint rate change timed out\n"); return 1;
    }
    check(AudioObjectGetPropertyData(device,&address,0,NULL,&size,&rate),"Read rate");
    AudioStreamBasicDescription format={.mSampleRate=rate,.mFormatID=kAudioFormatLinearPCM,
        .mFormatFlags=kAudioFormatFlagsNativeFloatPacked,.mBytesPerPacket=8,.mFramesPerPacket=1,
        .mBytesPerFrame=8,.mChannelsPerFrame=2,.mBitsPerChannel=32};
    AudioQueueRef queue=NULL;
    check(AudioQueueNewOutput(&format,fill,NULL,NULL,NULL,0,&queue),"Create output queue");
    check(AudioQueueSetProperty(queue,kAudioQueueProperty_CurrentDevice,&uid,sizeof(uid)),"Choose endpoint");
    for (int i=0;i<3;i++) { AudioQueueBufferRef buffer; check(AudioQueueAllocateBuffer(queue,4096,&buffer),"Allocate"); fill(NULL,queue,buffer); }
    check(AudioQueueStart(queue,NULL),"Start tone");
    sleep(argc>1 ? (unsigned)atoi(argv[1]) : 5);
    check(AudioQueueStop(queue,true),"Stop tone"); AudioQueueDispose(queue,true);
    printf("Generated %llu frames at %.0f Hz on Audio Bridge only\n",frame,rate);
}
