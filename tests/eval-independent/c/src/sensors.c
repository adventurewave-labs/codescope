#include "sensors.h"
#include <math.h>

static double offset = 0.5;

int read_sensor(int channel) {
    double v = raw_sample(channel); // @eval raw_sample=raw_sample@src/sensors.c
    v = floor(v + offset); // @eval floor=-
    return (int)v;
}

void calibrate(int channel, double gain) {
    offset = apply_gain(offset, gain); // @eval apply_gain=apply_gain@src/sensors.c
}

static double raw_sample(int channel) {
    return sin((double)channel) * 100.0; // @eval sin=-
}

static double apply_gain(double v, double g) {
    return v * g;
}
