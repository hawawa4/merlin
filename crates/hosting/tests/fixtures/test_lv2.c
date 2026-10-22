/* A minimal LV2 instrument for the host's tests: a sine voice from MIDI, a gain, work done on
   the worker thread (the answer shows on an output port), and state through state:interface. */
#include <math.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

typedef void *Handle;
typedef struct { const char *URI; void *data; } Feature;
typedef struct Descriptor Descriptor;
struct Descriptor {
    const char *URI;
    Handle (*instantiate)(const Descriptor *, double, const char *, const Feature *const *);
    void (*connect_port)(Handle, uint32_t, void *);
    void (*activate)(Handle);
    void (*run)(Handle, uint32_t);
    void (*deactivate)(Handle);
    void (*cleanup)(Handle);
    const void *(*extension_data)(const char *);
};
typedef struct { void *handle; uint32_t (*map)(void *, const char *); } UridMap;
typedef int (*Respond)(void *, uint32_t, const void *);
typedef struct { void *handle; int (*schedule_work)(void *, uint32_t, const void *); } Schedule;
typedef struct { int (*work)(Handle, Respond, void *, uint32_t, const void *);
                 int (*work_response)(Handle, uint32_t, const void *);
                 int (*end_run)(Handle); } WorkerIface;
typedef int (*Store)(void *, uint32_t, const void *, size_t, uint32_t, uint32_t);
typedef const void *(*Retrieve)(void *, uint32_t, size_t *, uint32_t *, uint32_t *);
typedef struct { int (*save)(Handle, Store, void *, uint32_t, const Feature *const *);
                 int (*restore)(Handle, Retrieve, void *, uint32_t, const Feature *const *); } StateIface;
typedef struct { uint32_t size, type; } Atom;
typedef struct { Atom atom; uint32_t unit, pad; } Seq;
typedef struct { int64_t frames; Atom body; } Event;

typedef struct {
    const Seq *midi; float *out_l, *out_r, *gain, *worked, *restored;
    double rate, phase, hz; int on; float vel;
    UridMap *map; Schedule *sched; uint32_t midi_urid, int_urid, key_urid;
    int worker_asked; int32_t saved; int32_t response;
} Plugin;

static Handle instantiate(const Descriptor *d, double rate, const char *bundle, const Feature *const *f) {
    Plugin *p = calloc(1, sizeof(Plugin));
    p->rate = rate; p->saved = 1234;
    for (int i = 0; f[i]; i++) {
        if (!strcmp(f[i]->URI, "http://lv2plug.in/ns/ext/urid#map")) p->map = f[i]->data;
        if (!strcmp(f[i]->URI, "http://lv2plug.in/ns/ext/worker#schedule")) p->sched = f[i]->data;
    }
    if (!p->map || !p->sched) { free(p); return 0; }
    p->midi_urid = p->map->map(p->map->handle, "http://lv2plug.in/ns/ext/midi#MidiEvent");
    p->int_urid = p->map->map(p->map->handle, "http://lv2plug.in/ns/ext/atom#Int");
    p->key_urid = p->map->map(p->map->handle, "urn:merlin:test-key");
    return p;
}
static void connect_port(Handle h, uint32_t port, void *data) {
    Plugin *p = h;
    switch (port) { case 0: p->midi = data; break; case 1: p->out_l = data; break; case 2: p->out_r = data; break;
                    case 3: p->gain = data; break; case 4: p->worked = data; break; case 5: p->restored = data; break; }
}
static void run(Handle h, uint32_t n) {
    Plugin *p = h;
    const uint8_t *base = (const uint8_t *)p->midi + sizeof(Seq);
    const uint8_t *end = (const uint8_t *)p->midi + sizeof(Atom) + p->midi->atom.size;
    uint32_t pos = 0;
    const uint8_t *cur = base;
    while (cur < end) {
        const Event *e = (const Event *)cur;
        const uint8_t *d = cur + sizeof(Event);
        if (e->body.type == p->midi_urid && (d[0] & 0xF0) == 0x90 && d[2] > 0) {
            p->on = 1; p->vel = d[2] / 127.0f; p->hz = 440.0 * pow(2.0, (d[1] - 69) / 12.0);
            if (!p->worker_asked) { p->worker_asked = 1; int32_t v = 21; p->sched->schedule_work(p->sched->handle, 4, &v); }
        } else if (e->body.type == p->midi_urid && (d[0] & 0xF0) == 0x80) p->on = 0;
        cur += sizeof(Event) + ((e->body.size + 7) & ~7u);
    }
    for (uint32_t i = 0; i < n; i++) {
        float v = 0;
        if (p->on) { v = (float)sin(p->phase) * p->vel * *p->gain; p->phase += 6.283185307179586 * p->hz / p->rate; }
        p->out_l[i] = v; p->out_r[i] = v;
    }
    *p->worked = (float)p->response;
    *p->restored = (float)p->saved;
}
static void cleanup(Handle h) { free(h); }
static int work(Handle h, Respond respond, void *rh, uint32_t size, const void *data) {
    int32_t v = *(const int32_t *)data * 2; /* done off the audio thread */
    return respond(rh, 4, &v);
}
static int work_response(Handle h, uint32_t size, const void *data) { ((Plugin *)h)->response = *(const int32_t *)data; return 0; }
static int save(Handle h, Store store, void *sh, uint32_t flags, const Feature *const *f) {
    Plugin *p = h; return store(sh, p->key_urid, &p->saved, sizeof(int32_t), p->int_urid, flags);
}
static int restore(Handle h, Retrieve retrieve, void *sh, uint32_t flags, const Feature *const *f) {
    Plugin *p = h; size_t size; uint32_t type, fl;
    const void *v = retrieve(sh, p->key_urid, &size, &type, &fl);
    if (v && size == 4) p->saved = *(const int32_t *)v;
    return 0;
}
static const StateIface state_iface = { save, restore };
static const WorkerIface worker_iface = { work, work_response, 0 };
static const void *extension_data(const char *uri) {
    if (!strcmp(uri, "http://lv2plug.in/ns/ext/state#interface")) return &state_iface;
    if (!strcmp(uri, "http://lv2plug.in/ns/ext/worker#interface")) return &worker_iface;
    return 0;
}
static const Descriptor desc = { "urn:merlin:test-lv2", instantiate, connect_port, 0, run, 0, cleanup, extension_data };
const Descriptor *lv2_descriptor(uint32_t i) { return i == 0 ? &desc : 0; }
