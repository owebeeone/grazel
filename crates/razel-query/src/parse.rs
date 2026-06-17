//! P1.1: the `razel query` expression parser — a tokenizer + recursive-descent parser over the
//! §12 grammar producing an [`Expr`] AST. Pure (no graph); evaluated later (P1.3+). The binary set
//! operators are left-associative at one precedence level (Bazel's behavior).

/// A parsed query expression (RazelCrateUniverseDesign §12).
#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    /// A target pattern / label literal (`//pkg:t`, `//...`, `@crates//:x`, `:name`).
    Pattern(String),
    /// A `$`-prefixed `let` variable reference.
    Var(String),
    /// `deps(x[, depth])` — forward transitive closure.
    Deps(Box<Expr>, Option<u32>),
    /// `rdeps(universe, x[, depth])` — reverse closure within `universe`.
    Rdeps(Box<Expr>, Box<Expr>, Option<u32>),
    /// `kind(regex, x)` — filter by the kind string.
    Kind(String, Box<Expr>),
    /// `filter(regex, x)` — filter by a regex over labels.
    Filter(String, Box<Expr>),
    /// `attr(name, regex, x)` — filter by the canonical stringification of one attr.
    Attr(String, String, Box<Expr>),
    /// `labels(attr, x)` — the raw label literals of `attr`.
    Labels(String, Box<Expr>),
    /// `somepath(x, y)` — one shortest path x→y.
    SomePath(Box<Expr>, Box<Expr>),
    /// `allpaths(x, y)` — the x→y subgraph node set.
    AllPaths(Box<Expr>, Box<Expr>),
    /// `siblings(x)` — every target in the same package(s) as `x` (Bazel's `:*` per package).
    Siblings(Box<Expr>),
    /// `same_pkg_direct_rdeps(x)` — same-package targets that directly depend on a target in `x`.
    SamePkgDirectRdeps(Box<Expr>),
    /// `x + y` / `x union y`.
    Union(Box<Expr>, Box<Expr>),
    /// `x - y` / `x except y`.
    Except(Box<Expr>, Box<Expr>),
    /// `x ^ y` / `x intersect y`.
    Intersect(Box<Expr>, Box<Expr>),
    /// `let v = e1 in e2`.
    Let(String, Box<Expr>, Box<Expr>),
}

/// Parse a query expression string, or a human-readable error.
pub fn parse(input: &str) -> Result<Expr, String> {
    let tokens = tokenize(input)?;
    let mut p = Parser { tokens, pos: 0 };
    let e = p.expr()?;
    if let Some(t) = p.peek() {
        return Err(format!("unexpected trailing token `{}`", t.text()));
    }
    Ok(e)
}

// ── tokenizer ────────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    /// A bare word — may be a keyword, an operator (`+`/`-`/`^`/`union`/…), a `$var`, or a pattern.
    Word(String),
    /// A `"..."` literal — always a string/pattern, never an operator (so `"+"` isn't a union).
    Str(String),
    LParen,
    RParen,
    Comma,
}

impl Tok {
    fn text(&self) -> String {
        match self {
            Tok::Word(s) | Tok::Str(s) => s.clone(),
            Tok::LParen => "(".into(),
            Tok::RParen => ")".into(),
            Tok::Comma => ",".into(),
        }
    }
}

/// Split into tokens: whitespace separates; `(` `)` `,` are always their own tokens (even
/// mid-word, so `deps(//x,2)` lexes); `"..."` is a `Str`; everything else is a `Word` — so `-`
/// inside a label (`//a:b-c`) stays in the word, while a spaced `-` becomes its own `Word("-")`
/// the parser reads as `except`.
fn tokenize(input: &str) -> Result<Vec<Tok>, String> {
    let mut out = Vec::new();
    let mut word = String::new();
    let mut chars = input.chars().peekable();
    fn flush(word: &mut String, out: &mut Vec<Tok>) {
        if !word.is_empty() {
            out.push(Tok::Word(std::mem::take(word)));
        }
    }
    while let Some(c) = chars.next() {
        match c {
            c if c.is_whitespace() => flush(&mut word, &mut out),
            '(' => {
                flush(&mut word, &mut out);
                out.push(Tok::LParen);
            }
            ')' => {
                flush(&mut word, &mut out);
                out.push(Tok::RParen);
            }
            ',' => {
                flush(&mut word, &mut out);
                out.push(Tok::Comma);
            }
            '"' => {
                flush(&mut word, &mut out);
                let mut s = String::new();
                let mut closed = false;
                for q in chars.by_ref() {
                    if q == '"' {
                        closed = true;
                        break;
                    }
                    s.push(q);
                }
                if !closed {
                    return Err("unterminated quoted string".into());
                }
                out.push(Tok::Str(s));
            }
            _ => word.push(c),
        }
    }
    flush(&mut word, &mut out);
    Ok(out)
}

