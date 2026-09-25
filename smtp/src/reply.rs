use crate::client::ReplySummary;

const MAX_LINE: usize = 512;
const MAX_TOTAL: usize = 8 * 1024;
const MAX_LINES: usize = 32;

fn text_byte(byte: u8) -> bool {
    byte == b'\t' || (b' '..=b'~').contains(&byte)
}

pub(crate) struct Reply {
    pub(crate) code: u16,
    pub(crate) lines: Vec<String>,
}

impl Reply {
    pub(crate) fn summary(&self) -> ReplySummary {
        ReplySummary {
            code: self.code,
            text: self.lines.join("\n"),
        }
    }

    pub(crate) fn advertises(&self, extension: &str) -> bool {
        self.lines.iter().skip(1).any(|line| {
            line.split_ascii_whitespace()
                .next()
                .is_some_and(|word| word.eq_ignore_ascii_case(extension))
        })
    }
}

#[derive(Default)]
pub(crate) struct ReplyParser {
    line: Vec<u8>,
    lines: Vec<String>,
    code: Option<u16>,
    total: usize,
}

impl ReplyParser {
    pub(crate) fn push(&mut self, byte: u8) -> Result<Option<Reply>, ()> {
        self.total = self.total.saturating_add(1);
        if self.total > MAX_TOTAL || self.line.len() >= MAX_LINE {
            return Err(());
        }
        self.line.push(byte);
        if byte != b'\n' {
            if byte != b'\r' && !text_byte(byte) {
                return Err(());
            }
            return Ok(None);
        }
        if self.line.len() < 5 || self.line[self.line.len() - 2] != b'\r' {
            return Err(());
        }
        let bytes = &self.line;
        let bare_code = bytes.len() == 5;
        if !(b'2'..=b'5').contains(&bytes[0])
            || !(b'0'..=b'5').contains(&bytes[1])
            || !bytes[2].is_ascii_digit()
            || (!bare_code && !matches!(bytes[3], b'-' | b' '))
        {
            return Err(());
        }
        if !bare_code
            && bytes[4..bytes.len() - 2]
                .iter()
                .any(|byte| !text_byte(*byte))
        {
            return Err(());
        }
        let code = u16::from(bytes[0] - b'0') * 100
            + u16::from(bytes[1] - b'0') * 10
            + u16::from(bytes[2] - b'0');
        if self.code.is_some_and(|first| first != code) {
            return Err(());
        }
        self.code = Some(code);
        if self.lines.len() >= MAX_LINES {
            return Err(());
        }
        let text = if bare_code {
            String::new()
        } else {
            String::from_utf8(bytes[4..bytes.len() - 2].to_vec()).map_err(|_| ())?
        };
        self.lines.push(text);
        let final_line = bare_code || bytes[3] == b' ';
        self.line.clear();
        if !final_line {
            return Ok(None);
        }
        let lines = std::mem::take(&mut self.lines);
        self.code = None;
        self.total = 0;
        Ok(Some(Reply { code, lines }))
    }
}
