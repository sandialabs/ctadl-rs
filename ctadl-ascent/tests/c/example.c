#include <stdio.h>

// Define a struct
struct Point {
    int x;
    int y;
};

// Define a union
union Data {
    int intValue;
    float floatValue;
    char charValue;
};

// Function prototypes
void functionA(int a);
void functionB(int a, struct Point p);
void functionC(int a, int b, union Data d);

int main() {
    struct Point p1 = {10, 20};
    union Data d1;
    d1.intValue = 42;

    functionA(5);
    functionB(10, p1);
    functionC(15, 25, d1);

    int a, b, c, d, e;
    c = a + b;
    d = c;
    e = a ? c : d;

    return e;
}

// Function A: Calls Function B
void functionA(int a) {
    printf("Function A called with %d\n", a);
    functionB(a, (struct Point){0, 0}); // Calling functionB with a Point struct
}

// Function B: Calls Function C
void functionB(int a, struct Point p) {
    printf("Function B called with %d and Point(%d, %d)\n", a, p.x, p.y);
    union Data d;
    d.floatValue = 3.14f; // Assigning a float value to the union
    functionC(a, p.x + p.y, d); // Calling functionC with two integers and a union
}

// Function C: Receives two integers and a union
void functionC(int a, int b, union Data d) {
    printf("Function C called with %d, %d, and union intValue: %d\n", a, b, d.intValue);
}
