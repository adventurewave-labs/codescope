#include <stdlib.h>
#include "list.h"

static void grow(struct list *l) {}

void list_push(struct list *l, int v) {
    grow(l); // @eval grow=grow
    l->items = realloc(l->items, 8); // @eval realloc=-
}
