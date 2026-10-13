// External scanner for flowy.
//
// Pest literals have no word boundary, so at the start of a statement pest reads `returnx;` as
// `return x;` and `gotoend;` as `goto end;`. Pest first tries the statement alternatives, which
// read `returnx` as an access path, and falls back to the terminator only when they fail. A
// statement is an access path followed by `=` or `(`, so the access path that `returnx` begins
// decides it: if `=` or `(` follows it, `returnx` is an identifier (left to the internal lexer);
// otherwise `return` is the keyword. `goto` works the same way.

#include "tree_sitter/parser.h"

#include <stdbool.h>

enum TokenType {
  RETURN_PREFIX,
  GOTO_PREFIX,
  ERROR_SENTINEL,
};

void *tree_sitter_flowy_external_scanner_create(void) { return NULL; }
void tree_sitter_flowy_external_scanner_destroy(void *payload) {}
unsigned tree_sitter_flowy_external_scanner_serialize(void *payload, char *buffer) { return 0; }
void tree_sitter_flowy_external_scanner_deserialize(void *payload, const char *buffer,
                                                    unsigned length) {}

static bool is_ident_start(int32_t c) {
  return (c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z') || c == '_';
}

static bool is_digit(int32_t c) { return c >= '0' && c <= '9'; }

static bool is_ident_char(int32_t c) { return is_ident_start(c) || is_digit(c); }

static bool is_space(int32_t c) { return c == ' ' || c == '\t' || c == '\r' || c == '\n'; }

static void advance(TSLexer *lexer) { lexer->advance(lexer, false); }

// Skips whitespace and `//` comments. Returns false if it stops on a `/` that does not begin a
// comment, which nothing in the grammar can follow.
static bool skip_trivia(TSLexer *lexer) {
  for (;;) {
    if (is_space(lexer->lookahead)) {
      advance(lexer);
    } else if (lexer->lookahead == '/') {
      advance(lexer);
      if (lexer->lookahead != '/') return false;
      while (lexer->lookahead != '\n' && !lexer->eof(lexer)) advance(lexer);
    } else {
      return true;
    }
  }
}

// Consumes `p*` (`.*`, `.ident`, `.[int]`), stopping where it no longer matches.
static void skip_path(TSLexer *lexer) {
  for (;;) {
    if (!skip_trivia(lexer) || lexer->lookahead != '.') return;
    advance(lexer);
    if (!skip_trivia(lexer)) return;
    if (lexer->lookahead == '*') {
      advance(lexer);
    } else if (is_ident_start(lexer->lookahead)) {
      while (is_ident_char(lexer->lookahead)) advance(lexer);
    } else if (lexer->lookahead == '[') {
      advance(lexer);
      if (!skip_trivia(lexer)) return;
      if (lexer->lookahead == '+' || lexer->lookahead == '-') {
        advance(lexer);
        if (!skip_trivia(lexer)) return;
      }
      if (!is_digit(lexer->lookahead)) return;
      while (is_digit(lexer->lookahead)) {
        while (is_digit(lexer->lookahead)) advance(lexer);
        if (!skip_trivia(lexer)) return;
      }
      if (lexer->lookahead != ']') return;
      advance(lexer);
    } else {
      return;
    }
  }
}

static bool scan_prefix(TSLexer *lexer, const char *keyword) {
  for (const char *k = keyword; *k; k++) {
    if (lexer->lookahead != *k) return false;
    advance(lexer);
  }
  // Without a glued identifier the internal lexer handles the keyword.
  if (!is_ident_char(lexer->lookahead)) return false;
  lexer->mark_end(lexer);
  while (is_ident_char(lexer->lookahead)) advance(lexer);
  skip_path(lexer);
  if (skip_trivia(lexer) && (lexer->lookahead == '=' || lexer->lookahead == '(')) return false;
  return true;
}

bool tree_sitter_flowy_external_scanner_scan(void *payload, TSLexer *lexer,
                                             const bool *valid_symbols) {
  // Error recovery marks every external token valid.
  if (valid_symbols[ERROR_SENTINEL]) return false;
  if (!valid_symbols[RETURN_PREFIX] && !valid_symbols[GOTO_PREFIX]) return false;

  while (is_space(lexer->lookahead)) lexer->advance(lexer, true);

  if (lexer->lookahead == 'r' && valid_symbols[RETURN_PREFIX]) {
    lexer->result_symbol = RETURN_PREFIX;
    return scan_prefix(lexer, "return");
  }
  if (lexer->lookahead == 'g' && valid_symbols[GOTO_PREFIX]) {
    lexer->result_symbol = GOTO_PREFIX;
    return scan_prefix(lexer, "goto");
  }
  return false;
}
