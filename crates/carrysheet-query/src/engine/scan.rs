//! A light look at the query text: words and brackets outside strings,
//! quoted names and comments. Used only for notes and early messages; SQLite
//! and the authorizer decide what runs.

/// What the query text holds.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct Scan {
    /// UNION, INTERSECT or EXCEPT anywhere.
    pub compound: bool,
    /// ORDER BY anywhere.
    pub order_by: bool,
    /// An aggregate function call, such as `SUM(`.
    pub aggregate: bool,
    /// The first call of `max`, `min` or `json_extract` with more than
    /// [`super::cost::MAX_ARGUMENTS`] arguments: its name as written, and
    /// how many arguments it has.
    pub long_call: Option<(&'static str, usize)>,
}

/// Functions that do a full pass over a value per argument.
const PER_ARGUMENT: [&str; 3] = ["max", "min", "json_extract"];

const AGGREGATES: [&str; 8] = [
    "SUM",
    "AVG",
    "TOTAL",
    "COUNT",
    "MIN",
    "MAX",
    "GROUP_CONCAT",
    "STRING_AGG",
];

#[derive(Debug, PartialEq)]
enum Token {
    /// A bare word in capitals.
    Word(String),
    /// A name in double quotes, backticks or brackets, in capitals.
    Quoted(String),
    /// Any other character, such as `(`.
    Symbol(char),
    /// A string, quoted name, number or anything already read past.
    Other,
}

pub(crate) fn scan(sql: &str) -> Scan {
    let tokens = tokens(sql);
    let mut out = Scan::default();
    // Open brackets: for a call of a PER_ARGUMENT function, its index there
    // and the commas seen so far at its own level.
    let mut open: Vec<Option<(usize, usize)>> = Vec::new();
    for (i, token) in tokens.iter().enumerate() {
        match token {
            Token::Symbol('(') => {
                let name = match i.checked_sub(1).and_then(|j| tokens.get(j)) {
                    Some(Token::Word(w) | Token::Quoted(w)) => {
                        PER_ARGUMENT.iter().position(|f| f.eq_ignore_ascii_case(w))
                    }
                    _ => None,
                };
                open.push(name.map(|n| (n, 0)));
            }
            Token::Symbol(',') => {
                if let Some(Some((_, commas))) = open.last_mut() {
                    *commas += 1;
                }
            }
            Token::Symbol(')') => {
                if let Some(Some((name, commas))) = open.pop()
                    && commas >= super::cost::MAX_ARGUMENTS
                    && out.long_call.is_none()
                {
                    out.long_call = Some((PER_ARGUMENT[name], commas + 1));
                }
            }
            _ => {}
        }
        let Token::Word(word) = token else { continue };
        let next = tokens.get(i + 1);
        match word.as_str() {
            "UNION" | "INTERSECT" | "EXCEPT" => out.compound = true,
            "ORDER" if next == Some(&Token::Word("BY".into())) => out.order_by = true,
            w if AGGREGATES.contains(&w) && next == Some(&Token::Symbol('(')) => {
                out.aggregate = true;
            }
            _ => {}
        }
    }
    out
}

fn tokens(sql: &str) -> Vec<Token> {
    let mut out = Vec::new();
    let mut chars = sql.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            c if c.is_whitespace() => {}
            '-' if chars.peek() == Some(&'-') => {
                for c in chars.by_ref() {
                    if c == '\n' {
                        break;
                    }
                }
            }
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                let mut star = false;
                for c in chars.by_ref() {
                    if star && c == '/' {
                        break;
                    }
                    star = c == '*';
                }
            }
            '\'' | '"' | '`' | '[' => {
                let close = if c == '[' { ']' } else { c };
                // A doubled quote inside is part of the text; reading on
                // past it gives the same end.
                let mut text = String::new();
                for c in chars.by_ref() {
                    if c == close {
                        break;
                    }
                    text.push(c);
                }
                if c == '\'' {
                    out.push(Token::Other);
                } else {
                    out.push(Token::Quoted(text.to_ascii_uppercase()));
                }
            }
            c if c.is_alphanumeric() || c == '_' || c == '$' => {
                let mut word = String::from(c);
                while let Some(&n) = chars.peek() {
                    if n.is_alphanumeric() || n == '_' || n == '$' {
                        word.push(n);
                        chars.next();
                    } else {
                        break;
                    }
                }
                if word.starts_with(|c: char| c.is_ascii_digit()) {
                    out.push(Token::Other);
                } else {
                    out.push(Token::Word(word.to_ascii_uppercase()));
                }
            }
            c => out.push(Token::Symbol(c)),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_keywords_outside_strings_and_names() {
        let s = scan("SELECT a FROM t UNION ALL SELECT b FROM u ORDER BY 1");
        assert!(s.compound && s.order_by && !s.aggregate);
        let s = scan("SELECT 'union', \"order by\" FROM [except] -- union\n/* order by */");
        assert_eq!(s, Scan::default());
        let s = scan("SELECT region, sum (x) FROM t GROUP BY region");
        assert!(s.aggregate && !s.order_by);
        assert!(!scan("SELECT max_value, \"sum\"(x) FROM t").aggregate);
        assert!(scan("select count(*) from t").aggregate);
    }

    #[test]
    fn counts_arguments_of_max_min_and_json_extract() {
        let args = |n: usize| vec!["a"; n].join(", ");
        assert_eq!(scan(&format!("SELECT max({})", args(32))).long_call, None);
        assert_eq!(
            scan(&format!("SELECT MAX({})", args(33))).long_call,
            Some(("max", 33))
        );
        // Commas inside nested calls, strings and comments are not arguments.
        let nested = format!("SELECT min(coalesce({}), 'a,b,c' /* , , */)", args(40));
        assert_eq!(scan(&nested).long_call, None);
        assert_eq!(
            scan(&format!("SELECT \"json_extract\"(j, {})", args(40))).long_call,
            Some(("json_extract", 41))
        );
        // Other functions may have more.
        assert_eq!(
            scan(&format!("SELECT json_object({})", args(60))).long_call,
            None
        );
    }
}
