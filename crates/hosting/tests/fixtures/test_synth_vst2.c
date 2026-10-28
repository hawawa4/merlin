/* A minimal VST2 instrument for the host's tests: a sine voice per note, two parameters
   (gain, and a read-out of the tempo the host reported), chunk state, and an "editor".
   It can also misbehave on purpose, for the isolation tests: note 0 crashes the audio thread,
   note 1 hangs it, and a saved state starting with 666.0 crashes the main thread. */
#include <math.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

typedef struct AEffect AEffect;
typedef intptr_t (*Dispatcher)(AEffect *, int32_t, int32_t, intptr_t, void *, float);
typedef void (*Process)(AEffect *, float **, float **, int32_t);
typedef void (*SetParam)(AEffect *, int32_t, float);
typedef float (*GetParam)(AEffect *, int32_t);
typedef intptr_t (*HostCb)(AEffect *, int32_t, int32_t, intptr_t, void *, float);

struct AEffect {
    int32_t magic;
    Dispatcher dispatcher;
    Process process;
    SetParam setParameter;
    GetParam getParameter;
    int32_t numPrograms, numParams, numInputs, numOutputs, flags;
    intptr_t resvd1, resvd2;
    int32_t initialDelay, realQualities, offQualities;
    float ioRatio;
    void *object;
    void *user;
    int32_t uniqueID, version;
    Process processReplacing;
    void *processDoubleReplacing;
    char future[56];
};

typedef struct { int32_t type, byteSize, deltaFrames, flags, noteLength, noteOffset; unsigned char midi[4]; } MidiEvent;
typedef struct { int32_t numEvents; intptr_t reserved; MidiEvent *events[2]; } Events;
typedef struct { double samplePos, sampleRate, nanoSeconds, ppqPos, tempo, barStart, cycleStart, cycleEnd;
                 int32_t num, den, smpte, rate, toClock, flags; } TimeInfo;

static HostCb host;

typedef struct {
    float gain;
    float tempo_seen;
    int key;        /* -1 when silent */
    float vel;
    double phase;
    double rate;
    int editor_open;
} State;

static intptr_t dispatch(AEffect *e, int32_t op, int32_t index, intptr_t value, void *ptr, float opt) {
    State *s = (State *)e->object;
    switch (op) {
    case 0: return 0;                               /* open */
    case 1: free(s); free(e); return 0;             /* close */
    case 8: strcpy((char *)ptr, index == 0 ? "Gain" : "Host tempo"); return 0;
    case 10: s->rate = opt; return 0;
    case 12: return 0;
    case 13: { static short rect[4] = {0, 0, 200, 320}; *(short **)ptr = rect; return 1; } /* top,left,bottom,right */
    case 14: s->editor_open = 1; return 1;
    case 15: s->editor_open = 0; return 1;
    case 19: return 0;
    case 23: { static float chunk[2]; chunk[0] = s->gain; chunk[1] = 42.0f; *(void **)ptr = chunk; return sizeof(chunk); }
    case 24: { float *c = (float *)ptr; if (value >= 8) { if (c[0] == 666.0f) { *(volatile int *)0 = 1; } s->gain = c[0]; } return 0; }
    case 25: {
        Events *ev = (Events *)ptr;
        for (int i = 0; i < ev->numEvents; i++) {
            MidiEvent *m = ev->events[i];
            if ((m->midi[0] & 0xF0) == 0x90 && m->midi[2] > 0 && m->midi[1] == 0) { *(volatile int *)0 = 1; }
            if ((m->midi[0] & 0xF0) == 0x90 && m->midi[2] > 0 && m->midi[1] == 1) { for (volatile int spin = 1; spin; ) {} }
            if ((m->midi[0] & 0xF0) == 0x90 && m->midi[2] > 0) { s->key = m->midi[1]; s->vel = m->midi[2] / 127.0f; }
            else if ((m->midi[0] & 0xF0) == 0x80 || (m->midi[0] & 0xF0) == 0x90) { if (s->key == m->midi[1]) s->key = -1; }
        }
        return 1;
    }
    case 26: return 1;
    case 35: return 2;                              /* synth category */
    case 45: strcpy((char *)ptr, "Test Synth"); return 1;
    case 47: strcpy((char *)ptr, "Merlin tests"); return 1;
    case 48: strcpy((char *)ptr, "Test Synth"); return 1;
    case 71: case 72: return 0;
    }
    return 0;
}

static void process(AEffect *e, float **in, float **out, int32_t n) {
    State *s = (State *)e->object;
    TimeInfo *t = (TimeInfo *)host(e, 7, 0, 0, 0, 0);
    if (t) s->tempo_seen = (float)(t->tempo / 300.0);
    for (int i = 0; i < n; i++) {
        float v = 0;
        if (s->key >= 0) {
            double hz = 440.0 * pow(2.0, (s->key - 69) / 12.0);
            v = (float)sin(s->phase) * s->vel * s->gain;
            s->phase += 6.283185307179586 * hz / s->rate;
        }
        out[0][i] = v;
        out[1][i] = v;
    }
}

static void set_param(AEffect *e, int32_t i, float v) { State *s = e->object; if (i == 0) s->gain = v; }
static float get_param(AEffect *e, int32_t i) { State *s = e->object; return i == 0 ? s->gain : s->tempo_seen; }

AEffect *VSTPluginMain(HostCb cb) {
    host = cb;
    AEffect *e = calloc(1, sizeof(AEffect));
    State *s = calloc(1, sizeof(State));
    s->gain = 0.5f; s->key = -1; s->rate = 44100;
    e->magic = 0x56737450;
    e->dispatcher = dispatch;
    e->process = process;
    e->processReplacing = process;
    e->setParameter = set_param;
    e->getParameter = get_param;
    e->numParams = 2;
    e->numOutputs = 2;
    e->flags = (1 << 0) | (1 << 4) | (1 << 5) | (1 << 8);
    e->uniqueID = 0x54535931; /* 'TSY1' */
    e->version = 1000;
    e->object = s;
    return e;
}
