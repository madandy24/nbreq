/// Incremental DATA encoder. It never creates a full dot-stuffed message copy.
pub(crate) struct DataEncoder {
    cursor: usize,
    suffix_len: usize,
    at_line_start: bool,
    extra_dot_pending: bool,
}

impl DataEncoder {
    pub(crate) fn new(message: &[u8]) -> Self {
        Self {
            cursor: 0,
            suffix_len: if message.ends_with(b"\r\n") { 0 } else { 2 },
            at_line_start: true,
            extra_dot_pending: false,
        }
    }

    pub(crate) fn finished(&self, message: &[u8]) -> bool {
        self.cursor == message.len() + self.suffix_len && !self.extra_dot_pending
    }

    pub(crate) fn next_chunk(&mut self, message: &[u8], limit: usize) -> Vec<u8> {
        let mut chunk = Vec::with_capacity(limit);
        while chunk.len() < limit && !self.finished(message) {
            let byte = match self.cursor.cmp(&message.len()) {
                std::cmp::Ordering::Less => message[self.cursor],
                std::cmp::Ordering::Equal => b'\r',
                std::cmp::Ordering::Greater => b'\n',
            };
            if self.at_line_start && byte == b'.' && !self.extra_dot_pending {
                chunk.push(b'.');
                self.extra_dot_pending = true;
                self.at_line_start = false;
                continue;
            }
            chunk.push(byte);
            self.cursor += 1;
            self.extra_dot_pending = false;
            self.at_line_start = byte == b'\n';
        }
        chunk
    }
}