// ── parser ───────────────────────────────────────────────────────────────────────────────────

struct Parser {
    tokens: Vec<Tok>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.tokens.get(self.pos)
    }
    fn next(&mut self) -> Option<Tok> {
        let t = self.tokens.get(self.pos).cloned();
        if t.is_some() {
            self.pos += 1;
        }
        t
    }
    /// The next token's word text, if it is a bare `Word` equal to `kw` (operators/keywords).
    fn peek_word(&self) -> Option<&str> {
        match self.peek() {
            Some(Tok::Word(s)) => Some(s.as_str()),
            _ => None,
        }
    }
    fn expect(&mut self, want: &Tok, ctx: &str) -> Result<(), String> {
        match self.next() {
            Some(ref t) if t == want => Ok(()),
            other => Err(format!(
                "expected `{}` {ctx}, found `{}`",
                want.text(),
                other.map(|t| t.text()).unwrap_or_else(|| "<end>".into())
            )),
        }
    }
    /// A literal string argument (a `Word` or `Str`) — for regex / attr-name args.
    fn string_arg(&mut self, ctx: &str) -> Result<String, String> {
        match self.next() {
            Some(Tok::Word(s)) | Some(Tok::Str(s)) => Ok(s),
            other => Err(format!(
                "expected a string {ctx}, found `{}`",
                other.map(|t| t.text()).unwrap_or_else(|| "<end>".into())
            )),
        }
    }

    /// Binary set operators, left-associative at one precedence level (Bazel's behavior).
    fn expr(&mut self) -> Result<Expr, String> {
        let mut left = self.primary()?;
        while let Some(w) = self.peek_word() {
            let make: fn(Box<Expr>, Box<Expr>) -> Expr = match w {
                "+" | "union" => |a, b| Expr::Union(a, b),
                "-" | "except" => |a, b| Expr::Except(a, b),
                "^" | "intersect" => |a, b| Expr::Intersect(a, b),
                _ => break,
            };
            self.next(); // the operator
            let right = self.primary()?;
            left = make(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn primary(&mut self) -> Result<Expr, String> {
        match self.peek() {
            Some(Tok::LParen) => {
                self.next();
                let e = self.expr()?;
                self.expect(&Tok::RParen, "to close `(`")?;
                Ok(e)
            }
            Some(Tok::Str(_)) => {
                let s = self.string_arg("")?;
                Ok(Expr::Pattern(s))
            }
            Some(Tok::Word(w)) => {
                let w = w.clone();
                match w.as_str() {
                    "let" => self.parse_let(),
                    "deps" => self.parse_deps(),
                    "rdeps" => self.parse_rdeps(),
                    "kind" => self.parse_filter2(|r, x| Expr::Kind(r, x)),
                    "filter" => self.parse_filter2(|r, x| Expr::Filter(r, x)),
                    "labels" => self.parse_filter2(|a, x| Expr::Labels(a, x)),
                    "attr" => self.parse_attr(),
                    "somepath" => self.parse_path2(|a, b| Expr::SomePath(a, b)),
                    "allpaths" => self.parse_path2(|a, b| Expr::AllPaths(a, b)),
                    "siblings" => self.parse_unary(|x| Expr::Siblings(x)),
                    "same_pkg_direct_rdeps" => {
                        self.parse_unary(|x| Expr::SamePkgDirectRdeps(x))
                    }
                    "tests" | "set" | "buildfiles" | "loadfiles" | "rbuildfiles" | "visible" => {
                        Err(format!("`{w}` is not supported in razel query v1 (deferred — §12)"))
                    }
                    _ if w.starts_with('$') => {
                        self.next();
                        Ok(Expr::Var(w[1..].to_string()))
                    }
                    "+" | "-" | "^" | "union" | "except" | "intersect" | "in" => {
                        Err(format!("unexpected operator `{w}`"))
                    }
                    _ => {
                        self.next();
                        Ok(Expr::Pattern(w))
                    }
                }
            }
            None => Err("unexpected end of expression".into()),
            Some(t) => Err(format!("unexpected token `{}`", t.text())),
        }
    }

    fn parse_let(&mut self) -> Result<Expr, String> {
        self.next(); // let
        let name = match self.next() {
            Some(Tok::Word(s)) => s,
            other => {
                return Err(format!(
                    "expected a variable name after `let`, found `{}`",
                    other.map(|t| t.text()).unwrap_or_else(|| "<end>".into())
                ));
            }
        };
        if self.peek_word() != Some("=") {
            return Err("expected `=` in `let`".into());
        }
        self.next(); // =
        let value = self.expr()?;
        if self.peek_word() != Some("in") {
            return Err("expected `in` in `let`".into());
        }
        self.next(); // in
        let body = self.expr()?;
        Ok(Expr::Let(name, Box::new(value), Box::new(body)))
    }

    /// `f(expr [, depth])` → for `deps`.
    fn parse_deps(&mut self) -> Result<Expr, String> {
        self.next();
        self.expect(&Tok::LParen, "after `deps`")?;
        let x = self.expr()?;
        let depth = self.opt_depth()?;
        self.expect(&Tok::RParen, "to close `deps(`")?;
        Ok(Expr::Deps(Box::new(x), depth))
    }

    /// `rdeps(universe, x [, depth])`.
    fn parse_rdeps(&mut self) -> Result<Expr, String> {
        self.next();
        self.expect(&Tok::LParen, "after `rdeps`")?;
        let universe = self.expr()?;
        self.expect(&Tok::Comma, "after the rdeps universe")?;
        let x = self.expr()?;
        let depth = self.opt_depth()?;
        self.expect(&Tok::RParen, "to close `rdeps(`")?;
        Ok(Expr::Rdeps(Box::new(universe), Box::new(x), depth))
    }

    /// `f(string, expr)` → for `kind`/`filter`/`labels`.
    fn parse_filter2(&mut self, make: fn(String, Box<Expr>) -> Expr) -> Result<Expr, String> {
        self.next();
        self.expect(&Tok::LParen, "after the function name")?;
        let s = self.string_arg("as the first argument")?;
        self.expect(&Tok::Comma, "after the first argument")?;
        let x = self.expr()?;
        self.expect(&Tok::RParen, "to close the function call")?;
        Ok(make(s, Box::new(x)))
    }

    /// `attr(name, regex, expr)`.
    fn parse_attr(&mut self) -> Result<Expr, String> {
        self.next();
        self.expect(&Tok::LParen, "after `attr`")?;
        let name = self.string_arg("as the attr name")?;
        self.expect(&Tok::Comma, "after the attr name")?;
        let regex = self.string_arg("as the attr regex")?;
        self.expect(&Tok::Comma, "after the attr regex")?;
        let x = self.expr()?;
        self.expect(&Tok::RParen, "to close `attr(`")?;
        Ok(Expr::Attr(name, regex, Box::new(x)))
    }

    /// `f(expr, expr)` → for `somepath`/`allpaths`.
    fn parse_path2(&mut self, make: fn(Box<Expr>, Box<Expr>) -> Expr) -> Result<Expr, String> {
        self.next();
        self.expect(&Tok::LParen, "after the function name")?;
        let a = self.expr()?;
        self.expect(&Tok::Comma, "between the two path arguments")?;
        let b = self.expr()?;
        self.expect(&Tok::RParen, "to close the function call")?;
        Ok(make(Box::new(a), Box::new(b)))
    }

    /// `f(expr)` → for single-argument verbs like `siblings`.
    fn parse_unary(&mut self, make: fn(Box<Expr>) -> Expr) -> Result<Expr, String> {
        self.next();
        self.expect(&Tok::LParen, "after the function name")?;
        let x = self.expr()?;
        self.expect(&Tok::RParen, "to close the function call")?;
        Ok(make(Box::new(x)))
    }

    /// An optional `, <number>` depth argument.
    fn opt_depth(&mut self) -> Result<Option<u32>, String> {
        if self.peek() != Some(&Tok::Comma) {
            return Ok(None);
        }
        self.next(); // ,
        let n = self.string_arg("as the depth")?;
        n.parse::<u32>().map(Some).map_err(|_| format!("depth `{n}` is not a non-negative integer"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Expr {
        parse(s).unwrap_or_else(|e| panic!("parse `{s}`: {e}"))
    }

    #[test]
    fn patterns_and_vars() {
        assert_eq!(p("//foo:bar"), Expr::Pattern("//foo:bar".into()));
        assert_eq!(p("@crates//..."), Expr::Pattern("@crates//...".into()));
        assert_eq!(p("//a:b-c"), Expr::Pattern("//a:b-c".into())); // `-` inside a label stays
        assert_eq!(p("$v"), Expr::Var("v".into()));
    }

    #[test]
    fn deps_with_and_without_depth() {
        assert_eq!(p("deps(//x)"), Expr::Deps(Box::new(Expr::Pattern("//x".into())), None));
        assert_eq!(
            p("deps(//x, 2)"),
            Expr::Deps(Box::new(Expr::Pattern("//x".into())), Some(2))
        );
        // no spaces around the comma/parens lexes too.
        assert_eq!(p("deps(//x,2)"), Expr::Deps(Box::new(Expr::Pattern("//x".into())), Some(2)));
    }

    #[test]
    fn rdeps_kind_attr_labels_somepath() {
        assert!(matches!(p("rdeps(//..., //x)"), Expr::Rdeps(..)));
        assert_eq!(
            p("kind(\"rust_library rule\", //x)"),
            Expr::Kind("rust_library rule".into(), Box::new(Expr::Pattern("//x".into())))
        );
        assert!(matches!(p("attr(srcs, foo, //x)"), Expr::Attr(..)));
        assert!(matches!(p("labels(deps, //x)"), Expr::Labels(..)));
        assert!(matches!(p("somepath(//a, //b)"), Expr::SomePath(..)));
        assert!(matches!(p("allpaths(//a, //b)"), Expr::AllPaths(..)));
    }

    #[test]
    fn set_ops_are_left_associative() {
        // a + b - c == (a + b) - c
        assert_eq!(
            p("//a + //b - //c"),
            Expr::Except(
                Box::new(Expr::Union(
                    Box::new(Expr::Pattern("//a".into())),
                    Box::new(Expr::Pattern("//b".into()))
                )),
                Box::new(Expr::Pattern("//c".into()))
            )
        );
        // keyword spellings parse identically to the symbols.
        assert_eq!(p("//a union //b"), p("//a + //b"));
        assert_eq!(p("//a intersect //b"), p("//a ^ //b"));
        assert_eq!(p("//a except //b"), p("//a - //b"));
    }

    #[test]
    fn parens_override_associativity() {
        assert_eq!(
            p("//a + (//b - //c)"),
            Expr::Union(
                Box::new(Expr::Pattern("//a".into())),
                Box::new(Expr::Except(
                    Box::new(Expr::Pattern("//b".into())),
                    Box::new(Expr::Pattern("//c".into()))
                ))
            )
        );
    }

    #[test]
    fn let_binding() {
        assert_eq!(
            p("let v = //x in $v + $v"),
            Expr::Let(
                "v".into(),
                Box::new(Expr::Pattern("//x".into())),
                Box::new(Expr::Union(
                    Box::new(Expr::Var("v".into())),
                    Box::new(Expr::Var("v".into()))
                ))
            )
        );
    }

    #[test]
    fn quoted_strings_are_literal_never_operators() {
        // a quoted "+" is a pattern, not a union operator.
        assert_eq!(p("\"+\""), Expr::Pattern("+".into()));
        assert_eq!(p("filter(\"a b\", //x)"), Expr::Filter("a b".into(), Box::new(Expr::Pattern("//x".into()))));
    }

    #[test]
    fn errors_are_human_readable() {
        assert!(parse("deps(").is_err()); // unexpected end
        assert!(parse("deps(//x").unwrap_err().contains("expected `)`"));
        assert!(parse("//a +").is_err()); // dangling operator
        assert!(parse("\"unterminated").unwrap_err().contains("unterminated"));
        assert!(parse("tests(//x)").unwrap_err().contains("deferred"));
        assert!(parse("//a //b").unwrap_err().contains("trailing")); // two primaries, no operator
        assert!(parse("deps(//x, foo)").unwrap_err().contains("depth")); // non-numeric depth
    }
}
