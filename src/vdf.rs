//! Valve's text KeyValues format (`libraryfolders.vdf`, `appworkshop_221100.acf`): quoted keys
//! with quoted values or `{ }` blocks, and `//` comments.

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Str(String),
    Obj(Vec<(String, Value)>),
}

impl Value {
    /// The first entry named `key` (case-insensitive, as Steam treats them).
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Obj(entries) => entries
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(key))
                .map(|(_, v)| v),
            Value::Str(_) => None,
        }
    }

    pub fn str(&self, key: &str) -> Option<&str> {
        match self.get(key)? {
            Value::Str(s) => Some(s),
            Value::Obj(_) => None,
        }
    }

    pub fn entries(&self) -> &[(String, Value)] {
        match self {
            Value::Obj(entries) => entries,
            Value::Str(_) => &[],
        }
    }
}

/// Parses a whole file: its top-level entries as one object. Malformed input gives what was read
/// up to the problem.
pub fn parse(text: &str) -> Value {
    let mut tokens = Tokens { rest: text };
    Value::Obj(entries(&mut tokens))
}

fn entries(tokens: &mut Tokens) -> Vec<(String, Value)> {
    let mut out = Vec::new();
    while let Some(token) = tokens.next() {
        let key = match token {
            Token::Str(key) => key,
            Token::Close => break,
            Token::Open => continue,
        };
        match tokens.next() {
            Some(Token::Str(value)) => out.push((key, Value::Str(value))),
            Some(Token::Open) => out.push((key, Value::Obj(entries(tokens)))),
            Some(Token::Close) | None => break,
        }
    }
    out
}

enum Token {
    Str(String),
    Open,
    Close,
}

struct Tokens<'a> {
    rest: &'a str,
}

impl Tokens<'_> {
    fn next(&mut self) -> Option<Token> {
        loop {
            self.rest = self.rest.trim_start();
            if let Some(after) = self.rest.strip_prefix("//") {
                self.rest = after.split_once('\n').map_or("", |(_, r)| r);
                continue;
            }
            break;
        }
        let mut chars = self.rest.chars();
        match chars.next()? {
            '{' => {
                self.rest = chars.as_str();
                Some(Token::Open)
            }
            '}' => {
                self.rest = chars.as_str();
                Some(Token::Close)
            }
            '"' => {
                let mut value = String::new();
                let mut escaped = false;
                for (i, c) in chars.as_str().char_indices() {
                    if escaped {
                        value.push(match c {
                            'n' => '\n',
                            't' => '\t',
                            other => other,
                        });
                        escaped = false;
                    } else if c == '\\' {
                        escaped = true;
                    } else if c == '"' {
                        self.rest = &chars.as_str()[i + 1..];
                        return Some(Token::Str(value));
                    } else {
                        value.push(c);
                    }
                }
                self.rest = "";
                Some(Token::Str(value))
            }
            _ => {
                // An unquoted word (Steam doesn't write these, but they're valid).
                let end = self
                    .rest
                    .find(|c: char| c.is_whitespace() || c == '{' || c == '}' || c == '"')
                    .unwrap_or(self.rest.len());
                let word = self.rest[..end].to_string();
                self.rest = &self.rest[end..];
                Some(Token::Str(word))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_workshop_manifest() {
        let text = r#"
"AppWorkshop"
{
	"appid"		"221100"
	// a comment
	"WorkshopItemsInstalled"
	{
		"1559212036"
		{
			"size"		"1234"
			"timeupdated"		"1700000000"
			"manifest"		"55"
		}
	}
	"path"	"C:\\Program Files (x86)\\Steam"
}"#;
        let root = parse(text);
        let app = root.get("appworkshop").unwrap();
        assert_eq!(app.str("appid"), Some("221100"));
        let item = app.get("WorkshopItemsInstalled").unwrap().get("1559212036");
        assert_eq!(item.unwrap().str("timeupdated"), Some("1700000000"));
        assert_eq!(app.str("path"), Some(r"C:\Program Files (x86)\Steam"));
    }

    #[test]
    fn survives_truncation() {
        let root = parse(r#""a" { "b" "c" "d" { "e" "#);
        assert_eq!(root.get("a").unwrap().str("b"), Some("c"));
    }
}
