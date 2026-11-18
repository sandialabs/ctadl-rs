/* A pointer to tainted bytes is stored in a struct field by one function and handed to a
 * body-less sink by another, with NO propagation model loaded. This is the tutorial's shape
 * (read_request -> parse_request -> dispatch -> run_shell -> system), reduced.
 *
 * It pins two engine properties at once, so it fails on either half alone:
 *
 *   - `sink_hit` must be reached. The bytes behind `req->arg` are the source's bytes, and no
 *     libc call is on the data path. Nothing in this program spells `.arg.deref` in one frame
 *     (`run` loads the pointer, the sink derefs its own formal), so if `compute_paths` admitted
 *     a compound access path only by concatenating a *model* path with a program path,
 *     `arg.deref` would never be admissible and an index built with no models would lose the
 *     flow.
 *
 *   - `sink_clean` must stay silent. `req->verb = line[0]` really taints `verb` (a byte of
 *     the buffer), and `req->arg` is a sibling. A call-site smear that leaks a saturating
 *     field's taint onto its siblings (pinned by `test_cli_query_c_no_callsite_smear`) would
 *     reach the constant sibling `dry_run` here, and would "find" the tutorial's flow through
 *     the pointer `arg` at the empty path rather than through the bytes.
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
