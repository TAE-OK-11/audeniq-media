//! A small pull parser for ADM XML (`axml`): elements, attributes, text,
//! comments, CDATA, processing instructions and the five predefined plus
//! numeric entities. No DTDs or external entities are ever resolved.
use crate::Error;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Event<'a> {
    /// Local name (prefix dropped) and attributes (local names, decoded).
    Start(&'a str, Vec<(&'a str, String)>),
    End(&'a str),
    Text(String),
}

pub(crate) struct Parser<'a> {
    s: &'a str,
    at: usize,
    stack: Vec<&'a str>,
    /// A `<x/>` returns Start then this End.
    pending_end: Option<&'a str>,
}

/// Deepest nesting accepted.
const MAX_DEPTH: usize = 256;

fn local(name: &str) -> &str {
    name.rsplit(':').next().unwrap_or(name)
}

pub(crate) fn decode(raw: &str) -> Result<String, Error> {
    if !raw.contains('&') {
        return Ok(raw.to_owned());
    }
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        let tail = &rest[i + 1..];
        let end = tail.find(';').ok_or(Error::Invalid("XML entity".into()))?;
        let name = &tail[..end];
        let c = match name {
            "lt" => '<',
            "gt" => '>',
            "amp" => '&',
            "quot" => '"',
            "apos" => '\'',
            n if n.starts_with("#x") || n.starts_with("#X") => char::from_u32(
                u32::from_str_radix(&n[2..], 16)
                    .map_err(|_| Error::Invalid("XML character reference".into()))?,
            )
            .ok_or(Error::Invalid("XML character reference".into()))?,
            n if n.starts_with('#') => char::from_u32(
                n[1..]
                    .parse()
                    .map_err(|_| Error::Invalid("XML character reference".into()))?,
            )
            .ok_or(Error::Invalid("XML character reference".into()))?,
            _ => return Err(Error::Invalid("XML entity".into())),
        };
        out.push(c);
        rest = &tail[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

impl<'a> Parser<'a> {
    pub(crate) fn new(s: &'a str) -> Self {
        Parser {
            s,
            at: 0,
            stack: Vec::new(),
            pending_end: None,
        }
    }

    fn skip_past(&mut self, end: &str) -> Result<(), Error> {
        let i = self.s[self.at..]
            .find(end)
            .ok_or(Error::Invalid("unterminated XML construct".into()))?;
        self.at += i + end.len();
        Ok(())
    }

    pub(crate) fn next(&mut self) -> Result<Option<Event<'a>>, Error> {
        if let Some(name) = self.pending_end.take() {
            return Ok(Some(Event::End(name)));
        }
        loop {
            let rest = &self.s[self.at..];
            if rest.is_empty() {
                if !self.stack.is_empty() {
                    return Err(Error::Invalid("XML ends inside an element".into()));
                }
                return Ok(None);
            }
            if !rest.starts_with('<') {
                let i = rest.find('<').unwrap_or(rest.len());
                self.at += i;
                let text = &rest[..i];
                if self.stack.is_empty() || text.trim().is_empty() {
                    continue;
                }
                return Ok(Some(Event::Text(decode(text)?)));
            }
            if rest.starts_with("<?") {
                self.skip_past("?>")?;
                continue;
            }
            if rest.starts_with("<!--") {
                self.skip_past("-->")?;
                continue;
            }
            if let Some(cdata) = rest.strip_prefix("<![CDATA[") {
                let i = cdata
                    .find("]]>")
                    .ok_or(Error::Invalid("unterminated CDATA".into()))?;
                self.at += 9 + i + 3;
                return Ok(Some(Event::Text(cdata[..i].to_owned())));
            }
            if rest.starts_with("<!") {
                // DOCTYPE and the like: skipped, never expanded.
                self.skip_past(">")?;
                continue;
            }
            if let Some(close) = rest.strip_prefix("</") {
                let i = close
                    .find('>')
                    .ok_or(Error::Invalid("unterminated end tag".into()))?;
                let name = local(close[..i].trim());
                self.at += 2 + i + 1;
                let open = self
                    .stack
                    .pop()
                    .ok_or(Error::Invalid("unbalanced end tag".into()))?;
                if open != name {
                    return Err(Error::Invalid("mismatched end tag".into()));
                }
                return Ok(Some(Event::End(name)));
            }
            // Start tag: find its end outside quoted attribute values.
            let b = rest.as_bytes();
            let mut i = 1;
            let mut quote = 0u8;
            while i < b.len() {
                match b[i] {
                    q @ (b'"' | b'\'') if quote == 0 => quote = q,
                    q if q == quote => quote = 0,
                    b'>' if quote == 0 => break,
                    _ => {}
                }
                i += 1;
            }
            if i >= b.len() {
                return Err(Error::Invalid("unterminated start tag".into()));
            }
            let mut inner = &rest[1..i];
            let empty = inner.ends_with('/');
            if empty {
                inner = &inner[..inner.len() - 1];
            }
            self.at += i + 1;
            let name_end = inner
                .find(|c: char| c.is_ascii_whitespace())
                .unwrap_or(inner.len());
            let name = local(&inner[..name_end]);
            if name.is_empty() {
                return Err(Error::Invalid("XML element without a name".into()));
            }
            let mut attrs = Vec::new();
            let mut a = inner[name_end..].trim_start();
            while !a.is_empty() {
                let eq = a.find('=').ok_or(Error::Invalid("XML attribute".into()))?;
                let key = local(a[..eq].trim());
                let v = a[eq + 1..].trim_start();
                let q = v
                    .chars()
                    .next()
                    .ok_or(Error::Invalid("XML attribute".into()))?;
                if q != '"' && q != '\'' {
                    return Err(Error::Invalid("XML attribute".into()));
                }
                let close = v[1..]
                    .find(q)
                    .ok_or(Error::Invalid("XML attribute".into()))?;
                attrs.push((key, decode(&v[1..1 + close])?));
                a = v[1 + close + 1..].trim_start();
            }
            if empty {
                self.pending_end = Some(name);
            } else {
                if self.stack.len() >= MAX_DEPTH {
                    return Err(Error::Limit("XML nesting".into()));
                }
                self.stack.push(name);
            }
            return Ok(Some(Event::Start(name, attrs)));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events() {
        let mut p = Parser::new(
            "<?xml version=\"1.0\"?><!-- c --><a:root x:k='v&amp;w'><b n=\"1\"/>t&lt;x<![CDATA[<raw>]]></a:root>",
        );
        let mut got = Vec::new();
        while let Some(e) = p.next().unwrap() {
            got.push(e);
        }
        assert_eq!(
            got,
            [
                Event::Start("root", vec![("k", "v&w".into())]),
                Event::Start("b", vec![("n", "1".into())]),
                Event::End("b"),
                Event::Text("t<x".into()),
                Event::Text("<raw>".into()),
                Event::End("root"),
            ]
        );
    }

    #[test]
    fn malformed_is_an_error() {
        for bad in [
            "<a><b></a>",
            "<a>",
            "<a x=1/>",
            "<a>&bogus;</a>",
            "<a",
            "</a>",
        ] {
            let mut p = Parser::new(bad);
            let mut r = Ok(None);
            for _ in 0..10 {
                r = p.next();
                if !matches!(r, Ok(Some(_))) {
                    break;
                }
            }
            assert!(r.is_err(), "{bad}");
        }
        assert_eq!(decode("&#x41;&#66;").unwrap(), "AB");
    }
}
