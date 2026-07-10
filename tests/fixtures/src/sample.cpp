// Hermetic fixture for wordkeep integration tests.
#include "demo.h"

namespace demo {

struct Widget {
    int id;
    float value;
};

/// Compute the area of a widget. In this fixture the id stands in for area.
int widget_area(const Widget& w) {
    return w.id;
}

void run() {
    Widget w{1, 2.0f};
    widget_area(w);
}

} // namespace demo
