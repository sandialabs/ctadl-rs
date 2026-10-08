//! Text rendering of a [`Report`].
//!
//! Same data as `--format json`, laid out for a person. Two rules it follows throughout:
//! a number that is a lower bound says so where it prints, and a section that does not
//! apply to the program is absent rather than zero -- a page of zeros reads as a finding.

use std::io::{Result, Write};

use super::Report;
use super::callgraph::{CallGraphReport, Language, SignatureRow};
use crate::stats::Distribution;

/// Renders the whole report.
pub fn text(w: &mut dyn Write, report: &Report) -> Result<()> {
    // The first line names the tier. Not because anything here is missing for want of an
    // index -- see `crate::report` for why nothing is -- but because a reader who has not
    // read that will assume indexing first would have improved these numbers, and the
    // report should say plainly that it would not.
    writeln!(
        w,
        "ctadl report '{}' -- static tier: measured from the import alone.",
        report.name
    )?;
    writeln!(
        w,
        "No index was read, and indexing first would add nothing to these numbers"
    )?;
    writeln!(w, "in this version; see `ctadl_ascent::report` for why.")?;
    if !report.empty_imports.is_empty() {
        writeln!(
            w,
            "\n{} import(s) carry no functions and are not reported on: {}",
            report.empty_imports.len(),
            report.empty_imports.join(", ")
        )?;
        writeln!(
            w,
            "(An .xapk parent is always one of these: its code lives in the split sub-imports.)"
        )?;
    }
    if report.programs.is_empty() {
        writeln!(w, "\nNo program in '{}' has any functions.", report.name)?;
        return Ok(());
    }
    for program in &report.programs {
        program_text(w, program)?;
    }
    Ok(())
}

