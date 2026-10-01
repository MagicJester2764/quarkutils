// C++ on Quark: the parts a ported library actually uses.
//
// harfbuzz is C++ and so is the whole of Qt, so this is the floor under both.
// The interesting one is the exception: unwinding finds the frame tables
// through `dl_iterate_phdr`, which for a static program means reading the
// program headers out of the auxiliary vector -- and they are there because
// the argument page carries them, which is the same fact that lets musl find
// a thread-local template.

#include <algorithm>
#include <cstdio>
#include <map>
#include <memory>
#include <stdexcept>
#include <string>
#include <vector>

static int failures;

static void check(const char *what, bool ok) {
    std::printf("%s: %s\n", ok ? "ok" : "FAILED", what);
    if (!ok) {
        failures++;
    }
}

// A constructor that runs before main, which needs .init_array.
struct Early {
    Early() : value(0x1234) {}
    int value;
};
static Early early;

struct Shape {
    virtual ~Shape() = default;
    virtual int sides() const = 0;
};
struct Square : Shape {
    int sides() const override { return 4; }
};
struct Triangle : Shape {
    int sides() const override { return 3; }
    int corners() const { return 3; }
};

static int depth_charge(int n) {
    if (n == 0) {
        throw std::runtime_error("bottom");
    }
    return depth_charge(n - 1) + 1;
}

int main() {
    std::setvbuf(stdout, nullptr, _IONBF, 0);
    check("a constructor ran before main", early.value == 0x1234);

    std::vector<int> v;
    for (int i = 0; i < 1000; i++) {
        v.push_back((i * 37) % 101);
    }
    std::sort(v.begin(), v.end());
    check("vector and sort", v.size() == 1000 && v.front() <= v.back() &&
                                 std::is_sorted(v.begin(), v.end()));

    std::string s = "quark";
    s += "-";
    s += std::to_string(64);
    check("string", s == "quark-64" && s.find("64") == 6);

    std::map<std::string, int> m;
    m["one"] = 1;
    m["two"] = 2;
    m["three"] = 3;
    check("map", m.size() == 3 && m["two"] == 2 && m.count("four") == 0);

    std::unique_ptr<Shape> shape(new Triangle);
    check("virtual call", shape->sides() == 3);
    check("dynamic_cast", dynamic_cast<Triangle *>(shape.get()) != nullptr &&
                              dynamic_cast<Square *>(shape.get()) == nullptr);

    // Thrown through twenty frames, caught by type, and what it carries
    // survives the unwind.
    bool caught = false;
    std::string message;
    try {
        depth_charge(20);
    } catch (const std::runtime_error &e) {
        caught = true;
        message = e.what();
    } catch (...) {
        message = "wrong type";
    }
    check("exception", caught && message == "bottom");

    // And one that has to run a destructor on the way out.
    struct Marker {
        bool *flag;
        ~Marker() { *flag = true; }
    };
    bool unwound = false;
    try {
        Marker marker{&unwound};
        throw 42;
    } catch (int n) {
        check("exception by value", n == 42);
    }
    check("destructor ran while unwinding", unwound);

    std::printf("cxxtest: %d failed\n", failures);
    return failures ? 1 : 0;
}
