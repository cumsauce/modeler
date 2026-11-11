//! Parametric L-system pieces: expressions over a module's parameters
//! (`l * 0.9`, `w > 0.01 && l < 2`), modules (`A(1, 0.1)`), words of
//! modules and rule heads (`A(l, w) : l > 0.05`). Compiled once, evaluated
//! per module per iteration.

use std::collections::BTreeMap;

/// Most parameters a module may carry.
pub const MAX_ARGS: usize = 6;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Module {
    pub sym: char,
    pub n: u8,
    pub a: [f32; MAX_ARGS],
}

impl Module {
    pub fn plain(sym: char) -> Self {
        Module {
            sym,
            n: 0,
            a: [0.0; MAX_ARGS],
        }
    }
    /// The first parameter, if any.
    pub fn arg(&self) -> Option<f32> {
        (self.n > 0).then_some(self.a[0])
    }
}

#[derive(Clone, Copy, Debug)]
pub enum Bin {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Pow,
    Lt,
    Le,
    Gt,
    Ge,
    Eq,
    Ne,
    And,
    Or,
}

#[derive(Clone, Debug)]
pub enum Expr {
    Num(f64),
    Var(usize),
    Neg(Box<Expr>),
    Not(Box<Expr>),
    Bin(Bin, Box<Expr>, Box<Expr>),
    Call(&'static str, Vec<Expr>),
}

impl Expr {
    pub fn eval(&self, v: &[f32]) -> f64 {
        match self {
            Expr::Num(x) => *x,
            Expr::Var(i) => v.get(*i).copied().unwrap_or(0.0) as f64,
            Expr::Neg(e) => -e.eval(v),
            Expr::Not(e) => f64::from(e.eval(v) == 0.0),
            Expr::Bin(op, a, b) => {
                let (x, y) = (a.eval(v), b.eval(v));
                match op {
                    Bin::Add => x + y,
                    Bin::Sub => x - y,
                    Bin::Mul => x * y,
                    Bin::Div => x / y,
                    Bin::Rem => x % y,
                    Bin::Pow => x.powf(y),
                    Bin::Lt => f64::from(x < y),
                    Bin::Le => f64::from(x <= y),
                    Bin::Gt => f64::from(x > y),
                    Bin::Ge => f64::from(x >= y),
                    Bin::Eq => f64::from((x - y).abs() < 1e-9),
                    Bin::Ne => f64::from((x - y).abs() >= 1e-9),
                    Bin::And => f64::from(x != 0.0 && y != 0.0),
                    Bin::Or => f64::from(x != 0.0 || y != 0.0),
                }
            }
            Expr::Call(f, args) => {
                let a: Vec<f64> = args.iter().map(|e| e.eval(v)).collect();
                match *f {
                    "min" => a[0].min(a[1]),
                    "max" => a[0].max(a[1]),
                    "abs" => a[0].abs(),
                    "sqrt" => a[0].max(0.0).sqrt(),
                    "sin" => a[0].to_radians().sin(),
                    "cos" => a[0].to_radians().cos(),
                    "floor" => a[0].floor(),
                    _ => 0.0,
                }
            }
        }
    }
}

const FUNCS: &[(&str, usize)] = &[
    ("min", 2),
    ("max", 2),
    ("abs", 1),
    ("sqrt", 1),
    ("sin", 1),
    ("cos", 1),
    ("floor", 1),
];

struct P<'a> {
    s: &'a [u8],
    i: usize,
    src: &'a str,
    vars: &'a [String],
    consts: &'a BTreeMap<String, f64>,
}