fn program_text(w: &mut dyn Write, r: &CallGraphReport) -> Result<()> {
    writeln!(w, "\n{}", "=".repeat(78))?;
    writeln!(w, "program '{}'  ({} functions)", r.import, r.functions)?;
    writeln!(w, "{}", "=".repeat(78))?;

    writeln!(
        w,
        "\n-- calls ---------------------------------------------------"
    )?;
    let c = &r.census;
    writeln!(w, "  call sites                {:>12}", c.total)?;
    writeln!(
        w,
        "    direct                  {:>12}   {}",
        c.direct,
        pct(c.direct, c.total)
    )?;
    if c.virtual_ > 0 || r.language == Language::Java {
        writeln!(
            w,
            "    virtual (Java)          {:>12}   {}",
            c.virtual_,
            pct(c.virtual_, c.total)
        )?;
    }
    if c.lua > 0 || r.language == Language::Lua {
        writeln!(
            w,
            "    virtual (Lua)           {:>12}   {}",
            c.lua,
            pct(c.lua, c.total)
        )?;
    }
    // Always shown for a program with no class hierarchy: there the indirect-call count is
    // the whole story, and a missing row would read as "not measured" rather than "none".
    if c.func_ptr > 0 || r.language == Language::Other {
        writeln!(
            w,
            "    function pointer        {:>12}   {}",
            c.func_ptr,
            pct(c.func_ptr, c.total)
        )?;
    }
    if c.unknown > 0 {
        writeln!(
            w,
            "    unclassified            {:>12}   {}",
            c.unknown,
            pct(c.unknown, c.total)
        )?;
    }
    if let Some(d) = &c.by_dispatch {
        writeln!(
            w,
            "      invoke-virtual      {:>12}   {}",
            d.virtual_,
            pct(d.virtual_, c.virtual_)
        )?;
        writeln!(
            w,
            "      invoke-interface    {:>12}   {}",
            d.interface,
            pct(d.interface, c.virtual_)
        )?;
        writeln!(
            w,
            "      invoke-super        {:>12}   {}",
            d.super_,
            pct(d.super_, c.virtual_)
        )?;
        if d.unknown > 0 {
            writeln!(
                w,
                "      no dispatch recorded{:>12}   {}   invokedynamic, or a synthesised call",
                d.unknown,
                pct(d.unknown, c.virtual_)
            )?;
        }
        writeln!(
            w,
            "  All three resolve identically today -- an invoke-super is resolved as though the"
        )?;
        writeln!(
            w,
            "  receiver's type were unknown, which over-approximates it. They are counted apart"
        )?;
        writeln!(
            w,
            "  because CHA behaves far worse on an interface, and an average over the two"
        )?;
        writeln!(w, "  describes neither.")?;
    }

    if r.language == Language::Other {
        writeln!(
            w,
            "\n  This program has no class hierarchy, so there is nothing to resolve by type"
        )?;
        writeln!(
            w,
            "  and the type-resolution sections are omitted rather than reported as zero."
        )?;
        return Ok(());
    }

    if let Some(v) = &r.virtual_targets {
        writeln!(
            w,
            "\n-- targets per virtual call site ---------------------------"
        )?;
        writeln!(w, "  virtual sites             {:>12}", v.sites)?;
        writeln!(w, "  distinct signatures       {:>12}", v.signatures)?;
        writeln!(
            w,
            "  call edges (site,target)  {:>12}   {:.1}x the site count",
            v.total_edges,
            ratio(v.total_edges, v.sites)
        )?;
        distribution(w, "  targets/site", &v.targets_per_site)?;
        writeln!(
            w,
            "  exactly one target        {:>12}   {}   <- already cheap",
            v.sites_with_one_target,
            pct(v.sites_with_one_target, v.sites)
        )?;
        writeln!(
            w,
            "  two or more targets       {:>12}   {}   <- handed to hybrid inlining under `mixed`",
            v.sites_deferred_to_hybrid_inlining,
            pct(v.sites_deferred_to_hybrid_inlining, v.sites)
        )?;
        writeln!(
            w,
            "  zero targets              {:>12}   {}   <- dropped silently by codegen; the",
            v.sites_with_zero_targets,
            pct(v.sites_with_zero_targets, v.sites)
        )?;
        writeln!(
            w,
            "                                                    graph is unsound here (missing"
        )?;
        writeln!(
            w,
            "                                                    library code, native, reflection)"
        )?;
        if !v.by_dispatch.is_empty() {
            writeln!(
                w,
                "\n  By dispatch kind. The pooled row above is an average over populations that"
            )?;
            writeln!(w, "  behave nothing alike, and belongs to neither of them:")?;
            writeln!(
                w,
                "    {:>10} {:>9} {:>13} {:>7} {:>7} {:>7} {:>7}  kind",
                "sites", "1 target", "edges", "p50", "p90", "p99", "max"
            )?;
            for d in &v.by_dispatch {
                writeln!(
                    w,
                    "    {:>10} {:>9} {:>13} {:>7} {:>7} {:>7} {:>7}  {}",
                    d.sites,
                    pct(d.sites_with_one_target, d.sites),
                    d.total_edges,
                    d.targets_per_site.p50,
                    d.targets_per_site.p90,
                    d.targets_per_site.p99,
                    d.targets_per_site.max,
                    d.dispatch
                )?;
            }
        }
    }

    if let Some(worst) = &r.worst_signatures {
        let v_sites = r.virtual_targets.as_ref().map_or(0, |v| v.sites);
        writeln!(
            w,
            "\n-- where the imprecision is -------------------------------"
        )?;
        writeln!(
            w,
            "  worst 10 sites own        {}  of all call edges",
            pct_f(worst.top_10_site_share)
        )?;
        writeln!(
            w,
            "  worst 100 sites own       {}  of all call edges",
            pct_f(worst.top_100_site_share)
        )?;
        writeln!(
            w,
            "  The two above are the literal ask -- ten instructions out of {},",
            plural(v_sites, "virtual site")
        )?;
        writeln!(
            w,
            "  so a small number there is the answer, not a measurement failure."
        )?;
        writeln!(
            w,
            "\n  excess edges              {:>12}   the call edges that exist only because",
            worst.excess_edges
        )?;
        writeln!(
            w,
            "                                           resolution is imprecise: every"
        )?;
        writeln!(
            w,
            "                                           resolved site needs one, the rest is"
        )?;
        writeln!(w, "                                           the cost")?;
        writeln!(
            w,
            "  worst 10 signatures own   {}  of the excess",
            pct_f(worst.top_10_signature_excess_share)
        )?;
        writeln!(
            w,
            "  worst 100 signatures own  {}  of the excess",
            pct_f(worst.top_100_signature_excess_share)
        )?;
        writeln!(
            w,
            "  These are the actionable ones -- you special-case a method, not a call"
        )?;
        writeln!(
            w,
            "  instruction, and doing so covers every site that dispatches on it."
        )?;
        writeln!(
            w,
            "\n  Least precise signatures, by target count (CHA is keyed by signature, so every"
        )?;
        writeln!(
            w,
            "  site sharing one has the same count; `sites` is how many there are):"
        )?;
        signature_rows(w, &worst.top_by_targets)?;
        writeln!(
            w,
            "\n  Most imprecision contributed, by excess -- what the shares above are made of,"
        )?;
        writeln!(w, "  and what there would be to special-case:")?;
        signature_rows(w, &worst.top_by_excess)?;
        for d in &worst.by_dispatch {
            writeln!(
                w,
                "\n  {} dispatch owns {} of the excess ({} edges); its worst, ranked within",
                d.dispatch,
                pct(d.excess_edges, worst.excess_edges),
                d.excess_edges
            )?;
            writeln!(
                w,
                "  the kind, with sites and edges counted over that kind's sites alone:"
            )?;
            writeln!(
                w,
                "    worst 10 own {} of it, worst 100 own {}",
                pct_f(d.top_10_signature_excess_share),
                pct_f(d.top_100_signature_excess_share)
            )?;
            signature_rows(w, &d.top_by_excess)?;
        }
    }

    if let Some(rta) = &r.rta {
        writeln!(
            w,
            "\n-- RTA versus CHA -----------------------------------------"
        )?;
        writeln!(
            w,
            "  classes the code allocates{:>12}",
            rta.allocated_classes
        )?;
        writeln!(w, "  CHA edges                 {:>12}", rta.cha_edges)?;
        writeln!(w, "  RTA edges                 {:>12}", rta.rta_edges)?;
        writeln!(
            w,
            "  dropped by RTA            {:>12}   {}",
            rta.edges_dropped,
            pct(rta.edges_dropped, rta.cha_edges)
        )?;
        distribution(w, "  dropped/site", &rta.gap_per_site)?;
        writeln!(
            w,
            "  RTA here is a LOWER BOUND. The allocated-class set comes from `new`-like"
        )?;
        writeln!(
            w,
            "  expressions in imported code only, so objects made by un-imported library code,"
        )?;
        writeln!(
            w,
            "  by reflection or by deserialization are invisible and RTA drops targets that are"
        )?;
        writeln!(
            w,
            "  genuinely reachable. This measures how much CHA over-approximates; it is not a"
        )?;
        writeln!(w, "  resolution strategy anything acts on.")?;
        if !rta.by_dispatch.is_empty() {
            writeln!(w, "    {:>13} {:>13} {:>13}  kind", "CHA", "RTA", "dropped")?;
            for d in &rta.by_dispatch {
                writeln!(
                    w,
                    "    {:>13} {:>13} {:>13}  {}   {}",
                    d.cha_edges,
                    d.rta_edges,
                    d.edges_dropped,
                    d.dispatch,
                    pct(d.edges_dropped, d.cha_edges)
                )?;
            }
        }
    }

    if let Some(cases) = &r.hard_cases {
        writeln!(
            w,
            "\n-- named hard cases ---------------------------------------"
        )?;
        writeln!(
            w,
            "  Matched by name and descriptor exactly, which survives obfuscation."
        )?;
        writeln!(
            w,
            "    {:>9} {:>9} {:>12} {:>9} {:>18}  method",
            "classes", "sites", "edges", "worst", "of those, iface"
        )?;
        for case in cases {
            writeln!(
                w,
                "    {:>9} {:>9} {:>12} {:>9} {:>18}  {}{}",
                case.signatures,
                case.sites,
                case.total_edges,
                case.max_targets,
                case.sites_by_dispatch
                    .as_ref()
                    .map_or_else(|| "-".to_string(), |d| d.interface.to_string()),
                case.name,
                case.descriptor
            )?;
        }
    }

    if let Some(k) = &r.kotlin_lambdas {
        writeln!(
            w,
            "\n-- Kotlin lambda / functional-interface calls --------------"
        )?;
        writeln!(
            w,
            "  by receiver type          {:>12}",
            k.sites_by_receiver_type
        )?;
        writeln!(
            w,
            "  by method name            {:>12}   (invoke, invokeSuspend)",
            k.sites_by_method_name
        )?;
        writeln!(w, "  both agree                {:>12}", k.sites_by_both)?;
        writeln!(
            w,
            "  receiver type only        {:>12}   name test misses these",
            k.receiver_type_only
        )?;
        writeln!(
            w,
            "  method name only          {:>12}   type test misses these",
            k.method_name_only
        )?;
        writeln!(
            w,
            "  of the type-matched, resolving to one body: {}   {}",
            k.receiver_type_sites_with_one_target,
            pct(
                k.receiver_type_sites_with_one_target,
                k.sites_by_receiver_type
            )
        )?;
        if let Some(d) = &k.receiver_type_sites_by_dispatch
            && d.total() > 0
        {
            writeln!(
                w,
                "  of the type-matched, by dispatch: {} interface, {} virtual, {} super, {} unrecorded",
                d.interface, d.virtual_, d.super_, d.unknown
            )?;
        }
        if k.matched_receiver_types.is_empty() {
            writeln!(
                w,
                "  No kotlin FunctionN receiver type appears in this program. That is what a"
            )?;
            writeln!(
                w,
                "  repackaged or type-stripped release APK looks like, not evidence of no lambdas;"
            )?;
            writeln!(w, "  compare the method-name count above.")?;
        } else {
            writeln!(w, "  matched receiver types:")?;
            for ty in &k.matched_receiver_types {
                writeln!(w, "    {ty}")?;
            }
        }
    }

    if let Some(f) = &r.functional_interfaces {
        writeln!(
            w,
            "\n-- functional interfaces (any SAM, not just Kotlin) --------"
        )?;
        writeln!(
            w,
            "  interfaces declared here  {:>12}",
            f.interfaces_declared
        )?;
        writeln!(
            w,
            "  with one abstract method  {:>12}   functional interfaces",
            f.single_abstract_method
        )?;
        writeln!(w, "  call sites on one         {:>12}", f.sites)?;
        writeln!(
            w,
            "  resolving to one body     {:>12}   {}",
            f.sites_with_one_target,
            pct(f.sites_with_one_target, f.sites)
        )?;
        writeln!(w, "  their call edges          {:>12}", f.total_edges)?;
        writeln!(w, "  of which excess           {:>12}", f.excess_edges)?;
        writeln!(
            w,
            "  interface calls on a type this import does not declare: {}",
            f.interface_sites_on_unknown_type
        )?;
        writeln!(
            w,
            "  That last line is the coverage gap, and on an app that does not ship the"
        )?;
        writeln!(
            w,
            "  framework it is most of them: `java/util/Iterator` is an interface nothing here"
        )?;
        writeln!(
            w,
            "  ever saw declared, so every number above is over the remainder. Unlike the"
        )?;
        writeln!(
            w,
            "  Kotlin section this matches no names, so obfuscation does not hide it."
        )?;
        if !f.top.is_empty() {
            writeln!(w, "  Worst functional-interface signatures, by excess:")?;
            signature_rows(w, &f.top)?;
        }
    }

    if let Some(p) = &r.policy {
        writeln!(
            w,
            "\n-- call-resolution policy (simulated) ----------------------"
        )?;
        writeln!(
            w,
            "  threshold {} ({} for interfaces), {}",
            p.cha_threshold, p.cha_threshold_interface, p.order
        )?;
        let b = &p.buckets;
        writeln!(w, "  java call sites           {:>12}", b.java_sites)?;
        writeln!(
            w,
            "    modelled                {:>12}   {}",
            b.modelled,
            pct(b.modelled, b.java_sites)
        )?;
        writeln!(
            w,
            "    skipped                 {:>12}   {}",
            b.skipped,
            pct(b.skipped, b.java_sites)
        )?;
        writeln!(
            w,
            "    CHA                     {:>12}   {}   ({} with no target, {} exact super)",
            b.cha,
            pct(b.cha, b.java_sites),
            b.cha_zero_targets,
            b.cha_super_exact
        )?;
        writeln!(
            w,
            "    inlined                 {:>12}   {}   ({} by model)",
            b.inlined,
            pct(b.inlined, b.java_sites),
            b.inlined_by_model
        )?;
        writeln!(w, "  call edges, plain CHA     {:>12}", p.cha_edges)?;
        writeln!(
            w,
            "  call edges, this policy   {:>12}   {:.1}x smaller",
            p.policy_edges,
            ratio(p.cha_edges, p.policy_edges.max(1))
        )?;
        for d in &p.by_dispatch {
            if d.buckets.java_sites == 0 {
                continue;
            }
            writeln!(
                w,
                "  {:<10} {:>12} sites: {} modelled, {} skipped, {} CHA, {} inlined",
                d.dispatch,
                d.buckets.java_sites,
                d.buckets.modelled,
                d.buckets.skipped,
                d.buckets.cha,
                d.buckets.inlined
            )?;
        }
        if !p.top_modelled.is_empty() {
            writeln!(w, "  Covered by a dispatch model, by excess removed:")?;
            for row in &p.top_modelled {
                writeln!(
                    w,
                    "    {:>7} excess  {:<5}  {}  [{}]",
                    row.signature.excess,
                    row.disposition,
                    signature(
                        &row.signature.class,
                        &row.signature.name,
                        &row.signature.descriptor
                    ),
                    row.provenance.join(", ")
                )?;
            }
        }
        if !p.top_inlined.is_empty() {
            writeln!(
                w,
                "  Left to hybrid inlining, by excess. These are what a dispatch model would"
            )?;
            writeln!(
                w,
                "  cover; write one and re-run this report, no re-import needed:"
            )?;
            for row in &p.top_inlined {
                writeln!(
                    w,
                    "    {:>7} excess  {:<9}  {}{}",
                    row.signature.excess,
                    row.reason,
                    signature(
                        &row.signature.class,
                        &row.signature.name,
                        &row.signature.descriptor
                    ),
                    if row.provenance.is_empty() {
                        String::new()
                    } else {
                        format!("  [{}]", row.provenance.join(", "))
                    }
                )?;
            }
        }
        if !p.unmodelled_closures.is_empty() {
            writeln!(
                w,
                "  Closure-shaped and named by no model. A model here is the wrong tool -- these"
            )?;
            writeln!(
                w,
                "  are what hybrid inlining is for -- but they say where the residue is:"
            )?;
            signature_rows(w, &p.unmodelled_closures)?;
        }
        if !p.refused.is_empty() {
            writeln!(
                w,
                "  Dispatch models refused: a matched source or sink is in the target set, so"
            )?;
            writeln!(
                w,
                "  those bodies stay in the analysis and the sites take CHA:"
            )?;
            for row in &p.refused {
                writeln!(
                    w,
                    "    {}  <- {}",
                    signature(
                        &row.signature.class,
                        &row.signature.name,
                        &row.signature.descriptor
                    ),
                    row.endpoint
                )?;
            }
        }
    }

    if let Some(f) = &r.fan_in {
        writeln!(
            w,
            "\n-- fan-in (CHA call graph) ---------------------------------"
        )?;
        writeln!(w, "  methods with a caller     {:>12}", f.methods)?;
        distribution(w, "  callers/method", &f.calls_per_method)?;
        if let Some(d) = &f.edges_by_dispatch {
            writeln!(
                w,
                "  incoming edges by kind    {:>12} interface, {} virtual, {} super, {} direct",
                d.interface, d.virtual_, d.super_, f.direct_edges
            )?;
        }
        writeln!(
            w,
            "  Most-called methods (`iface` = callers reaching it through an interface):"
        )?;
        for row in &f.top {
            writeln!(
                w,
                "    {:>10} {:>10} iface  {}",
                row.callers,
                row.via_interface
                    .map_or_else(|| "-".to_string(), |n| n.to_string()),
                row.method
            )?;
        }
    }

    if let Some(rec) = &r.recursion {
        writeln!(
            w,
            "\n-- recursion ----------------------------------------------"
        )?;
        writeln!(w, "  graph nodes               {:>12}", rec.nodes)?;
        writeln!(
            w,
            "  deduplicated edges        {:>12}   per (caller function, target)",
            rec.edges
        )?;
        writeln!(w, "  self-recursive functions  {:>12}", rec.self_recursive)?;
        writeln!(w, "  cycles (SCCs > 1 member)  {:>12}", rec.nontrivial_sccs)?;
        writeln!(
            w,
            "  functions inside one      {:>12}   {}",
            rec.functions_in_nontrivial_sccs,
            pct(rec.functions_in_nontrivial_sccs, rec.nodes)
        )?;
        writeln!(w, "  largest cycle             {:>12}", rec.largest_scc)?;
        writeln!(
            w,
            "  Over the CHA graph, so this is the UPPER bound on recursion -- and it is the"
        )?;
        writeln!(
            w,
            "  bound inlining faces, which has to terminate against every target the"
        )?;
        writeln!(w, "  resolution admits.")?;
        if let Some(cv) = &rec.without_interface_edges {
            writeln!(
                w,
                "\n  The same graph with every interface-dispatched edge removed:"
            )?;
            writeln!(
                w,
                "    deduplicated edges      {:>12}   {} of them",
                cv.edges,
                pct(cv.edges, rec.edges)
            )?;
            writeln!(w, "    self-recursive          {:>12}", cv.self_recursive)?;
            writeln!(w, "    cycles (SCCs > 1)       {:>12}", cv.nontrivial_sccs)?;
            writeln!(
                w,
                "    functions inside one    {:>12}   {}",
                cv.functions_in_nontrivial_sccs,
                pct(cv.functions_in_nontrivial_sccs, rec.nodes)
            )?;
            writeln!(
                w,
                "    largest cycle           {:>12}   {} of the full graph's",
                cv.largest_scc,
                pct(cv.largest_scc, rec.largest_scc)
            )?;
            writeln!(
                w,
                "  If the largest cycle survives here, interface dispatch is not what makes"
            )?;
            writeln!(
                w,
                "  inlining non-terminating; if it collapses, it is, and that is a thing to fix"
            )?;
            writeln!(w, "  rather than a thing to inline through.")?;
        }
    } else {
        writeln!(
            w,
            "\n-- recursion ----------------------------------------------"
        )?;
        writeln!(
            w,
            "  Skipped (--no-recursion). It is the only section that builds the whole CHA call"
        )?;
        writeln!(
            w,
            "  graph, which on a large app is over a billion edges and dominates the run."
        )?;
    }
    Ok(())
}

