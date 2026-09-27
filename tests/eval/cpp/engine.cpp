#include <vector>
#include "engine.h"

namespace app {
class Engine {
public:
    void start() { tick(); } // @eval tick=Engine::tick
    void tick();
};
void Engine::tick() { std::sort(nullptr, nullptr); } // @eval sort=-

class Timer {
public:
    void tick() {}
};
}

int main() {
    app::Engine e;
    e.start(); // @eval start=Engine::start
    std::vector<int> v;
    v.push_back(1); // @eval push_back=-
    helper(); // @eval helper=helper
    return 0;
}

int helper() { return 0; }