impl P<'_> {
    fn ws(&mut self) {
        while self.s.get(self.i).is_some_and(|c| c.is_ascii_whitespace()) {
            self.i += 1;
        }
    }
    fn eat(&mut self, t: &str) -> bool {
        self.ws();
        if self.s[self.i..].starts_with(t.as_bytes()) {
            self.i += t.len();
            true
        } else {
            false
        }
    }
    fn err<T>(&self, what: &str) -> Result<T, String> {
        Err(format!(
            "`{}`: {what} at `{}`",
            self.src,
            &self.src[self.i.min(self.src.len())..]
        ))
    }
    fn or(&mut self) -> Result<Expr, String> {
        let mut a = self.and()?;
        while self.eat("||") {
            a = Expr::Bin(Bin::Or, Box::new(a), Box::new(self.and()?));
        }
        Ok(a)
    }
    fn and(&mut self) -> Result<Expr, String> {
        let mut a = self.cmp()?;
        while self.eat("&&") {
            a = Expr::Bin(Bin::And, Box::new(a), Box::new(self.cmp()?));
        }
        Ok(a)
    }
    fn cmp(&mut self) -> Result<Expr, String> {
        let a = self.sum()?;
        for (t, op) in [
            ("<=", Bin::Le),
            (">=", Bin::Ge),
            ("==", Bin::Eq),
            ("!=", Bin::Ne),
            ("<", Bin::Lt),
            (">", Bin::Gt),
        ] {
            if self.eat(t) {
                return Ok(Expr::Bin(op, Box::new(a), Box::new(self.sum()?)));
            }
        }
        Ok(a)
    }
    fn sum(&mut self) -> Result<Expr, String> {
        let mut a = self.prod()?;
        loop {
            let op = if self.eat("+") {
                Bin::Add
            } else if self.eat("-") {
                Bin::Sub
            } else {
                return Ok(a);
            };
            a = Expr::Bin(op, Box::new(a), Box::new(self.prod()?));
        }
    }
    fn prod(&mut self) -> Result<Expr, String> {
        let mut a = self.unary()?;
        loop {
            let op = if self.eat("*") {
                Bin::Mul
            } else if self.eat("/") {
                Bin::Div
            } else if self.eat("%") {
                Bin::Rem
            } else {
                return Ok(a);
            };
            a = Expr::Bin(op, Box::new(a), Box::new(self.pow()?));
        }
    }
    // `-2^2` is -(2^2) and `2^-1` works: `^` binds tighter than a sign
    // on its left and takes one on its right (right-associative).
    fn unary(&mut self) -> Result<Expr, String> {
        if self.eat("-") {
            return Ok(Expr::Neg(Box::new(self.unary()?)));
        }
        if self.eat("!") {
            return Ok(Expr::Not(Box::new(self.unary()?)));
        }
        self.pow()
    }
    fn pow(&mut self) -> Result<Expr, String> {
        let a = self.atom()?;
        if self.eat("^") {
            return Ok(Expr::Bin(Bin::Pow, Box::new(a), Box::new(self.unary()?)));
        }
        Ok(a)
    }
    fn atom(&mut self) -> Result<Expr, String> {
        self.ws();
        if self.eat("(") {
            let e = self.or()?;
            if !self.eat(")") {
                return self.err("missing `)`");
            }
            return Ok(e);
        }
        let start = self.i;
        let c = self.s.get(self.i).copied().unwrap_or(0);
        if c.is_ascii_digit() || c == b'.' {
            while let Some(&c) = self.s.get(self.i) {
                let sign = (c == b'-' || c == b'+')
                    && self.i > start
                    && matches!(self.s[self.i - 1], b'e' | b'E');
                if !(c.is_ascii_digit() || c == b'.' || c == b'e' || c == b'E' || sign) {
                    break;
                }
                self.i += 1;
            }
            return self.src[start..self.i]
                .parse()
                .map(Expr::Num)
                .or_else(|_| self.err("bad number"));
        }
        if c.is_ascii_alphabetic() || c == b'_' {
            while self
                .s
                .get(self.i)
                .is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'_')
            {
                self.i += 1;
            }
            let id = &self.src[start..self.i];
            if let Some(&(f, n)) = FUNCS.iter().find(|(f, _)| *f == id) {
                if !self.eat("(") {
                    return self.err(&format!("`{f}` needs `(`"));
                }
                let mut args = vec![self.or()?];
                while self.eat(",") {
                    args.push(self.or()?);
                }
                if !self.eat(")") || args.len() != n {
                    return self.err(&format!("`{f}` takes {n} argument(s)"));
                }
                return Ok(Expr::Call(f, args));
            }
            if let Some(i) = self.vars.iter().position(|v| v == id) {
                return Ok(Expr::Var(i));
            }
            if let Some(x) = self.consts.get(id) {
                return Ok(Expr::Num(*x));
            }
            self.i = start;
            return self.err(&format!(
                "unknown name `{id}` (use this rule's parameters, `constants`, or {})",
                FUNCS.iter().map(|f| f.0).collect::<Vec<_>>().join(", ")
            ));
        }
        self.err("expected a number, a name or `(`")
    }
}

pub fn compile(src: &str, vars: &[String], consts: &BTreeMap<String, f64>) -> Result<Expr, String> {
    let mut p = P {
        s: src.as_bytes(),
        i: 0,
        src,
        vars,
        consts,
    };
    let e = p.or()?;
    p.ws();
    if p.i != p.s.len() {
        return p.err("unexpected text");
    }
    Ok(e)
}

/// One module of a successor: a symbol and its argument expressions.
#[derive(Clone, Debug)]
pub struct Template {
    pub sym: char,
    pub args: Vec<Expr>,
}

impl Template {
    pub fn eval(&self, v: &[f32]) -> Module {
        let mut m = Module::plain(self.sym);
        m.n = self.args.len() as u8;
        for (k, e) in self.args.iter().enumerate() {
            m.a[k] = e.eval(v) as f32;
        }
        m
    }
}

/// Splits `src` at top-level commas (not inside parentheses).
fn split_args(src: &str) -> Vec<&str> {
    let (mut out, mut depth, mut from) = (Vec::new(), 0i32, 0);
    for (i, c) in src.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            ',' if depth == 0 => {
                out.push(&src[from..i]);
                from = i + 1;
            }
            _ => {}
        }
    }
    out.push(&src[from..]);
    out
}

