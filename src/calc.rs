//! The `= 2*(3+4)` inline calculator: a dependency-free recursive-descent
//! evaluator over f64. Precedence: `+ -` < `* / %` < `^` (right-assoc) <
//! unary minus < parentheses.

struct Parser<'a> {
    chars: &'a [u8],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn skip_ws(&mut self) {
        while self
            .chars
            .get(self.pos)
            .is_some_and(|c| c.is_ascii_whitespace())
        {
            self.pos += 1;
        }
    }

    fn eat(&mut self, want: u8) -> bool {
        self.skip_ws();
        if self.chars.get(self.pos) == Some(&want) {
            self.pos += 1;
            return true;
        }
        false
    }

    fn peek(&mut self) -> Option<u8> {
        self.skip_ws();
        self.chars.get(self.pos).copied()
    }

    fn expr(&mut self) -> Option<f64> {
        let mut acc = self.term()?;
        loop {
            if self.eat(b'+') {
                acc += self.term()?;
            } else if self.eat(b'-') {
                acc -= self.term()?;
            } else {
                return Some(acc);
            }
        }
    }

    fn term(&mut self) -> Option<f64> {
        let mut acc = self.factor()?;
        loop {
            if self.eat(b'*') {
                acc *= self.factor()?;
            } else if self.eat(b'/') {
                acc /= self.factor()?;
            } else if self.eat(b'%') {
                acc %= self.factor()?;
            } else {
                return Some(acc);
            }
        }
    }

    /// `^` binds tighter than unary minus and associates right:
    /// `2^3^2` is 512 and `-3^2` is -9.
    fn factor(&mut self) -> Option<f64> {
        let mut negations = 0usize;
        while self.eat(b'-') {
            negations += 1;
        }
        let base = self.primary()?;
        let value = if self.eat(b'^') {
            base.powf(self.factor()?)
        } else {
            base
        };
        Some(if negations % 2 == 1 { -value } else { value })
    }

    fn primary(&mut self) -> Option<f64> {
        if self.eat(b'(') {
            let inner = self.expr()?;
            return self.eat(b')').then_some(inner);
        }
        self.number()
    }

    fn number(&mut self) -> Option<f64> {
        self.skip_ws();
        let start = self.pos;
        while self.chars.get(self.pos).is_some_and(|c| c.is_ascii_digit()) {
            self.pos += 1;
        }
        if self.chars.get(self.pos) == Some(&b'.') {
            self.pos += 1;
            while self.chars.get(self.pos).is_some_and(|c| c.is_ascii_digit()) {
                self.pos += 1;
            }
        }
        if self.pos == start || (self.pos == start + 1 && self.chars[start] == b'.') {
            return None;
        }
        std::str::from_utf8(&self.chars[start..self.pos])
            .ok()?
            .parse()
            .ok()
    }
}

/// Evaluates an arithmetic expression; None on parse errors, trailing
/// garbage, or a non-finite result (division by zero, overflow).
pub fn eval(expr: &str) -> Option<f64> {
    let mut p = Parser {
        chars: expr.as_bytes(),
        pos: 0,
    };
    let v = p.expr()?;
    if p.peek().is_some() {
        return None; // trailing garbage
    }
    v.is_finite().then_some(v)
}

/// "14", "3.5", "0.3333333333" — integers plain, fractions trimmed.
pub fn format_result(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e15 {
        return format!("{}", v as i64);
    }
    let s = format!("{v:.10}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn precedence_and_parens() {
        assert_eq!(eval("2+3*4"), Some(14.0));
        assert_eq!(eval("(2+3)*4"), Some(20.0));
        assert_eq!(eval("7%3"), Some(1.0));
        assert_eq!(eval("7/2"), Some(3.5));
        assert_eq!(eval(" 2 * ( 3 + 4 ) / 7 "), Some(2.0));
    }

    #[test]
    fn exponent_is_right_associative_and_tighter_than_unary_minus() {
        assert_eq!(eval("2^3^2"), Some(512.0));
        assert_eq!(eval("-3^2"), Some(-9.0));
        assert_eq!(eval("(-3)^2"), Some(9.0));
    }

    #[test]
    fn leading_dot_and_unary_chains() {
        assert_eq!(eval(".5*2"), Some(1.0));
        assert_eq!(eval("--4"), Some(4.0));
    }

    #[test]
    fn invalid_input_is_none() {
        assert_eq!(eval("2 +"), None);
        assert_eq!(eval("abc"), None);
        assert_eq!(eval("1/0"), None);
        assert_eq!(eval(""), None);
        assert_eq!(eval("2 2"), None); // trailing garbage
        assert_eq!(eval("(2"), None);
        assert_eq!(eval("."), None);
    }

    #[test]
    fn results_format_cleanly() {
        assert_eq!(format_result(14.0), "14");
        assert_eq!(format_result(3.5), "3.5");
        assert_eq!(format_result(1.0 / 3.0), "0.3333333333");
        assert_eq!(format_result(-2.0), "-2");
    }
}
