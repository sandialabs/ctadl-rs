//! Reflection type-name grammar parser.
//!
//! Port of `Mono.Cecil/TypeParser.cs` — needed for `typeof(...)` arguments
//! inside custom-attribute blobs, serialized enum type names, custom
//! marshalers, and the `GetType(name, runtimeName: true)` API.

use crate::error::Result;
use crate::model::handles::TypeDef;
use crate::model::module::Module;

/// One parsed type specification suffix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Spec {
    Ptr,
    ByRef,
    SzArray,
    /// Multi-dimensional array of the given rank (rank 1 = `[*]`).
    Array(u32),
}

/// A parsed reflection type name (Cecil `TypeParser.Type`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ParsedType {
    /// Top-level `Namespace.Name`.
    pub fullname: String,
    pub nested_names: Vec<String>,
    /// The sum of the segments' arities, an `int` as in Cecil: a segment
    /// may give a negative one, and the sum wraps.
    pub arity: i32,
    pub specs: Vec<Spec>,
    pub generic_arguments: Vec<ParsedType>,
    /// The assembly part of a fully-qualified name.
    pub assembly: Option<String>,
}

/// The most generic parameters a type can have: GenericParam's Number
/// column is a `u16`. A name with a larger arity names no type, and Cecil
/// would add that many empty arguments one at a time.
const MAX_ARITY: i32 = u16::MAX as i32;

struct Parser<'s> {
    chars: Vec<char>,
    position: usize,
    _s: std::marker::PhantomData<&'s ()>,
}

impl ParsedType {
    /// `None` for an empty name, where Cecil returns null, or for one whose
    /// arity is over [`MAX_ARITY`].
    pub fn parse(fullname: &str) -> Option<ParsedType> {
        if fullname.is_empty() {
            return None;
        }
        let mut parser = Parser {
            chars: fullname.chars().collect(),
            position: 0,
            _s: std::marker::PhantomData,
        };
        parser.parse_type(true)
    }

    /// Renders the Cecil `TypeReference.FullName` this parsed name would
    /// produce (nested via `/`, generic instances via `<...>`, then specs).
    pub fn cecil_full_name(&self) -> String {
        let mut s = self.fullname.clone();
        for nested in &self.nested_names {
            s.push('/');
            s.push_str(nested);
        }
        if !self.generic_arguments.is_empty() {
            s.push('<');
            for (i, arg) in self.generic_arguments.iter().enumerate() {
                if i > 0 {
                    s.push(',');
                }
                s.push_str(&arg.cecil_full_name());
            }
            s.push('>');
        }
        for spec in &self.specs {
            match spec {
                Spec::Ptr => s.push('*'),
                Spec::ByRef => s.push('&'),
                Spec::SzArray => s.push_str("[]"),
                Spec::Array(rank) => {
                    s.push('[');
                    // ArrayType.Suffix with unsized dimensions: rank 1 => "[]"
                    // would be a vector; TypeParser creates rank dimensions
                    // with no bounds, so rank >= 2 prints commas only.
                    for _ in 1..*rank {
                        s.push(',');
                    }
                    s.push(']');
                }
            }
        }
        s
    }

    /// Resolves the (element) type to an in-module TypeDef, mirroring
    /// `TypeParser.TryGetDefinition`: only when the name is not qualified with
    /// a foreign assembly.
    pub fn resolve_in_module<'a>(&self, module: &'a Module<'a>) -> Result<Option<TypeDef<'a>>> {
        if let Some(assembly) = &self.assembly {
            let matches_self = match module.assembly_name()? {
                Some(name) => name.full_name() == *assembly,
                None => false,
            };
            if !matches_self && !assembly.is_empty() {
                return Ok(None);
            }
        }
        let Some(mut ty) = module.get_type(&self.fullname)? else {
            return Ok(None);
        };
        for nested_name in &self.nested_names {
            let Some(nested) = ty.nested_type(nested_name)? else {
                return Ok(None);
            };
            ty = nested;
        }
        Ok(Some(ty))
    }
}

