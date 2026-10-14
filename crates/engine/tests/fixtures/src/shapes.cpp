// Fixture: C++ from MinGW g++. Virtual classes produce vtables and typeinfo,
// a global with a constructor produces static initialisers, and the extern
// variable is reached through a `.refptr.imported_value` cell.
#include <cstdio>

struct Shape {
    virtual ~Shape() {}
    virtual double area() const = 0;
};

struct Square : Shape {
    double s;
    explicit Square(double v) : s(v) {}
    double area() const override { return s * s; }
};

int total = 0;
extern int imported_value;
static Square unit(1.0);

double measure(const Shape& shape) {
    double a = shape.area();
    total += 1;
    return a;
}

int main() {
    int count = 3;
    Square sq(2.0);
    double sum = measure(sq) + measure(unit);
    std::printf("%d %f %d\n", count, sum, imported_value);
    return 0;
}
