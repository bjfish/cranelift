
// ====--------------------------------------------------------------------------------------====//
//
// Lexical analysis for .cton files.
//
// ====--------------------------------------------------------------------------------------====//

use std::str::CharIndices;
use std::u16;
use cretonne::ir::types;
use cretonne::ir::{Value, Ebb};
use error::Location;

/// A Token returned from the `Lexer`.
///
/// Some variants may contains references to the original source text, so the `Token` has the same
/// lifetime as the source.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Token<'a> {
    Comment(&'a str),
    LPar, // '('
    RPar, // ')'
    LBrace, // '{'
    RBrace, // '}'
    Comma, // ','
    Dot, // '.'
    Colon, // ':'
    Equal, // '='
    Arrow, // '->'
    Float(&'a str), // Floating point immediate
    Integer(&'a str), // Integer immediate
    Type(types::Type), // i32, f32, b32x4, ...
    Value(Value), // v12, vx7
    Ebb(Ebb), // ebb3
    StackSlot(u32), // ss3
    JumpTable(u32), // jt2
    FuncRef(u32), // fn2
    SigRef(u32), // sig2
    Identifier(&'a str), // Unrecognized identifier (opcode, enumerator, ...)
}

/// A `Token` with an associated location.
#[derive(Debug, PartialEq, Eq)]
pub struct LocatedToken<'a> {
    pub token: Token<'a>,
    pub location: Location,
}

/// Wrap up a `Token` with the given location.
fn token<'a>(token: Token<'a>, loc: Location) -> Result<LocatedToken<'a>, LocatedError> {
    Ok(LocatedToken {
        token: token,
        location: loc,
    })
}

/// An error from the lexical analysis.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    InvalidChar,
}

/// An `Error` with an associated Location.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocatedError {
    pub error: Error,
    pub location: Location,
}

/// Wrap up an `Error` with the given location.
fn error<'a>(error: Error, loc: Location) -> Result<LocatedToken<'a>, LocatedError> {
    Err(LocatedError {
        error: error,
        location: loc,
    })
}

/// Get the number of decimal digits at the end of `s`.
fn trailing_digits(s: &str) -> usize {
    // It's faster to iterate backwards over bytes, and we're only counting ASCII digits.
    s.as_bytes().iter().rev().cloned().take_while(|&b| b'0' <= b && b <= b'9').count()
}

/// Pre-parse a supposed entity name by splitting it into two parts: A head of lowercase ASCII
/// letters and numeric tail.
pub fn split_entity_name(name: &str) -> Option<(&str, u32)> {
    let (head, tail) = name.split_at(name.len() - trailing_digits(name));
    if tail.len() > 1 && tail.starts_with('0') {
        None
    } else {
        tail.parse().ok().map(|n| (head, n))
    }
}

/// Lexical analysis.
///
/// A `Lexer` reads text from a `&str` and provides a sequence of tokens.
///
/// Also keep track of a line number for error reporting.
///
pub struct Lexer<'a> {
    // Complete source being processed.
    source: &'a str,

    // Iterator into `source`.
    chars: CharIndices<'a>,

    // Next character to be processed, or `None` at the end.
    lookahead: Option<char>,

    // Index into `source` of lookahead character.
    pos: usize,

    // Current line number.
    line_number: usize,
}

impl<'a> Lexer<'a> {
    pub fn new(s: &'a str) -> Lexer {
        let mut lex = Lexer {
            source: s,
            chars: s.char_indices(),
            lookahead: None,
            pos: 0,
            line_number: 1,
        };
        // Advance to the first char.
        lex.next_ch();
        lex
    }

    // Advance to the next character.
    // Return the next lookahead character, or None when the end is encountered.
    // Always update cur_ch to reflect
    fn next_ch(&mut self) -> Option<char> {
        if self.lookahead == Some('\n') {
            self.line_number += 1;
        }
        match self.chars.next() {
            Some((idx, ch)) => {
                self.pos = idx;
                self.lookahead = Some(ch);
            }
            None => {
                self.pos = self.source.len();
                self.lookahead = None;
            }
        }
        self.lookahead
    }

    // Get the location corresponding to `lookahead`.
    fn loc(&self) -> Location {
        Location { line_number: self.line_number }
    }

