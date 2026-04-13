//! OpenStep / GNUstep ASCII plist parser tuned for project.pbxproj files.
//!
//! Grammar (simplified):
//!   File      := "// !$*UTF8*$!" "\n" Dict EOF
//!   Dict      := "{" (Key "=" Value ";")* "}"
//!   Array     := "(" (Value ","?)* ")"
//!   Value     := Dict | Array | QuotedStr | UnquotedStr
//!   QuotedStr := '"' … '"'            (handles \n \t \r \\ \")
//!   UnquotedStr := [^ \t\n\r{}()=;,/]+  (but stops at comment start)
//!
//! Comments (`/* … */` and `// …`) are stripped before values and keys.

use indexmap::IndexMap;

use crate::error::ElectrolysisError;

// ── Value types ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum PbxValue {
    Str(String),
    Dict(IndexMap<String, PbxValue>),
    Array(Vec<PbxValue>),
}

impl PbxValue {
    pub fn as_str(&self) -> Option<&str> {
        match self { PbxValue::Str(s) => Some(s), _ => None }
    }
    pub fn as_dict(&self) -> Option<&IndexMap<String, PbxValue>> {
        match self { PbxValue::Dict(d) => Some(d), _ => None }
    }
    pub fn as_array(&self) -> Option<&[PbxValue]> {
        match self { PbxValue::Array(a) => Some(a), _ => None }
    }
    /// String representation for dict/str, None for array.
    #[allow(dead_code)]
    pub fn str_val(&self) -> Option<String> {
        match self {
            PbxValue::Str(s) => Some(s.clone()),
            _ => None,
        }
    }
}

// ── Project model ─────────────────────────────────────────────────────────────

/// Flat representation of a single pbxproj object.
pub type PbxObject = IndexMap<String, PbxValue>;

#[derive(Debug)]
pub struct PbxProject {
    pub root_object: String,
    pub objects: IndexMap<String, PbxObject>,
    #[allow(dead_code)]
    pub archive_version: String,
    #[allow(dead_code)]
    pub object_version: String,
}

impl PbxProject {
    pub fn get_object(&self, uuid: &str) -> Option<&PbxObject> {
        self.objects.get(uuid)
    }

    pub fn isa(&self, uuid: &str) -> Option<&str> {
        self.objects
            .get(uuid)
            .and_then(|o| o.get("isa"))
            .and_then(|v| v.as_str())
    }

    pub fn str_field<'a>(&'a self, uuid: &str, field: &str) -> Option<&'a str> {
        self.objects
            .get(uuid)
            .and_then(|o| o.get(field))
            .and_then(|v| v.as_str())
    }

    /// Return the raw `PbxValue` for a field (useful for arrays-of-dicts).
    pub fn raw_field<'a>(&'a self, uuid: &str, field: &str) -> Option<&'a PbxValue> {
        self.objects.get(uuid).and_then(|o| o.get(field))
    }

    pub fn array_field<'a>(&'a self, uuid: &str, field: &str) -> Option<Vec<String>> {
        self.objects
            .get(uuid)
            .and_then(|o| o.get(field))
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
    }
}

// ── Parser ────────────────────────────────────────────────────────────────────

pub struct PbxParser<'a> {
    src: &'a [u8],
    pos: usize,
    line: usize,
}

impl<'a> PbxParser<'a> {
    pub fn new(src: &'a str) -> Self {
        PbxParser { src: src.as_bytes(), pos: 0, line: 1 }
    }

    // ── low-level helpers ──────────────────────────────────────────────────

    #[inline]
    fn cur(&self) -> Option<u8> { self.src.get(self.pos).copied() }
    #[inline]
    fn peek(&self, n: usize) -> Option<u8> { self.src.get(self.pos + n).copied() }

    fn advance(&mut self) -> Option<u8> {
        let c = self.src.get(self.pos).copied();
        if c == Some(b'\n') { self.line += 1; }
        self.pos += 1;
        c
    }

