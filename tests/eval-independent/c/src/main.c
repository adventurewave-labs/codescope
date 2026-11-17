#include <stdio.h>
#include "sensors.h"

int poll(int channel) {
    /* cross-TU call: read_sensor is declared in sensors.h, defined in sensors.c */
    int v = read_sensor(channel); // @eval read_sensor=read_sensor@src/sensors.c
    printf("poll=%d\n", v); // @eval printf=-
    return v;
}

int main(void) {
    return poll(3) > 0 ? 0 : 1; // @eval poll=poll@src/main.c
}
