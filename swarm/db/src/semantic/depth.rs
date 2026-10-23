//! Lexical bound on how deep a SPARQL text can drive recursion.
//!
//! spargebra's parser, sparopt and spareval recurse once per nesting level and
//! once per link of a chain (`a + b + c`, `||`, `IN` lists, `/` paths, triple
//! patterns that fold into joins, collection items, sibling groups, `UNION`s,
//! repeated `FILTER`s). A stack overflow aborts the process rather than
//! panicking, so the text is bounded before it is parsed.
//!
//! Every bracket and operator character adds one to the bracket level it
//! appears in, as does every triple separator in a pattern, every `,` in
//! parentheses, and every term that directly follows another one in
//! parentheses. A level's count never drops while it is open, and the depth is
//! the sum over the open levels. Data blocks and the top level are flat lists,
//! so only their nesting counts.
//!
//! Strings, comments, IRIs, names and numbers are opaque, but only where
//! spargebra cannot read their characters as anything else. A `<` after an
//! operand in parentheses may be an IRI or less-than, so it is counted both
//! ways: any level it could open stays open, which only ever makes later text
//! count more.

/// Deepest structure [`check`] accepts.
pub(crate) const MAX_DEPTH: usize = 128;

pub(crate) fn check(text: &str) -> anyhow::Result<()> {
    let mut scan = Scan {
        bytes: text.as_bytes(),
        pos: 0,
        levels: vec![Level {
            count: 0,
            kind: Kind::Top,
        }],
        depth: 0,
        operand: false,
        after_string: false,
        data_next: false,
    };

    if scan.run() {
        Ok(())
    } else {
        anyhow::bail!("query nests deeper than the limit of {MAX_DEPTH}")
    }
}

struct Level {
    count: usize,
    kind: Kind,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// Update operations, or a query's clauses.
    Top,
    /// A group graph pattern, a blank node property list or a quoted triple.
    Pattern,
    /// An expression, argument list, path or collection.
    Paren,
    /// `INSERT DATA`, `DELETE DATA`, `VALUES` or a template: ground terms
    /// spargebra collects without evaluating.
    Data,
}

struct Scan<'a> {
    bytes: &'a [u8],
    pos: usize,
    levels: Vec<Level>,
    depth: usize,
    /// The previous token ends an operand, so a `<` may be less-than and a
    /// term in parentheses is a collection item.
    operand: bool,
    /// The previous token is a string, so `^^` and `@` start its datatype or
    /// language tag.
    after_string: bool,
    /// The next bracket opens a [`Kind::Data`] block.
    data_next: bool,
}

