#pragma once
#include <string>

// Exactly one in-repo implementation of the Actuator interface.
class Actuator {
public:
    virtual ~Actuator() = default;
    virtual void engage(int level) = 0;
    virtual bool engaged() const = 0;
};

class Servo : public Actuator {
public:
    void engage(int level) override;
    bool engaged() const override;

private:
    int pwm_ = 0;
};
