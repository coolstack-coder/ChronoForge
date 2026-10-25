// A tiny request handler that segfaults "sometimes".
// Whether it crashes depends on getrandom(), so re-running it under gdb
// is a coin flip. Record it once with ChronoForge and the crash is yours forever.

#include <stdio.h>
#include <string.h>
#include <time.h>
#include <sys/random.h>

int requests_served = 0;
long last_latency_ns = 0;
unsigned int session_seed = 0;
int *session_table[4];
int sessions[3] = {11, 22, 33};

static long now_ns(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts); // goes through the vDSO normally
    return ts.tv_sec * 1000000000L + ts.tv_nsec;
}

int main(void) {
    setvbuf(stdout, NULL, _IOLBF, 0);
    getrandom(&session_seed, sizeof session_seed, 0);

    // warm the session cache. slot 3 is "reserved"... and never filled
    for (int i = 0; i < 3; i++)
        session_table[i] = &sessions[i];

    char line[128];
    while (fgets(line, sizeof line, stdin)) {
        long t0 = now_ns();
        unsigned int slot = (session_seed + (unsigned)strlen(line)) % 4;
        int *session = session_table[slot];
        last_latency_ns = now_ns() - t0;
        printf("req %d -> slot %u session %d\n", requests_served, slot, *session);
        requests_served++;
    }
    printf("served %d requests\n", requests_served);
    return 0;
}
