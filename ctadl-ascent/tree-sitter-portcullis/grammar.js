/**
 * @file Tree-sitter grammar for Portcullis, the ctadl model language (.ctadl).
 *
 * This is a port of the peg grammar in `../src/models/portcullis/parse.rs` and should accept the
 * same language, except that it cannot reject an integer out of range: a literal or offset that
 * does not fit 64 bits, or an argument index that does not fit 16 (peg parses it and the builder
 * rejects it). Node names follow the peg rules (`rule`, `head_atom`, `anchor`,
 * `out_rel`, `flow`, `port`, `port_base`, `rel_atom`, `attr_op`, ...). Rules that carry no
 * information of their own here (`head_arg`, `rel_arg`, `term`, `literal`, `rhs`, `arg_sel`,
 * `path_seg`) are hidden, and `or_expr`/`and_expr` appear only when they have two or more
 * operands. Attributes in heads (`name = literal`) and in body atoms (`name op rhs`) are both
 * `attr`; the body ones carry an `op` field.
 *
 * Names are not reserved by the grammar: `return`, `arg`, `true`, `source`, `_`, etc. are all
 * valid `lname`s (the checker, not the parser, rejects most of them), so `name` admits each
 * keyword. peg's `!name_char()` after a keyword (`returnx` is a name, never `return x`) is what
 * `word` gives us. `lname`'s interior hyphens (`qualified-id`) are immediate tokens after the
 * word, rather than part of it, so that `X in-1` lexes `in` as the operator, as peg does.
 */

/// <reference types="tree-sitter-cli/dsl" />
// @ts-check

const OUT_RELS = [
  'source',
  'sink',
  'propagation',
  'bridge',
  'access_paths',
  'dispatch',
  'skip_analysis',
  'mem2reg_full',
  'mem2reg_window',
  'frame_store',
  'frame_update',
];

// Every keyword that is also an `lname` in peg.
const NAME_KEYWORDS = [...OUT_RELS, 'return', 'arg', 'param', 'local', 'true', 'false', 'in', '_'];

/** `rule ++ ","` */
const commaSep1 = (rule) => seq(rule, repeat(seq(',', rule)));

module.exports = grammar({
  name: 'portcullis',

  extras: $ => [/[ \t\r\n]/, $.comment],

  word: $ => $._word,

  rules: {
    program: $ => repeat($.rule),

    // peg: `rule_stmt`.
    rule: $ => seq(commaSep1(field('head', $.head_atom)), optional($.body), ';'),

    body: $ => seq(':-', commaSep1($._or_expr)),

    // -----------------------------------------------------------------------
    // Heads
    // -----------------------------------------------------------------------

    head_atom: $ => seq(
      optional(field('anchor', $.anchor)),
      field('relation', $.out_rel),
      '(',
      optional(commaSep1($._head_arg)),
      ')',
    ),

    out_rel: _ => choice(...OUT_RELS),

    anchor: $ => seq($._term, '::'),

    // peg tries these in order; each alternative's follow set tells them apart here.
    _head_arg: $ => choice(
      alias($.head_attr, $.attr),
      $.flow,
      $.port,
      $._literal,
      $.var,
    ),

    head_attr: $ => seq(field('name', $.name), '=', field('value', $._literal)),

    flow: $ => seq(field('left', $.port), field('op', $.flow_op), field('right', $.port)),

    flow_op: _ => choice('<->', '<-', '->'),

    // -----------------------------------------------------------------------
    // Ports
    // -----------------------------------------------------------------------

    // peg: `port_expr`.
    port: $ => seq(
      optional(field('anchor', $.anchor)),
      field('base', $.port_base),
      repeat($._path_seg),
    ),

    port_base: $ => choice(
      'return',
      seq(choice('arg', 'param'), '(', $._arg_sel, ')'),
      seq('local', '(', $.var, ')'),
    ),

    _arg_sel: $ => choice($.int, $.wildcard, $.var),

    // `.foo`, `[2]`, `.[2]`, `."weird.field[0]"`.
    _path_seg: $ => choice($.offset, $.symbol),

    offset: $ => seq(optional('.'), '[', $.int, ']'),

    symbol: $ => seq('.', choice($.string, $.segment)),

    // peg: `seg_char()+`.
    segment: _ => /[A-Za-z0-9_$%*#<>-]+/,

    // -----------------------------------------------------------------------
    // Bodies
    // -----------------------------------------------------------------------

    _or_expr: $ => choice($.or_expr, $._and_expr),

    or_expr: $ => seq($._and_expr, repeat1(seq('||', $._and_expr))),

    _and_expr: $ => choice($.and_expr, $._unary_expr),

    and_expr: $ => seq($._unary_expr, repeat1(seq('&&', $._unary_expr))),

    // peg counts the `!`s of one `unary_expr`; here each is a `negation`.
    _unary_expr: $ => choice($.negation, $._atom_expr),

    negation: $ => seq('!', $._unary_expr),

    _atom_expr: $ => choice($.paren, $.rel_atom, $.test),

    paren: $ => seq('(', $._or_expr, ')'),

    // `subclass*` and `subclass+` are relation names; peg reads the suffix with no space.
    rel_atom: $ => seq(
      field('name', $.name),
      optional(field('closure', alias(token.immediate(choice('*', '+')), $.closure))),
      '(',
      optional(commaSep1($._rel_arg)),
      ')',
    ),

    _rel_arg: $ => choice($.attr, $._term),

    attr: $ => seq(field('name', $.name), field('op', $.attr_op), field('value', $._rhs)),

    test: $ => seq(field('left', $._term), field('op', $.attr_op), field('right', $._rhs)),

    attr_op: _ => choice('<=', '>=', '!=', '<-', '<', '>', '=', 'in'),

    _rhs: $ => choice($.set, $._term),

    set: $ => seq('{', optional(commaSep1($._literal)), '}'),

    // -----------------------------------------------------------------------
    // Terminals
    // -----------------------------------------------------------------------

    _term: $ => choice($._literal, $.var, $.wildcard),

    _literal: $ => choice($.string, $.int, $.bool),

    bool: _ => choice('true', 'false'),

    wildcard: _ => '_',

    var: _ => /[A-Z][A-Za-z0-9_]*/,

    // peg: `lname`.
    name: $ => seq(
      choice($._word, ...NAME_KEYWORDS),
      repeat(token.immediate(/-[A-Za-z0-9_]+/)),
    ),

    _word: _ => /[a-z_][A-Za-z0-9_]*/,

    int: _ => /[+-]?[0-9]+/,

    string: $ => seq(
      '"',
      repeat(choice($.string_content, $.escape_sequence)),
      token.immediate('"'),
    ),

    string_content: _ => token.immediate(prec(1, /[^"\\]+/)),

    // peg takes any character after a backslash, newline included.
    escape_sequence: _ => token.immediate(/\\[\s\S]/),

    comment: _ => token(choice(
      seq('//', /[^\n]*/),
      seq('/*', /[^*]*\*+([^/*][^*]*\*+)*/, '/'),
    )),
  },
});