impl Parser<'_> {
    fn parse_type(&mut self, fq_name: bool) -> Option<ParsedType> {
        let mut ty = ParsedType {
            fullname: self.parse_part(),
            ..Default::default()
        };
        while self.try_parse('+') {
            ty.nested_names.push(self.parse_part());
        }

        // TryGetArity: sum the arities encoded in each name segment.
        let mut arity: i32 = 0;
        for name in std::iter::once(&ty.fullname).chain(&ty.nested_names) {
            if let Some(idx) = name.rfind('`')
                && let Some(n) = parse_int32(&name[idx + 1..])
            {
                arity = arity.wrapping_add(n);
            }
        }
        ty.arity = arity;

        if arity > MAX_ARITY {
            return None;
        }
        if arity > 0 {
            ty.generic_arguments = self.parse_generic_arguments(arity)?;
        }
        ty.specs = self.parse_specs();
        if fq_name {
            ty.assembly = Some(self.parse_assembly_name());
        }
        Some(ty)
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.position).copied()
    }

    fn parse_part(&mut self) -> String {
        let mut part = String::new();
        while let Some(c) = self.peek() {
            if is_delimiter(c) {
                break;
            }
            if c == '\\' {
                self.position += 1;
            }
            if let Some(c) = self.peek() {
                part.push(c);
                self.position += 1;
            }
        }
        part
    }

    fn try_parse(&mut self, chr: char) -> bool {
        if self.peek() == Some(chr) {
            self.position += 1;
            true
        } else {
            false
        }
    }

    fn skip_whitespace(&mut self) {
        while self.peek().is_some_and(char::is_whitespace) {
            self.position += 1;
        }
    }

    fn parse_specs(&mut self) -> Vec<Spec> {
        let mut specs = Vec::new();
        while let Some(c) = self.peek() {
            match c {
                '*' => {
                    self.position += 1;
                    specs.push(Spec::Ptr);
                }
                '&' => {
                    self.position += 1;
                    specs.push(Spec::ByRef);
                }
                '[' => {
                    self.position += 1;
                    match self.peek() {
                        Some(']') => {
                            self.position += 1;
                            specs.push(Spec::SzArray);
                        }
                        Some('*') => {
                            self.position += 1;
                            specs.push(Spec::Array(1));
                            self.try_parse(']');
                        }
                        _ => {
                            let mut rank = 1;
                            while self.try_parse(',') {
                                rank += 1;
                            }
                            specs.push(Spec::Array(rank));
                            self.try_parse(']');
                        }
                    }
                }
                _ => return specs,
            }
        }
        specs
    }

    fn parse_generic_arguments(&mut self, arity: i32) -> Option<Vec<ParsedType>> {
        let mut args = Vec::new();
        if self.peek() != Some('[') {
            return Some(args);
        }
        self.try_parse('[');
        for _ in 0..arity {
            let fq_argument = self.try_parse('[');
            args.push(self.parse_type(fq_argument)?);
            if fq_argument {
                self.try_parse(']');
            }
            self.try_parse(',');
            self.skip_whitespace();
        }
        self.try_parse(']');
        Some(args)
    }

    fn parse_assembly_name(&mut self) -> String {
        if !self.try_parse(',') {
            return String::new();
        }
        self.skip_whitespace();
        let start = self.position;
        while let Some(c) = self.peek() {
            if c == '[' || c == ']' {
                break;
            }
            self.position += 1;
        }
        self.chars[start..self.position].iter().collect()
    }
}

fn is_delimiter(chr: char) -> bool {
    "+,[]*&".contains(chr)
}

/// `TypeParser.SplitFullName`.
pub fn split_full_name(fullname: &str) -> (&str, &str) {
    match fullname.rsplit_once('.') {
        Some((ns, name)) => (ns, name),
        None => ("", fullname),
    }
}

