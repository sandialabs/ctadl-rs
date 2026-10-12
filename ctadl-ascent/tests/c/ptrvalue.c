/* A tainted pointer *value* stays tainted through address arithmetic: every
 * sink_hit_* is reached, sink_clean_* is not. See
 * `test_cli_query_c_pointer_value_through_offset`. */
int *source_ptr(void);                  /* the pointer value itself is tainted */
void sink_hit_element_address(int *p);
void sink_hit_returned_address(int *p);
void sink_hit_sum(int *p);
void sink_clean_element(int v);

/* An address computed from the argument, and returned. */
static int *third(int *p) { return &p[2]; }

int main(void)
{
    int *p = source_ptr();

    /* `&p[2]` is `p + 2`: an address computed from the tainted value. */
    sink_hit_element_address(&p[2]);

    /* The same address, computed in a callee. */
    sink_hit_returned_address(third(source_ptr()));

    /* A binary `+` still lowers as a merge of its operands, so this one flows. */
    sink_hit_sum(p + 2);

    /* The value is tainted, not what it points at. */
    sink_clean_element(p[2]);
    return 0;
}