    fn skip_ws_and_comments(&mut self) {
        loop {
            // whitespace
            while matches!(self.cur(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
                self.advance();
            }
            // block comment
            if self.cur() == Some(b'/') && self.peek(1) == Some(b'*') {
                self.pos += 2;
                while self.pos + 1 < self.src.len() {
                    if self.src[self.pos] == b'\n' { self.line += 1; }
                    if self.src[self.pos] == b'*' && self.src[self.pos + 1] == b'/' {
                        self.pos += 2;
                        break;
                    }
                    self.pos += 1;
                }
                continue;
            }
            // line comment
            if self.cur() == Some(b'/') && self.peek(1) == Some(b'/') {
                while !matches!(self.cur(), Some(b'\n') | None) {
                    self.advance();
                }
                continue;
            }
            break;
        }
    }

    fn err(&self, msg: impl Into<String>) -> ElectrolysisError {
        ElectrolysisError::parse(self.line, msg)
    }

    // ── string parsers ─────────────────────────────────────────────────────

    fn parse_quoted_string(&mut self) -> Result<String, ElectrolysisError> {
        debug_assert_eq!(self.cur(), Some(b'"'));
        self.advance(); // consume '"'
        let mut s = Vec::with_capacity(64);
        loop {
            match self.cur() {
                None => return Err(self.err("unterminated quoted string")),
                Some(b'"') => { self.advance(); break; }
                Some(b'\\') => {
                    self.advance();
                    match self.advance() {
                        Some(b'n')  => s.push(b'\n'),
                        Some(b't')  => s.push(b'\t'),
                        Some(b'r')  => s.push(b'\r'),
                        Some(b'"')  => s.push(b'"'),
                        Some(b'\\') => s.push(b'\\'),
                        Some(b'U')  => {
                            // \UXXXX — consume 4 hex digits and keep verbatim
                            s.push(b'\\');
                            s.push(b'U');
                            for _ in 0..4 {
                                if let Some(c) = self.advance() { s.push(c); }
                            }
                        }
                        Some(c) => { s.push(b'\\'); s.push(c); }
                        None    => return Err(self.err("truncated escape sequence")),
                    }
                }
                Some(c) => { s.push(c); self.advance(); }
            }
        }
        String::from_utf8(s).map_err(|_| self.err("invalid UTF-8 in quoted string"))
    }

    fn parse_unquoted_string(&mut self) -> Result<String, ElectrolysisError> {
        let start = self.pos;
        loop {
            match self.cur() {
                // Stop at delimiter characters or comment start.
                None
                | Some(b'{' | b'}' | b'(' | b')' | b'=' | b';' | b',' | b' ' | b'\t' | b'\n' | b'\r')
                => break,
                Some(b'/') if matches!(self.peek(1), Some(b'*' | b'/')) => break,
                _ => { self.advance(); }
            }
        }
        if self.pos == start {
            return Err(self.err(format!(
                "expected string value, found {:?}",
                self.cur().map(|c| c as char)
            )));
        }
        std::str::from_utf8(&self.src[start..self.pos])
            .map(|s| s.to_string())
            .map_err(|_| self.err("invalid UTF-8 in unquoted string"))
    }

    fn parse_string(&mut self) -> Result<String, ElectrolysisError> {
        self.skip_ws_and_comments();
        match self.cur() {
            Some(b'"') => self.parse_quoted_string(),
            Some(_)    => self.parse_unquoted_string(),
            None       => Err(self.err("unexpected EOF while reading string")),
        }
    }

    // ── value parsers ──────────────────────────────────────────────────────

    fn parse_dict(&mut self) -> Result<PbxValue, ElectrolysisError> {
        debug_assert_eq!(self.cur(), Some(b'{'));
        self.advance(); // consume '{'
        let mut map: IndexMap<String, PbxValue> = IndexMap::new();
        loop {
            self.skip_ws_and_comments();
            match self.cur() {
                Some(b'}') => { self.advance(); break; }
                None       => return Err(self.err("unterminated dict")),
                _          => {}
            }
            let key = self.parse_string()?;
            self.skip_ws_and_comments();
            match self.cur() {
                Some(b'=') => { self.advance(); }
                c => return Err(self.err(format!(
                    "expected '=' after key {:?}, got {:?}", key, c.map(|x| x as char)
                ))),
            }
            let val = self.parse_value()?;
            self.skip_ws_and_comments();
            if self.cur() == Some(b';') { self.advance(); }
            map.insert(key, val);
        }
        Ok(PbxValue::Dict(map))
    }

    fn parse_array(&mut self) -> Result<PbxValue, ElectrolysisError> {
        debug_assert_eq!(self.cur(), Some(b'('));
        self.advance(); // consume '('
        let mut arr: Vec<PbxValue> = Vec::new();
        loop {
            self.skip_ws_and_comments();
            match self.cur() {
                Some(b')') => { self.advance(); break; }
                None       => return Err(self.err("unterminated array")),
                _          => {}
            }
            let val = self.parse_value()?;
            arr.push(val);
            self.skip_ws_and_comments();
            if self.cur() == Some(b',') { self.advance(); }
        }
        Ok(PbxValue::Array(arr))
    }

    fn parse_value(&mut self) -> Result<PbxValue, ElectrolysisError> {
        self.skip_ws_and_comments();
        match self.cur() {
            Some(b'{') => self.parse_dict(),
            Some(b'(') => self.parse_array(),
            Some(b'"') => self.parse_quoted_string().map(PbxValue::Str),
            Some(_)    => self.parse_unquoted_string().map(PbxValue::Str),
            None       => Err(self.err("unexpected EOF")),
        }
    }

    // ── entry point ────────────────────────────────────────────────────────

    pub fn parse_file(&mut self) -> Result<PbxValue, ElectrolysisError> {
        // Skip the optional `// !$*UTF8*$!` header.
        self.skip_ws_and_comments();
        self.parse_value()
    }
}

// ── Public API ────────────────────────────────────────────────────────────────

pub fn parse_project(content: &str) -> Result<PbxProject, ElectrolysisError> {
    let mut p = PbxParser::new(content);
    let root_val = p.parse_file()?;

    let root_dict = root_val.as_dict()
        .ok_or_else(|| ElectrolysisError::structure("top-level value is not a dict"))?;

    let root_object = root_dict
        .get("rootObject")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ElectrolysisError::structure("missing rootObject"))?
        .to_string();