/// A word: symbols, each optionally followed by `(expr, ...)`.
pub fn parse_word(
    src: &str,
    vars: &[String],
    consts: &BTreeMap<String, f64>,
) -> Result<Vec<Template>, String> {
    let mut out = Vec::new();
    let mut it = src.char_indices().peekable();
    while let Some((_, c)) = it.next() {
        if c.is_whitespace() {
            continue;
        }
        if c == '(' || c == ')' || c == ',' {
            return Err(format!("`{src}`: `{c}` without a symbol before it"));
        }
        let mut args = Vec::new();
        if let Some(&(open, '(')) = it.peek() {
            it.next();
            let mut depth = 1;
            let mut close = None;
            for (i, ch) in it.by_ref() {
                match ch {
                    '(' => depth += 1,
                    ')' => {
                        depth -= 1;
                        if depth == 0 {
                            close = Some(i);
                            break;
                        }
                    }
                    _ => {}
                }
            }
            let Some(close) = close else {
                return Err(format!("`{src}`: `{c}(` is never closed"));
            };
            for a in split_args(&src[open + 1..close]) {
                args.push(compile(a.trim(), vars, consts)?);
            }
            if args.len() > MAX_ARGS {
                return Err(format!(
                    "`{src}`: `{c}` has more than {MAX_ARGS} parameters"
                ));
            }
        }
        out.push(Template { sym: c, args });
    }
    Ok(out)
}

/// A rule head: `A`, `A(l, w)` or `A(l, w) : l > 0.05`.
pub struct Head {
    pub sym: char,
    pub vars: Vec<String>,
    pub cond: Option<Expr>,
}

pub fn parse_head(src: &str, consts: &BTreeMap<String, f64>) -> Result<Head, String> {
    let (pred, cond) = match src.split_once(':') {
        Some((p, c)) => (p.trim(), Some(c.trim())),
        None => (src.trim(), None),
    };
    let mut cs = pred.chars();
    let Some(sym) = cs.next() else {
        return Err(format!("rule `{src}`: no symbol"));
    };
    let rest: String = cs.collect();
    let rest = rest.trim();
    let vars: Vec<String> = if rest.is_empty() {
        Vec::new()
    } else if let Some(inner) = rest.strip_prefix('(').and_then(|r| r.strip_suffix(')')) {
        inner.split(',').map(|v| v.trim().to_string()).collect()
    } else {
        return Err(format!(
            "rule `{src}`: the left side is one symbol, optionally with parameter names: `A` or `A(l, w)`"
        ));
    };
    for v in &vars {
        let ok = v
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            && v.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
        if !ok {
            return Err(format!("rule `{src}`: `{v}` is not a parameter name"));
        }
    }
    if vars.len() > MAX_ARGS {
        return Err(format!("rule `{src}`: more than {MAX_ARGS} parameters"));
    }
    let cond = cond.map(|c| compile(c, &vars, consts)).transpose()?;
    Ok(Head { sym, vars, cond })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expressions_words_and_heads_parse_and_evaluate() {
        let consts: BTreeMap<String, f64> = [("r".to_string(), 0.5)].into();
        let vars = vec!["l".to_string(), "w".to_string()];
        let e = compile("l * r + max(w, 2) ^ 2", &vars, &consts).unwrap_or_else(|e| panic!("{e}"));
        assert!((e.eval(&[4.0, 1.0]) - 6.0).abs() < 1e-9);
        let c = compile("l > 1 && !(w == 0)", &vars, &consts).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(c.eval(&[2.0, 1.0]), 1.0);
        assert_eq!(c.eval(&[2.0, 0.0]), 0.0);
        for (src, want) in [
            ("-2^2", -4.0),
            ("2^-1", 0.5),
            ("1e-3 * 1000", 1.0),
            ("2.5E+1 - 5", 20.0),
        ] {
            let e = compile(src, &vars, &consts).unwrap_or_else(|e| panic!("{src}: {e}"));
            assert!((e.eval(&[0.0, 0.0]) - want).abs() < 1e-9, "{src}");
        }
        let w = parse_word("!(w)F(l)[+(45)A(l*r, w)]X", &vars, &consts)
            .unwrap_or_else(|e| panic!("{e}"));
        let syms: String = w.iter().map(|t| t.sym).collect();
        assert_eq!(syms, "!F[+A]X");
        let m = w[4].eval(&[2.0, 0.1]);
        assert_eq!((m.n, m.a[0], m.a[1]), (2, 1.0, 0.1));
        let h = parse_head("A(l, w) : l > 0.1", &consts).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!((h.sym, h.vars.len()), ('A', 2));
        let Err(e) = compile("l + q", &vars, &consts) else {
            panic!("unknown name should fail")
        };
        assert!(e.contains("unknown name `q`"), "{e}");
    }
}
