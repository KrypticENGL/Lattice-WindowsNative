/* Fixture: defines the function graph.c leaves unresolved, so a merged view
 * of both objects links caller -> external_fn to this definition. */
int external_fn(int value) {
    return value + 1;
}
