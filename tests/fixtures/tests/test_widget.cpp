namespace demo {

struct Widget {
    int id;
    float value;
};

int widget_area(const Widget& w);

void test_widget_area_unit() {
    Widget w{2, 1.0f};
    (void)widget_area(w);
}

} // namespace demo
