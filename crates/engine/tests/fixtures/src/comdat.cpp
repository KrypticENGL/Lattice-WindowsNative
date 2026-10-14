// Fixture: inline functions and template instantiations, which MSVC emits
// as COMDAT sections (with associative .pdata/.xdata COMDATs).

template <typename T>
T twice(T v) {
    return v + v;
}

inline int shared_inline(int x) {
    return twice(x) + 1;
}

int use_comdat(int x) {
    return shared_inline(x) + static_cast<int>(twice<long long>(x));
}
