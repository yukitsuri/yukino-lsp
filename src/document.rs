use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use tower_lsp::lsp_types::{Position, Range, Url};
use yukic_span::BytePos;

#[derive(Clone, Debug)]
pub struct Document {
    pub uri: Url,
    pub text: String,
    pub version: i32,
}

impl Document {
    pub fn new(uri: Url, text: String, version: i32) -> Self {
        Self { uri, text, version }
    }

    pub fn position_to_byte_offset(&self, position: Position) -> usize {
        let mut current_line: u32 = 0;
        let mut byte_offset = 0;

        for line in self.text.split_inclusive('\n') {
            if current_line == position.line {
                let mut current_char: u32 = 0;
                for (idx, ch) in line.char_indices() {
                    if current_char == position.character {
                        return byte_offset + idx;
                    }
                    current_char += ch.len_utf16() as u32;
                }
                return byte_offset + line.len();
            }
            byte_offset += line.len();
            current_line += 1;
        }

        self.text.len()
    }

    pub fn byte_offset_to_position(&self, offset: usize) -> Position {
        let safe_offset = offset.min(self.text.len());
        let slice = &self.text[..safe_offset];
        let line = slice.chars().filter(|&c| c == '\n').count() as u32;

        let last_nl = slice.rfind('\n').map(|idx| idx + 1).unwrap_or(0);
        let col = slice[last_nl..].encode_utf16().count() as u32;

        Position {
            line,
            character: col,
        }
    }

    pub fn span_to_range(&self, start: BytePos, end: BytePos) -> Range {
        Range {
            start: self.byte_offset_to_position(start.0 as usize),
            end: self.byte_offset_to_position(end.0 as usize),
        }
    }

    pub fn slice_span(&self, start: BytePos, end: BytePos) -> &str {
        let s = (start.0 as usize).min(self.text.len());
        let e = (end.0 as usize).min(self.text.len());
        if s <= e {
            &self.text[s..e]
        } else {
            ""
        }
    }
}

#[derive(Default, Clone)]
pub struct DocumentStore {
    docs: Arc<RwLock<HashMap<Url, Document>>>,
}

impl DocumentStore {
    pub fn new() -> Self {
        Self {
            docs: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub fn insert(&self, uri: Url, text: String, version: i32) {
        let mut lock = self.docs.write().expect("lock document store");
        lock.insert(uri.clone(), Document::new(uri, text, version));
    }

    pub fn get(&self, uri: &Url) -> Option<Document> {
        let lock = self.docs.read().expect("lock document store");
        lock.get(uri).cloned()
    }

    pub fn remove(&self, uri: &Url) {
        let mut lock = self.docs.write().expect("lock document store");
        lock.remove(uri);
    }

    pub fn all_uris(&self) -> Vec<Url> {
        let lock = self.docs.read().expect("lock document store");
        lock.keys().cloned().collect()
    }
}
