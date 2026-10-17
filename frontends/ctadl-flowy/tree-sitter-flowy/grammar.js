/**
 * @file Tree-sitter grammar for flowy (.tnt), the test-case language of the ctadl analyzer.
 *
 * This is a port of the peg grammar in `../src/parse.rs` and should accept the same language,
 * except that it cannot reject an integer too large for 64 bits or a return arity above 255.
 * Node names follow the peg rules and the syntax tree types there (`path`, `field`, `offset`,
 * `star`, ...). Rules that carry no information of their own here (`def`, `stmt`, `terminator`,
 * `segment`) are hidden.
 *
 * Keywords are not reserved: `return`, `ptr`, `goto`, etc. are still valid `ident`s (e.g. the
 * `return` port in a summary, or a variable named `ptr`). `_name` reproduces that by admitting
 * each keyword as an `ident`. A keyword must not run into an identifier (`returnx` is an
 * identifier, never `return x`), which `word` gives us.
 */

/// <reference types="tree-sitter-cli/dsl" />
// @ts-check

const KEYWORDS = ['var', 'def', 'where', 'summaries', 'shared', 'shared_fields', 'goto', 'return', 'ptr'];

/** `rule ++ ","` */
const commaSep1 = (rule) => seq(rule, repeat(seq(',', rule)));

module.exports = grammar({
  name: 'flowy',

  extras: $ => [/[ \t\r\n]/, $.comment],

  word: $ => $.ident,

  rules: {
    program: $ => repeat($._def),

    _def: $ => choice($.var_def, $.function_def),

    var_def: $ => seq('var', field('name', $._name), ';'),

    function_def: $ => seq(
      'def',
      field('name', $._name),
      $.params,
      optional($.return_arity),
      optional($.where_clause),
      '{',
      repeat($.block),
      '}',
    ),

    params: $ => seq('(', optional(commaSep1($.param)), ')'),

    param: $ => seq(field('name', $._name), optional($.param_type)),

    param_type: _ => choice('shared_fields', 'shared'),

    // peg reads the arity as a `u8`, so it cannot be negative.
    return_arity: $ => seq(':', alias(/\+?[0-9]+/, $.int)),

    where_clause: $ => seq(
      'where',
      'summaries',
      '[',
      optional(commaSep1($.summary_entry)),
      ']',
    ),

    summary_entry: $ => seq(
      field('dest', $.path),
      field('flow', $.summary_op),
      field('source', $.path),
    ),

    summary_op: _ => choice('<-', '</-'),

    block: $ => seq(field('label', $._name), ':', repeat($._stmt), $._terminator),

    _stmt: $ => choice($.update_stmt, $.assign_stmt, $.assign_call_stmt, $.call_stmt),

    // `dest = [source := value];`
    update_stmt: $ => seq(
      field('dest', $.path),
      '=',
      '[',
      field('source', $.path),
      ':=',
      field('value', $.ap),
      ']',
      ';',
    ),

    assign_stmt: $ => seq(field('dest', $.path), '=', commaSep1(field('source', $.ap)), ';'),

    assign_call_stmt: $ => seq(
      field('dest', $.path),
      '=',
      field('callee', $.path),
      $.actuals,
      ';',
    ),

    call_stmt: $ => seq(field('callee', $.path), $.actuals, ';'),

    _terminator: $ => choice($.goto_stmt, $.return_stmt),

    goto_stmt: $ => seq('goto', commaSep1(field('target', $._name)), ';'),

    return_stmt: $ => seq('return', optional(field('value', $.ap)), ';'),

    actuals: $ => seq('(', optional(commaSep1($.ap)), ')'),

    ap: $ => choice($.function_ptr, $.path, $.int, $.string),

    function_ptr: $ => seq('ptr', '<', field('name', $._name), '>'),

    path: $ => seq(field('base', $._name), repeat($._segment)),

    _segment: $ => choice($.star, $.field, $.offset),

    star: _ => seq('.', '*'),

    field: $ => seq('.', $._name),

    offset: $ => seq('.', '[', $.int, ']'),

    int: _ => /[+-]?[0-9]+/,

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
