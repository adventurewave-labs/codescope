/* Sensor bus header: declarations live here, definitions in sensors.c. */
#ifndef SENSORS_H
#define SENSORS_H

int read_sensor(int channel);
void calibrate(int channel, double gain);

#endif