fn signature_rows(w: &mut dyn Write, rows: &[SignatureRow]) -> Result<()> {
    writeln!(
        w,
        "    {:>7} {:>7} {:>7} {:>12} {:>12}  signature",
        "cha", "rta", "sites", "edges", "excess"
    )?;
    for row in rows {
        writeln!(
            w,
            "    {:>7} {:>7} {:>7} {:>12} {:>12}  {}",
            row.cha_targets,
            row.rta_targets,
            row.sites,
            row.edges,
            row.excess,
            signature(&row.class, &row.name, &row.descriptor)
        )?;
    }
    Ok(())
}

/// `1 thing` / `2 things`, for a count read inside a sentence.
fn plural(n: usize, noun: &str) -> String {
    if n == 1 {
        format!("{n} {noun}")
    } else {
        format!("{n} {noun}s")
    }
}

/// `Class.name(descriptor)` for Java, and the bare method name for Lua, which has neither a
/// declared receiver class nor overloading -- its signature key carries an empty class and an
/// empty descriptor, and printing them would render as a leading dot.
fn signature(class: &str, name: &str, descriptor: &str) -> String {
    let mut out = String::new();
    if !class.is_empty() {
        out.push_str(class);
        out.push('.');
    }
    out.push_str(name);
    out.push_str(descriptor);
    out
}

fn distribution(w: &mut dyn Write, label: &str, d: &Distribution) -> Result<()> {
    writeln!(
        w,
        "{label:<26}mean {:.2}   p50 {}   p90 {}   p99 {}   max {}",
        d.mean, d.p50, d.p90, d.p99, d.max
    )
}

fn pct(part: usize, whole: usize) -> String {
    if whole == 0 {
        return "     -".to_string();
    }
    format!("{:5.1}%", 100.0 * part as f64 / whole as f64)
}

fn pct_f(share: f64) -> String {
    format!("{:5.1}%", 100.0 * share)
}

fn ratio(part: usize, whole: usize) -> f64 {
    if whole == 0 {
        0.0
    } else {
        part as f64 / whole as f64
    }
}
