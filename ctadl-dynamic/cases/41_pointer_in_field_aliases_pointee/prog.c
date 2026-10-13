/* F7 (OPEN, known gap): a pointer stored in a struct field does not alias its pointee for writes
   made later through the field. `sj.obs = &st; sj.obs->last = source();` writes `sj.obs.last`,
   which never meets `st.last`. d=flow, s=none. */
int source();
void sink(int);

struct Store { int last; };
struct Subj { struct Store* obs; };

int main() {
    struct Store st;
    st.last = 0;
    struct Subj sj;
    sj.obs = &st;
    sj.obs->last = source();
    sink(st.last);
    return 0;
}