    let archive_version = root_dict
        .get("archiveVersion").and_then(|v| v.as_str()).unwrap_or("1").to_string();
    let object_version = root_dict
        .get("objectVersion").and_then(|v| v.as_str()).unwrap_or("46").to_string();

    let objects_val = root_dict
        .get("objects")
        .and_then(|v| v.as_dict())
        .ok_or_else(|| ElectrolysisError::structure("missing objects dict"))?;

    let mut objects: IndexMap<String, PbxObject> = IndexMap::with_capacity(objects_val.len());
    for (uuid, val) in objects_val {
        if let Some(obj_dict) = val.as_dict() {
            objects.insert(uuid.clone(), obj_dict.clone());
        }
    }

    Ok(PbxProject { root_object, objects, archive_version, object_version })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_project() {
        let src = r#"// !$*UTF8*$!
{
    archiveVersion = 1;
    classes = {};
    objectVersion = 56;
    objects = {
        AABBCCDDEEFF00112233445566 /* group */ = {
            isa = PBXGroup;
            children = (
            );
            sourceTree = "<group>";
        };
    };
    rootObject = AABBCCDDEEFF00112233445566;
}
"#;
        let proj = parse_project(src).unwrap();
        assert_eq!(proj.root_object, "AABBCCDDEEFF00112233445566");
        assert_eq!(proj.archive_version, "1");
        assert_eq!(proj.object_version, "56");
        assert_eq!(proj.objects.len(), 1);
        assert_eq!(proj.isa("AABBCCDDEEFF00112233445566"), Some("PBXGroup"));
    }

    #[test]
    fn parses_quoted_string_escapes() {
        let src = r#"{ key = "hello \"world\"\nnewline"; }"#;
        let mut p = PbxParser::new(src);
        let val = p.parse_file().unwrap();
        let d = val.as_dict().unwrap();
        let v = d["key"].as_str().unwrap();
        assert!(v.contains("\"world\""));
        assert!(v.contains('\n'));
    }
}