impl Scan<'_> {
    /// Returns `false` as soon as the depth exceeds [`MAX_DEPTH`].
    fn run(&mut self) -> bool {
        while let Some(&b) = self.bytes.get(self.pos) {
            let after_string = std::mem::take(&mut self.after_string);
            let data_next = std::mem::take(&mut self.data_next);

            let fits = match b {
                b' ' | b'\t' | b'\r' | b'\n' | b'#' => {
                    self.after_string = after_string;
                    self.data_next = data_next;
                    self.skip_blank(b);
                    true
                }
                b'"' | b'\'' => {
                    self.string(b);
                    let fits = self.term();
                    self.after_string = true;
                    fits
                }
                b'^' if after_string && self.peek(1) == Some(b'^') => {
                    self.pos += 2;
                    self.operand = false;
                    true
                }
                b'@' if after_string => {
                    self.lang_tag();
                    self.operand = true;
                    true
                }
                b'<' => self.angle(),
                b'>' if self.peek(1) == Some(b'>') => {
                    self.pos += 2;
                    self.close();
                    self.operand = true;
                    true
                }
                b'(' | b'[' | b'{' => {
                    self.pos += 1;
                    self.open_bracket(b, data_next)
                }
                b')' | b']' | b'}' => {
                    self.pos += 1;
                    self.close();
                    self.operand = true;
                    self.data_next = data_next && b == b')';
                    true
                }
                b'!' | b'&' | b'|' | b'=' | b'+' | b'-' | b'*' | b'/' | b'^' | b'>' => {
                    self.pos += 1;
                    self.operator()
                }
                b'?' | b'$' => {
                    self.pos += 1;
                    while self.peek(0).is_some_and(is_name_char) {
                        self.pos += 1;
                    }
                    self.data_next = data_next;
                    self.term()
                }
                b'0'..=b'9' => {
                    self.number();
                    self.term()
                }
                b'.' if self.peek(1).is_some_and(|b| b.is_ascii_digit()) => {
                    self.number();
                    self.term()
                }
                b'.' | b';' | b',' => {
                    self.pos += 1;
                    let fits = self.separator(b);
                    // Calling `.` and `;` operands only makes a `<` count more.
                    self.operand = b != b',';
                    fits
                }
                b':' => {
                    self.pos += 1;
                    self.local_name();
                    self.term()
                }
                _ if is_name_char(b) => {
                    let start = self.pos;
                    self.name();
                    let fits = self.term();
                    self.data_next = opens_data(&self.bytes[start..self.pos]);
                    fits
                }
                // Anything spargebra rejects.
                _ => {
                    self.pos += 1;
                    self.operand = true;
                    true
                }
            };

            if !fits {
                return false;
            }
        }

        true
    }

    fn peek(&self, offset: usize) -> Option<u8> {
        self.bytes.get(self.pos + offset).copied()
    }

    /// Whitespace, or a `#` comment up to its line end.
    fn skip_blank(&mut self, b: u8) {
        self.pos += 1;
        if b == b'#' {
            self.pos += self.run_of(0, |b| b != b'\n' && b != b'\r');
        }
    }

    fn kind(&self) -> Kind {
        self.levels.last().map_or(Kind::Top, |level| level.kind)
    }

    fn bump(&mut self, n: usize) -> bool {
        if let Some(level) = self.levels.last_mut() {
            level.count += n;
        }
        self.depth += n;
        self.depth <= MAX_DEPTH
    }

    /// Ends a term. In parentheses one term directly after another is a
    /// collection item, which spargebra expands into two more triple patterns.
    fn term(&mut self) -> bool {
        let item = self.kind() == Kind::Paren && self.operand;
        self.operand = true;
        !item || self.bump(1)
    }

    /// An operator, or the sign of a number in a data block.
    fn operator(&mut self) -> bool {
        self.operand = false;
        self.kind() == Kind::Data || self.bump(1)
    }

    fn open_bracket(&mut self, b: u8, data_next: bool) -> bool {
        let kind = match b {
            _ if self.kind() == Kind::Data || data_next => Kind::Data,
            b'(' => Kind::Paren,
            _ => Kind::Pattern,
        };
        let fits = self.open(kind);
        // `VALUES (?a ?b) {`
        self.data_next = data_next && b == b'(';
        fits
    }

    fn open(&mut self, kind: Kind) -> bool {
        let fits = self.bump(1);
        self.levels.push(Level { count: 0, kind });
        self.operand = false;
        fits
    }

    fn close(&mut self) {
        if self.levels.len() <= 1 {
            return;
        }
        let Some(level) = self.levels.pop() else {
            return;
        };
        self.depth -= level.count;

        // Siblings in a flat list do not chain, so only nesting counts there.
        let flat = match self.kind() {
            Kind::Data => true,
            Kind::Top => level.kind != Kind::Paren,
            Kind::Pattern | Kind::Paren => false,
        };
        if flat && let Some(parent) = self.levels.last_mut() {
            parent.count -= 1;
            self.depth -= 1;
        }
    }

    fn separator(&mut self, b: u8) -> bool {
        match self.kind() {
            Kind::Pattern => self.bump(1),
            Kind::Paren if b == b',' => self.bump(1),
            Kind::Top | Kind::Paren | Kind::Data => true,
        }
    }

    /// A `<` opens a quoted triple, or is an IRI, or is less-than.
    fn angle(&mut self) -> bool {
        if self.peek(1) == Some(b'<') {
            self.pos += 2;
            let kind = match self.kind() {
                Kind::Data => Kind::Data,
                Kind::Top | Kind::Pattern | Kind::Paren => Kind::Pattern,
            };
            return self.open(kind);
        }

        let Some(end) = self.iri_end() else {
            self.pos += 1;
            return self.operator();
        };

        // Outside parentheses spargebra reads an IRI or fails; inside them it
        // tries less-than first, but only after an operand.
        if self.kind() != Kind::Paren || !self.operand {
            self.pos = end + 1;
            return self.term();
        }

        // Count the less-than reading, in which the "IRI" is an expression
        // whose parentheses close after the `>`, on top of the IRI reading.
        // Closers inside it are ignored: under neither reading can they close
        // anything opened before it in a text spargebra accepts.
        let inner = self.pos + 1..end;
        self.pos = end + 1;

        if !self.bump(2) {
            return false;
        }
        for &b in &self.bytes[inner] {
            let fits = match b {
                b'(' => self.open(Kind::Paren),
                b'!' | b'&' | b'=' | b'+' | b'-' | b'*' | b'/' => self.bump(1),
                _ => true,
            };
            if !fits {
                return false;
            }
        }
        self.operand = true;

        true
    }

    /// Index of the `>` closing an IRI that starts at `pos`, if one can.
    fn iri_end(&self) -> Option<usize> {
        let rest = self.bytes.get(self.pos + 1..)?;
        let len = rest.iter().position(|&b| {
            b <= b' '
                || matches!(
                    b,
                    b'<' | b'>' | b'"' | b'{' | b'}' | b'|' | b'^' | b'`' | b'\\'
                )
        })?;

        (rest[len] == b'>').then_some(self.pos + 1 + len)
    }

    /// Skips a string literal the way spargebra delimits it. One spargebra
    /// fails on is skipped up to its line end, and scanning resumes there.
    fn string(&mut self, quote: u8) {
        if self.peek(1) == Some(quote) && self.peek(2) == Some(quote) {
            let mut i = self.pos + 3;
            while let Some(&b) = self.bytes.get(i) {
                if b == b'\\' {
                    i += 2;
                } else if b == quote
                    && self.bytes.get(i + 1) == Some(&quote)
                    && self.bytes.get(i + 2) == Some(&quote)
                {
                    self.pos = i + 3;
                    return;
                } else {
                    i += 1;
                }
            }
            // Unterminated, so spargebra falls back to a short string.
        }

        self.pos += 1;
        while let Some(b) = self.peek(0) {
            self.pos += 1;
            match b {
                b'\\' if self.peek(0).is_some_and(|b| b != b'\n' && b != b'\r') => self.pos += 1,
                b'\n' | b'\r' => return,
                _ if b == quote => return,
                _ => {}
            }
        }
    }

    /// `@` [a-zA-Z]+ (`-` [a-zA-Z0-9]+)* (`--` [a-zA-Z]+)?, as spargebra's
    /// `LANGDIR` matches it; whatever it leaves is scanned normally.
    fn lang_tag(&mut self) {
        self.pos += 1;
        self.pos += self.run_of(0, |b| b.is_ascii_alphabetic());
        loop {
            let len = self.run_of(1, |b| b.is_ascii_alphanumeric());
            if self.peek(0) != Some(b'-') || len == 0 {
                break;
            }
            self.pos += 1 + len;
        }
        if self.peek(0) == Some(b'-') && self.peek(1) == Some(b'-') {
            let len = self.run_of(2, |b| b.is_ascii_alphabetic());
            if len > 0 {
                self.pos += 2 + len;
            }
        }
    }

    fn run_of(&self, offset: usize, f: impl Fn(u8) -> bool) -> usize {
        self.bytes
            .get(self.pos + offset..)
            .map_or(0, |rest| rest.iter().take_while(|&&b| f(b)).count())
    }

    /// `INTEGER`, `DECIMAL` or `DOUBLE`, without a sign: a leading `+`/`-`
    /// counts as an operator.
    fn number(&mut self) {
        self.pos += self.run_of(0, |b| b.is_ascii_digit());
        if self.peek(0) == Some(b'.') && self.peek(1).is_some_and(|b| b.is_ascii_digit()) {
            self.pos += 1 + self.run_of(1, |b| b.is_ascii_digit());
        }
        if matches!(self.peek(0), Some(b'e' | b'E')) {
            let sign = usize::from(matches!(self.peek(1), Some(b'+' | b'-')));
            let digits = self.run_of(1 + sign, |b| b.is_ascii_digit());
            if digits > 0 {
                self.pos += 1 + sign + digits;
            }
        }
    }

    /// A keyword, or the prefix of a prefixed name or blank node. A `-` ends
    /// it: spargebra may stop a keyword there and read the rest as a chain.
    fn name(&mut self) {
        self.pos += self.run_of(0, is_name_char);
        if self.peek(0) == Some(b':') {
            self.pos += 1;
            self.local_name();
        }
    }

    /// The part after the `:`, matched as spargebra's `PN_LOCAL` does: it never
    /// starts with `-` or `.`, and only one run of dots may follow its start.
    fn local_name(&mut self) {
        let mut first = true;
        let mut dotted = false;
        loop {
            let len = match self.peek(0) {
                Some(b'-') if !first => 1,
                Some(b'.') if !first && !dotted => {
                    let dots = self.run_of(0, |b| b == b'.');
                    dotted = true;
                    match self.local_char(dots) {
                        Some(len) => dots + len,
                        None => return,
                    }
                }
                _ => match self.local_char(0) {
                    Some(len) => len,
                    None => return,
                },
            };
            self.pos += len;
            first = false;
        }
    }

    /// Length of the `PN_LOCAL` character at `pos + offset` other than `-`/`.`.
    fn local_char(&self, offset: usize) -> Option<usize> {
        match self.bytes.get(self.pos + offset)? {
            b':' => Some(1),
            b'%' => {
                let hex = self
                    .bytes
                    .get(self.pos + offset + 1..self.pos + offset + 3)?;
                hex.iter().all(u8::is_ascii_hexdigit).then_some(3)
            }
            b'\\' => {
                let escaped = self.bytes.get(self.pos + offset + 1)?;
                b"_~.-!$&'()*+,;=/?#@%".contains(escaped).then_some(2)
            }
            &b if is_name_char(b) => Some(1),
            _ => None,
        }
    }
}

