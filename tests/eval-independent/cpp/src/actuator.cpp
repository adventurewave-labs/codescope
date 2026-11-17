#include "actuator.hpp"

void Servo::engage(int level) {
    pwm_ = clamp_level(level); // @eval clamp_level=clamp_level@src/actuator.cpp
}

bool Servo::engaged() const {
    return std::to_string(pwm_) != "0"; // @eval to_string=-
}

static int clamp_level(int level) {
    return std::max(0, std::min(180, level)); // @eval max=-
}