/// C#'s `int.TryParse(s)` under the invariant culture (`NumberStyles.Integer`):
/// optional white space around an optional sign and decimal digits, failing
/// on overflow. Like .NET, trailing NULs are ignored.
pub(crate) fn parse_int32(s: &str) -> Option<i32> {
    let is_white = |c: char| matches!(c, '\u{09}'..='\u{0d}' | ' ');
    let s = s.trim_end_matches('\0').trim_matches(is_white);
    let (negative, digits) = match s.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, s.strip_prefix('+').unwrap_or(s)),
    };
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let mut value: i64 = 0;
    for b in digits.bytes() {
        value = value * 10 + i64::from(b - b'0');
        if value > i64::from(i32::MAX) + 1 {
            return None;
        }
    }
    i32::try_from(if negative { -value } else { value }).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple_name() {
        let t = ParsedType::parse("Foo.Bar").unwrap();
        assert_eq!(t.fullname, "Foo.Bar");
        assert_eq!(t.cecil_full_name(), "Foo.Bar");
    }

    #[test]
    fn nested_and_specs() {
        let t = ParsedType::parse("Foo.Bar+Baz[]").unwrap();
        assert_eq!(t.fullname, "Foo.Bar");
        assert_eq!(t.nested_names, ["Baz"]);
        assert_eq!(t.specs, [Spec::SzArray]);
        assert_eq!(t.cecil_full_name(), "Foo.Bar/Baz[]");
    }

    #[test]
    fn generic_instance() {
        let t = ParsedType::parse(
            "System.Collections.Generic.Dictionary`2[[System.String, mscorlib],[System.Int32, mscorlib]]",
        )
        .unwrap();
        assert_eq!(t.arity, 2);
        assert_eq!(
            t.cecil_full_name(),
            "System.Collections.Generic.Dictionary`2<System.String,System.Int32>"
        );
    }

    #[test]
    fn assembly_qualified() {
        let t = ParsedType::parse("Foo.Bar, MyAssembly, Version=1.0.0.0").unwrap();
        assert_eq!(t.fullname, "Foo.Bar");
        assert_eq!(t.assembly.as_deref(), Some("MyAssembly, Version=1.0.0.0"));
    }

    #[test]
    fn escaped_name() {
        let t = ParsedType::parse("Foo\\+Bar").unwrap();
        assert_eq!(t.fullname, "Foo+Bar");
    }

    // parse_int32 cases checked against .NET 8's int.TryParse; the type
    // names against Cecil on gap-attributes.il.dll.
    #[test]
    fn arity_parses_as_int32() {
        assert_eq!(parse_int32(" 1"), Some(1));
        assert_eq!(parse_int32("1 "), Some(1));
        assert_eq!(parse_int32("\t+1\n"), Some(1));
        assert_eq!(parse_int32("-1"), Some(-1));
        assert_eq!(parse_int32("01"), Some(1));
        assert_eq!(parse_int32("1\0\0"), Some(1));
        assert_eq!(parse_int32("1 \0"), Some(1));
        assert_eq!(parse_int32("1\0 "), None);
        assert_eq!(parse_int32("\u{a0}1"), None);
        assert_eq!(parse_int32("2147483647"), Some(i32::MAX));
        assert_eq!(parse_int32("-2147483648"), Some(i32::MIN));
        assert_eq!(parse_int32("2147483648"), None);
        assert_eq!(parse_int32("4294967297"), None);
        assert_eq!(parse_int32(""), None);
        assert_eq!(parse_int32("-"), None);
        assert_eq!(parse_int32("1 1"), None);
        assert_eq!(parse_int32("+-1"), None);

        let t = ParsedType::parse("A` 1[[System.Int32]]").unwrap();
        assert_eq!(t.cecil_full_name(), "A` 1<System.Int32>");
        let t = ParsedType::parse("A`-1+B`2[[System.Int32]]").unwrap();
        assert_eq!(t.arity, 1);
        assert_eq!(t.cecil_full_name(), "A`-1/B`2<System.Int32>");
        let t = ParsedType::parse("A`4294967297+B`1[[System.Int32]]").unwrap();
        assert_eq!(t.arity, 1);
    }

    // A truncated name keeps an empty argument per arity, as Cecil's does,
    // up to the most a type can have.
    #[test]
    fn arity_is_capped() {
        let t = ParsedType::parse("A`3[").unwrap();
        assert_eq!(t.generic_arguments, vec![ParsedType::default(); 3]);
        let t = ParsedType::parse("A`65535[").unwrap();
        assert_eq!(t.generic_arguments.len(), 65535);
        assert_eq!(ParsedType::parse("A`65536["), None);
        assert_eq!(ParsedType::parse("A`2000000000["), None);
        assert_eq!(ParsedType::parse("A`65535+B`1["), None);
        assert_eq!(ParsedType::parse("A`1[[B`2000000000[]]]"), None);
    }

    #[test]
    fn multi_dim_array() {
        let t = ParsedType::parse("Foo[,]").unwrap();
        assert_eq!(t.specs, [Spec::Array(2)]);
        assert_eq!(t.cecil_full_name(), "Foo[,]");
    }
}
