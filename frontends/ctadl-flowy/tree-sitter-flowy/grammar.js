/**
 * @file Tree-sitter grammar for flowy (.tnt), the test-case language of the ctadl analyzer.
 *
 * This is a port of `../src/flowy.pest` and should accept exactly the same language. Rule names
 * follow the pest grammar. Rules that pest has but that carry no information of their own here
 * (`def`, `stmt`, `terminator`, `p`, `ap_comma_list`, `summary_ap`) are hidden.
 *
 * Pest literals are not reserved words: `return`, `ptr`, `goto`, etc. are still valid `ident`s
 * (e.g. the `return` port in a summary, or a variable named `ptr`). `_name` reproduces that by
 * admitting each keyword as an `ident`. Nor do they need a word boundary: `returnx;` is
 * `return x;`; src/scanner.c handles that at the start of a statement, the only place where pest's
 * reading differs from the longest match.
 */

/// <reference types="tree-sitter-cli/dsl" />
// @ts-check

const KEYWORDS = ['var', 'def', 'where', 'summaries', 'byref', 'byval', 'goto', 'return', 'ptr'];

/** `rule ~ ("," ~ rule)*` */
const commaSep1 = (rule) => seq(rule, repeat(seq(',', rule)));

module.exports = grammar({
  name: 'flowy',

  extras: $ => [/[ \t\r\n]/, $.comment],

  // See src/scanner.c.
  externals: $ => [$._return_prefix, $._goto_prefix, $._error_sentinel],

  rules: {
    top: $ => repeat($._def),

    _def: $ => choice($.var_def, $.function_def),

    var_def: $ => seq('var', $._name, ';'),

    function_def: $ => seq(
      'def',
      field('name', $._name),
      $.params,
      optional($.return_arity),
      optional($.where_clause),
      $.block_list,
    ),

    block_list: $ => seq('{', repeat($.block), '}'),

    params: $ => seq('(', optional(commaSep1($.param)), ')'),

    param: $ => seq($._name, optional($.param_passing_style)),

    param_passing_style: _ => choice('byref', 'byval'),

    return_arity: $ => seq(':', $.int),

    where_clause: $ => seq('where', 'summaries', $.summaries),

    block: $ => seq(field('label', $._name), ':', repeat($._stmt), $._terminator),

    _terminator: $ => choice($.goto_stmt, $.return_stmt),

    _stmt: $ => choice($.update_stmt, $.assign_stmt, $.assign_call_stmt, $.call_stmt),

    update_stmt: $ => seq($.ap, '=', '[', $.ap, ':=', $.ap, ']', ';'),

    assign_stmt: $ => seq($.ap, '=', commaSep1($.ap), ';'),

    assign_call_stmt: $ => seq($.ap, '=', $.ap, $.actuals, ';'),

    call_stmt: $ => seq($.ap, $.actuals, ';'),

    goto_stmt: $ => seq(choice('goto', alias($._goto_prefix, 'goto')), commaSep1($._name), ';'),

    return_stmt: $ => seq(choice('return', alias($._return_prefix, 'return')), optional($.ap), ';'),

    _p: $ => choice($.star_p, $.field_p, $.offset_p),

    star_p: _ => seq('.', '*'),

    field_p: $ => seq('.', $._name),

    offset_p: $ => seq('.', '[', $.int, ']'),

    ap: $ => choice(
      $.function_ptr,
      seq($._name, repeat($._p)),
      $.int,
      $.string,
    ),

    function_ptr: $ => seq('ptr', '<', $._name, '>'),

    actuals: $ => seq('(', optional(commaSep1($.ap)), ')'),

    summary_entry: $ => seq($.ap, $.summary_op, $.ap),

    summary_op: _ => choice('<-', '</-'),

    summaries: $ => seq('[', optional(commaSep1($.summary_entry)), ']'),

    // `int` is not atomic in pest, so whitespace and comments may separate the sign and digits
    // (`- 1 2` is an int). `digits` is a named node so that error recovery cannot silently invent one.
    int: $ => seq(optional(choice('+', '-')), repeat1($.digits)),

    digits: _ => /[0-9]+/,

    _name: $ => choice($.ident, ...KEYWORDS.map(k => alias(k, $.ident))),

    ident: _ => /[A-Za-z_][A-Za-z0-9_]*/,

    string: $ => seq(
      '"',
      repeat(choice($.string_content, $.escape_sequence)),
      token.immediate('"'),
    ),

    string_content: _ => token.immediate(prec(1, /[^"\\]+/)),

    escape_sequence: _ => token.immediate(choice(
      /\\["\\\/bfnrt]/,
      /\\u[0-9a-fA-F]{4}/,
    )),

    comment: _ => token(seq('//', /[^\n]*/)),
  },
});
