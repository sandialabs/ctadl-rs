/* A pointer to tainted bytes is stored in a struct field by one function and handed to a
 * body-less sink by another, with NO propagation model loaded. This is the tutorial's shape
 * (read_request -> parse_request -> dispatch -> run_shell -> system), reduced.
 *
 * It pins two engine properties at once, so it fails on either half alone:
 *
 *   - `sink_hit` must be reached. The bytes behind `req->arg` are the source's bytes, and no
 *     libc call is on the data path. Before this test, an index built with no models could
 *     not carry taint through a pointer stored in a field: `compute_paths` admits a compound
 *     access path (`arg.deref`) only by concatenating a *model* path with a program path, and
 *     nothing in this program spells `.arg.deref` in one frame (`run` loads the pointer, the
 *     sink derefs its own formal). So `arg.deref` was never admissible and the flow was lost.
 *
 *   - `sink_clean` must stay silent. `req->verb = line[0]` really taints `verb` (a byte of
 *     the buffer), and `req->arg` is a sibling. The call-site smear (fixed by
 *     `test_cli_query_c_no_callsite_smear`) used to leak a saturating field's taint onto its
 *     siblings, which is how the tutorial's flow was "found" before: through the pointer
 *     `arg` at the empty path, never through the bytes. That same leak would reach the
 *     constant sibling `dry_run` here.
 *
 * See `test_cli_query_c_pointer_field_without_models` in tests/cli.rs.
 */
char *source(void);
void  sink_hit(const char *s);
void  sink_clean(int x);

struct request {
    char  verb;
    char *arg;
    int   dry_run;
};

/* Taint moves into a struct field through an out-parameter: a byte copy into `verb`, and a
 * pointer into the same bytes into `arg`. */
static void parse(char *line, struct request *req)
{
    req->verb = line[0];
    req->arg = line + 2;
    req->dry_run = 0;
}

static void run(struct request *req)
{
    sink_clean(req->dry_run);       /* a sibling field holding a constant: must stay silent */
    sink_hit(req->arg);             /* the source's bytes, two hops later: must be reached */
}

int main(void)
{
    struct request req;
    char *line = source();
    if (line == 0)
        return 1;
    parse(line, &req);
    run(&req);
    return 0;
}