/// Keywords whose next bracket holds data or a template rather than a pattern.
fn opens_data(word: &[u8]) -> bool {
    ["DATA", "VALUES", "INSERT", "DELETE", "CONSTRUCT"]
        .iter()
        .any(|keyword| keyword.as_bytes().eq_ignore_ascii_case(word))
}

/// ASCII letters, digits, `_`, and any byte of a non-ASCII character.
fn is_name_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b >= 0x80
}

#[cfg(test)]
mod tests {
    use super::{MAX_DEPTH, check};

    fn passes(text: &str) -> bool {
        check(text).is_ok()
    }

    #[test]
    fn ordinary_queries_pass() {
        assert!(passes(
            r#"
            PREFIX ex: <http://example.org/ns#>
            # a comment with ((((((( and !!!!!!!
            SELECT ?s (COUNT(?o) AS ?n) WHERE {
                ?s ex:has-part/ex:name ?o ;
                   ex:label "a (((( string"@en-GB , """long ((((
                   string"""^^<http://www.w3.org/2001/XMLSchema#string> .
                OPTIONAL { ?s ex:age ?age }
                FILTER(?age >= 18 && ?o != <http://example.org/a/b-c/d?e=f&g>)
                FILTER(?s IN (<http://example.org/a/b>, <http://example.org/c/d>))
                FILTER NOT EXISTS { ?s <http://example.org/p> <http://example.org/o> }
                ?s ex:list ( 1 "two" ex:three <http://example.org/four> ?five [ ] ) .
                << ?s ex:p ?o >> ex:certainty 0.9 .
                BIND(<< ?s ex:p ?o >> AS ?t)
            }
            GROUP BY ?s
            ORDER BY DESC(?n)
            "#
        ));
    }

    #[test]
    fn bulk_data_passes() {
        let triples = "<http://example.org/a/b> <http://example.org/p-q> \
                       \"2020-01-01\"^^<http://www.w3.org/2001/XMLSchema#date> .\n"
            .repeat(2_000);

        assert!(passes(&format!("INSERT DATA {{ {triples} }}")));
        assert!(passes(&format!(
            "DELETE DATA {{ GRAPH <http://x/g> {{ {triples} }} }}"
        )));
        assert!(passes(&format!("INSERT {{ {triples} }} WHERE {{}}")));

        let readings = "[ <http://x/at> -1.5e3 ; <http://x/v> (-1 -2 -3) ] <http://x/p> -1 .\n";
        assert!(passes(&format!(
            "INSERT DATA {{ {} }}",
            readings.repeat(2_000)
        )));

        let quoted = "<< <http://x/a> <http://x/b> -1 >> <http://x/c> +2 .\n";
        assert!(passes(&format!(
            "INSERT DATA {{ {} }}",
            quoted.repeat(2_000)
        )));

        let rows = "(-1 \"a\"@en <http://x/a>) ".repeat(2_000);
        assert!(passes(&format!(
            "SELECT * WHERE {{ VALUES (?a ?b ?c) {{ {rows} }} }}"
        )));
        assert!(passes(&format!(
            "SELECT * WHERE {{ VALUES ?n {{ {} }} }}",
            "-1 ".repeat(2_000)
        )));

        let operations = "INSERT DATA { <http://x/a> <http://x/b> -1 } ;\n".repeat(2_000);
        assert!(passes(&format!("{operations} CLEAR DEFAULT")));
    }

    #[test]
    fn iris_in_nested_patterns_are_opaque() {
        let triples = "?s <http://x/p> <http://x/o> . ".repeat(100);

        assert!(passes(&format!(
            "SELECT * WHERE {{ FILTER(NOT EXISTS {{ {triples} }}) }}"
        )));
        assert!(passes(&format!(
            "SELECT * WHERE {{ BIND(EXISTS {{ {triples} }} AS ?x) }}"
        )));
    }

    #[test]
    fn data_blocks_bound_their_nesting() {
        let nested = format!("{}1{}", "(".repeat(MAX_DEPTH), ")".repeat(MAX_DEPTH));
        let quoted = format!(
            "{}<http://x/s> <http://x/p> <http://x/o>{}",
            "<< ".repeat(MAX_DEPTH),
            " >>".repeat(MAX_DEPTH)
        );

        assert!(!passes(&format!(
            "INSERT DATA {{ <http://x/a> <http://x/b> {nested} }}"
        )));
        assert!(!passes(&format!(
            "INSERT DATA {{ {quoted} <http://x/b> <http://x/c> }}"
        )));
        assert!(!passes(&format!("SELECT * {{ VALUES ?a {{ {nested} }} }}")));
    }

    #[test]
    fn nesting_is_bounded() {
        let at = |n: usize| format!("{}{}", "(".repeat(n), ")".repeat(n));

        assert!(passes(&at(MAX_DEPTH)));
        assert!(!passes(&at(MAX_DEPTH + 1)));
    }

    #[test]
    fn chains_are_bounded() {
        for link in ["+1", "||1", "{} ", "/<http://x/p>", "!", "FILTER(1) "] {
            let text = format!("SELECT * WHERE {{ {} }}", link.repeat(MAX_DEPTH));

            assert!(!passes(&text), "{link}");
        }

        for pattern in ["?s ?p ?o . ", "?s ?p ?o ; ", "?s ?p ?o , ", "[ ?p ?o ; "] {
            let text = format!("SELECT * WHERE {{ {} }}", pattern.repeat(MAX_DEPTH));

            assert!(!passes(&text), "{pattern}");
        }

        let list = format!(
            "SELECT * WHERE {{ FILTER(1 IN (1{})) }}",
            ",1".repeat(MAX_DEPTH)
        );
        assert!(!passes(&list));

        let projection = format!("SELECT {} {{}}", "(1 AS ?x) ".repeat(MAX_DEPTH));
        assert!(!passes(&projection));
    }

    /// Every item of a collection becomes two triple patterns.
    #[test]
    fn collection_items_are_bounded() {
        for item in [
            "1 ",
            "?x ",
            "ex:a ",
            "<http://x/a> ",
            "\"s\" ",
            "[] ",
            "(1) ",
        ] {
            let text = format!(
                "SELECT * WHERE {{ ?s <http://x/p> ( {}) }}",
                item.repeat(MAX_DEPTH)
            );

            assert!(!passes(&text), "{item}");
        }

        let nested = format!(
            "SELECT * WHERE {{ ?s <http://x/p> ( ( {}) ) }}",
            "1 ".repeat(MAX_DEPTH)
        );
        assert!(!passes(&nested));
    }

    #[test]
    fn opaque_tokens_hide_nothing_spargebra_parses() {
        let n = MAX_DEPTH;
        let triples = "?s ?p ?o . ".repeat(n);
        let groups = "{ ?s ?p ?o } ".repeat(n);
        let cases = [
            // Keywords stop before `-`, and a local name never starts with one.
            format!("FILTER(true{})", "-1".repeat(n)),
            format!("FILTER(ex:{})", "-1".repeat(n)),
            // `<` after an operand in parentheses can be less-than.
            format!("FILTER(?a <ex:f({}1)> ?b)", "ex:f(".repeat(n)),
            format!("FILTER(?a <ex:a{}> ?b)", "/1".repeat(n)),
            // Closers inside an ambiguous `<…>` do not hide later nesting.
            format!("FILTER(?a <ex:b)> {}1{})", "(".repeat(n), ")".repeat(n)),
            // A paren opened inside an ambiguous `<…>` does not swallow the
            // closer of the enclosing expression.
            format!("SELECT * WHERE {{ FILTER(?o > <http://x/p(>) {triples} }}"),
            format!("SELECT * WHERE {{ FILTER(?a <(1> 2)) {groups} }}"),
            // A closer inside an IRI in a collection does not close anything.
            format!("SELECT * WHERE {{ ?s <http://x/p> ( <http://x/a)> ) {groups} }}"),
            // An unterminated long string is a short one.
            format!("FILTER(\"\"\"x\" = {}1{})", "(".repeat(n), ")".repeat(n)),
            // An unterminated short string ends at its line.
            format!("FILTER(\"x\n{}1{})", "(".repeat(n), ")".repeat(n)),
            // An invalid escape does not swallow what follows.
            format!("FILTER(ex:a\\x{})", "(".repeat(n)),
            // `^^` only follows a string.
            format!("FILTER(?a{}1)", "^^".repeat(n)),
            // A language tag stops where `LANGDIR` does.
            format!("FILTER(\"x\"@en--{}1)", "-1".repeat(n)),
        ];

        for text in &cases {
            assert!(!passes(text), "{text}");
        }
    }
}