    // Starting from `lookahead`, are we looking at `prefix`?
    fn looking_at(&self, prefix: &str) -> bool {
        self.source[self.pos..].starts_with(prefix)
    }

    // Scan a single-char token.
    fn scan_char(&mut self, tok: Token<'a>) -> Result<LocatedToken<'a>, LocatedError> {
        assert!(self.lookahead != None);
        let loc = self.loc();
        self.next_ch();
        token(tok, loc)
    }

    // Scan a multi-char token.
    fn scan_chars(&mut self,
                  count: usize,
                  tok: Token<'a>)
                  -> Result<LocatedToken<'a>, LocatedError> {
        let loc = self.loc();
        for _ in 0..count {
            assert!(self.lookahead != None);
            self.next_ch();
        }
        token(tok, loc)
    }

    /// Get the rest of the current line.
    /// The next token returned by `next()` will be from the following lines.
    pub fn rest_of_line(&mut self) -> &'a str {
        let begin = self.pos;
        loop {
            match self.next_ch() {
                None | Some('\n') => return &self.source[begin..self.pos],
                _ => {}
            }
        }
    }

    // Scan a comment extending to the end of the current line.
    fn scan_comment(&mut self) -> Result<LocatedToken<'a>, LocatedError> {
        let loc = self.loc();
        let text = self.rest_of_line();
        return token(Token::Comment(text), loc);
    }

    // Scan a number token which can represent either an integer or floating point number.
    //
    // Accept the following forms:
    //
    // - `10`: Integer
    // - `-10`: Integer
    // - `0xff_00`: Integer
    // - `0.0`: Float
    // - `0x1.f`: Float
    // - `-0x2.4`: Float
    // - `0x0.4p-34`: Float
    //
    // This function does not filter out all invalid numbers. It depends in the context-sensitive
    // decoding of the text for that. For example, the number of allowed digits an an Ieee32` and
    // an `Ieee64` constant are different.
    fn scan_number(&mut self) -> Result<LocatedToken<'a>, LocatedError> {
        let begin = self.pos;
        let loc = self.loc();
        let mut is_float = false;

        // Skip a leading sign.
        if self.lookahead == Some('-') {
            self.next_ch();
        }

        // Check for NaNs with payloads.
        if self.looking_at("NaN:") || self.looking_at("sNaN:") {
            // Skip the `NaN:` prefix, the loop below won't accept it.
            // We expect a hexadecimal number to follow the colon.
            while self.next_ch() != Some(':') {}
            is_float = true;
        } else if self.looking_at("NaN") || self.looking_at("Inf") {
            // This is Inf or a default quiet NaN.
            is_float = true;
        }

        // Look for the end of this number. Detect the radix point if there is one.
        loop {
            match self.next_ch() {
                Some('-') | Some('_') => {}
                Some('.') => is_float = true,
                Some(ch) if ch.is_alphanumeric() => {}
                _ => break,
            }
        }
        let text = &self.source[begin..self.pos];
        if is_float {
            token(Token::Float(text), loc)
        } else {
            token(Token::Integer(text), loc)
        }
    }

    // Scan a 'word', which is an identifier-like sequence of characters beginning with '_' or an
    // alphabetic char, followed by zero or more alphanumeric or '_' characters.
    fn scan_word(&mut self) -> Result<LocatedToken<'a>, LocatedError> {
        let begin = self.pos;
        let loc = self.loc();

        assert!(self.lookahead == Some('_') || self.lookahead.unwrap().is_alphabetic());
        loop {
            match self.next_ch() {
                Some('_') => {}
                Some(ch) if ch.is_alphanumeric() => {}
                _ => break,
            }
        }
        let text = &self.source[begin..self.pos];

        // Look for numbered well-known entities like ebb15, v45, ...
        token(split_entity_name(text)
                  .and_then(|(prefix, number)| {
                      Self::numbered_entity(prefix, number)
                          .or_else(|| Self::value_type(text, prefix, number))
                  })
                  .unwrap_or(Token::Identifier(text)),
              loc)
    }

    // If prefix is a well-known entity prefix and suffix is a valid entity number, return the
    // decoded token.
    fn numbered_entity(prefix: &str, number: u32) -> Option<Token<'a>> {
        match prefix {
            "v" => Value::direct_with_number(number).map(|v| Token::Value(v)),
            "vx" => Value::table_with_number(number).map(|v| Token::Value(v)),
            "ebb" => Ebb::with_number(number).map(|ebb| Token::Ebb(ebb)),
            "ss" => Some(Token::StackSlot(number)),
            "jt" => Some(Token::JumpTable(number)),
            "fn" => Some(Token::FuncRef(number)),
            "sig" => Some(Token::SigRef(number)),
            _ => None,
        }
    }

    // Recognize a scalar or vector type.
    fn value_type(text: &str, prefix: &str, number: u32) -> Option<Token<'a>> {
        let is_vector = prefix.ends_with('x');
        let scalar = if is_vector {
            &prefix[0..prefix.len() - 1]
        } else {
            text
        };
        let base_type = match scalar {
            "i8" => types::I8,
            "i16" => types::I16,
            "i32" => types::I32,
            "i64" => types::I64,
            "f32" => types::F32,
            "f64" => types::F64,
            "b1" => types::B1,
            "b8" => types::B8,
            "b16" => types::B16,
            "b32" => types::B32,
            "b64" => types::B64,
            _ => return None,
        };
        if is_vector {
            if number <= u16::MAX as u32 {
                base_type.by(number as u16).map(|t| Token::Type(t))
            } else {
                None
            }
        } else {
            Some(Token::Type(base_type))
        }
    }

    /// Get the next token or a lexical error.
    ///
    /// Return None when the end of the source is encountered.
    pub fn next(&mut self) -> Option<Result<LocatedToken<'a>, LocatedError>> {
        loop {
            let loc = self.loc();
            return match self.lookahead {
                None => None,
                Some(';') => Some(self.scan_comment()),
                Some('(') => Some(self.scan_char(Token::LPar)),
                Some(')') => Some(self.scan_char(Token::RPar)),
                Some('{') => Some(self.scan_char(Token::LBrace)),
                Some('}') => Some(self.scan_char(Token::RBrace)),
                Some(',') => Some(self.scan_char(Token::Comma)),
                Some('.') => Some(self.scan_char(Token::Dot)),
                Some(':') => Some(self.scan_char(Token::Colon)),
                Some('=') => Some(self.scan_char(Token::Equal)),
                Some('-') => {
                    if self.looking_at("->") {
                        Some(self.scan_chars(2, Token::Arrow))
                    } else {
                        Some(self.scan_number())
                    }
                }
                Some(ch) if ch.is_digit(10) => Some(self.scan_number()),
                Some(ch) if ch.is_alphabetic() => Some(self.scan_word()),
                Some(ch) if ch.is_whitespace() => {
                    self.next_ch();
                    continue;
                }
                _ => {
                    // Skip invalid char, return error.
                    self.next_ch();
                    Some(error(Error::InvalidChar, loc))
                }
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::trailing_digits;
    use super::*;
    use cretonne::ir::types;
    use cretonne::ir::{Value, Ebb};
    use error::Location;

    #[test]
    fn digits() {
        assert_eq!(trailing_digits(""), 0);
        assert_eq!(trailing_digits("x"), 0);
        assert_eq!(trailing_digits("0x"), 0);
        assert_eq!(trailing_digits("x1"), 1);
        assert_eq!(trailing_digits("1x1"), 1);
        assert_eq!(trailing_digits("1x01"), 2);
    }

    #[test]
    fn entity_name() {
        assert_eq!(split_entity_name(""), None);
        assert_eq!(split_entity_name("x"), None);
        assert_eq!(split_entity_name("x+"), None);
        assert_eq!(split_entity_name("x+1"), Some(("x+", 1)));
        assert_eq!(split_entity_name("x-1"), Some(("x-", 1)));
        assert_eq!(split_entity_name("1"), Some(("", 1)));
        assert_eq!(split_entity_name("x1"), Some(("x", 1)));
        assert_eq!(split_entity_name("xy0"), Some(("xy", 0)));
        // Reject this non-canonical form.
        assert_eq!(split_entity_name("inst01"), None);
    }

    fn token<'a>(token: Token<'a>, line: usize) -> Option<Result<LocatedToken<'a>, LocatedError>> {
        Some(super::token(token, Location { line_number: line }))
    }

    fn error<'a>(error: Error, line: usize) -> Option<Result<LocatedToken<'a>, LocatedError>> {
        Some(super::error(error, Location { line_number: line }))
    }

    #[test]
    fn make_lexer() {
        let mut l1 = Lexer::new("");
        let mut l2 = Lexer::new(" ");
        let mut l3 = Lexer::new("\n ");

        assert_eq!(l1.next(), None);
        assert_eq!(l2.next(), None);
        assert_eq!(l3.next(), None);
    }

    #[test]
    fn lex_comment() {
        let mut lex = Lexer::new("; hello");
        assert_eq!(lex.next(), token(Token::Comment("; hello"), 1));
        assert_eq!(lex.next(), None);

        lex = Lexer::new("\n  ;hello\n;foo");
        assert_eq!(lex.next(), token(Token::Comment(";hello"), 2));
        assert_eq!(lex.next(), token(Token::Comment(";foo"), 3));
        assert_eq!(lex.next(), None);

        // Scan a comment after an invalid char.
        let mut lex = Lexer::new("#; hello");
        assert_eq!(lex.next(), error(Error::InvalidChar, 1));
        assert_eq!(lex.next(), token(Token::Comment("; hello"), 1));
        assert_eq!(lex.next(), None);
    }

    #[test]
    fn lex_chars() {
        let mut lex = Lexer::new("(); hello\n = :{, }.");
        assert_eq!(lex.next(), token(Token::LPar, 1));
        assert_eq!(lex.next(), token(Token::RPar, 1));
        assert_eq!(lex.next(), token(Token::Comment("; hello"), 1));
        assert_eq!(lex.next(), token(Token::Equal, 2));
        assert_eq!(lex.next(), token(Token::Colon, 2));
        assert_eq!(lex.next(), token(Token::LBrace, 2));
        assert_eq!(lex.next(), token(Token::Comma, 2));
        assert_eq!(lex.next(), token(Token::RBrace, 2));
        assert_eq!(lex.next(), token(Token::Dot, 2));
        assert_eq!(lex.next(), None);
    }

    #[test]
    fn lex_numbers() {
        let mut lex = Lexer::new(" 0 2_000 -1,0xf -0x0 0.0 0x0.4p-34");
        assert_eq!(lex.next(), token(Token::Integer("0"), 1));
        assert_eq!(lex.next(), token(Token::Integer("2_000"), 1));
        assert_eq!(lex.next(), token(Token::Integer("-1"), 1));
        assert_eq!(lex.next(), token(Token::Comma, 1));
        assert_eq!(lex.next(), token(Token::Integer("0xf"), 1));
        assert_eq!(lex.next(), token(Token::Integer("-0x0"), 1));
        assert_eq!(lex.next(), token(Token::Float("0.0"), 1));
        assert_eq!(lex.next(), token(Token::Float("0x0.4p-34"), 1));
        assert_eq!(lex.next(), None);
    }

    #[test]
    fn lex_identifiers() {
        let mut lex = Lexer::new("v0 v00 vx01 ebb1234567890 ebb5234567890 v1x vx1 vxvx4 \
                                  function0 function b1 i32x4 f32x5");
        assert_eq!(lex.next(),
                   token(Token::Value(Value::direct_with_number(0).unwrap()), 1));
        assert_eq!(lex.next(), token(Token::Identifier("v00"), 1));
        assert_eq!(lex.next(), token(Token::Identifier("vx01"), 1));
        assert_eq!(lex.next(),
                   token(Token::Ebb(Ebb::with_number(1234567890).unwrap()), 1));
        assert_eq!(lex.next(), token(Token::Identifier("ebb5234567890"), 1));
        assert_eq!(lex.next(), token(Token::Identifier("v1x"), 1));
        assert_eq!(lex.next(),
                   token(Token::Value(Value::table_with_number(1).unwrap()), 1));
        assert_eq!(lex.next(), token(Token::Identifier("vxvx4"), 1));
        assert_eq!(lex.next(), token(Token::Identifier("function0"), 1));
        assert_eq!(lex.next(), token(Token::Identifier("function"), 1));
        assert_eq!(lex.next(), token(Token::Type(types::B1), 1));
        assert_eq!(lex.next(), token(Token::Type(types::I32.by(4).unwrap()), 1));
        assert_eq!(lex.next(), token(Token::Identifier("f32x5"), 1));
        assert_eq!(lex.next(), None);
    }
}