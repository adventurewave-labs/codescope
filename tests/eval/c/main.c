#include <stdio.h>
#include "list.h"

int main(void) {
    struct list l;
    list_push(&l, 1); // @eval list_push=list_push
    printf("%d\n", 1); // @eval printf=-
    l.ops->run(); // @eval run=-
    return helper(); // @eval helper=helper
}

int helper(void) { return 0; }
