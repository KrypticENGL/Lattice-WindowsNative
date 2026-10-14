/* Fixture: the basic graph shapes the engine must recover.
 *   caller -> callee -> helper     (calls)
 *   counter                        (global written by callee, read by helper/read_counter)
 *   external_fn                    (unresolved external, called by caller)
 *   callback = &read_counter       (address taken: a reference, not a call)
 */
#if defined(_MSC_VER)
#define NOINLINE __declspec(noinline)
#else
#define NOINLINE __attribute__((noinline))
#endif

int counter = 0;

extern int external_fn(int value);

static NOINLINE int helper(int x) {
    return x * 3 + counter;
}

NOINLINE int callee(int a, int b) {
    int sum = a + b;
    counter += sum;
    return helper(sum);
}

int caller(int n) {
    int total = 0;
    for (int i = 0; i < n; ++i) {
        total += callee(i, n);
    }
    return total + external_fn(total);
}

int read_counter(void) {
    return counter;
}

int (*callback)(void) = &read_counter;
