#include <vector>
#include <algorithm>
#include "actuator.hpp"

// Rig owns a Servo through the Actuator interface; Servo is the only impl.
class Rig {
public:
    explicit Rig(Actuator* a) : actuator_(a) {}

    void arm(int level) {
        actuator_->engage(level); // @eval engage=Servo::engage@src/actuator.cpp
        log_state("armed"); // @eval log_state=Rig::log_state@src/rig.cpp
    }

    void sweep(const std::vector<int>& levels) {
        std::for_each(levels.begin(), levels.end(),
                      [this](int l) { arm(l); }); // @eval arm=Rig::arm@src/rig.cpp
    }

    static void log_state(const std::string& state) {
        (void)state.size(); // @eval size=-
    }

private:
    Actuator* actuator_;
};
