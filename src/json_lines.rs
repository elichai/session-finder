use std::io::{self, BufRead};

pub(crate) struct JsonLines<R> {
    reader: R,
    buffer: Vec<u8>,
}

impl<R: BufRead> JsonLines<R> {
    pub fn new(reader: R) -> Self {
        Self {
            reader,
            buffer: Vec::new(),
        }
    }

    pub fn next_line(&mut self) -> io::Result<Option<&[u8]>> {
        self.buffer.clear();
        let read = self.reader.read_until(b'\n', &mut self.buffer)?;
        Ok((read != 0).then_some(self.buffer.as_slice()))
    }
}

#[cfg(test)]
pub(crate) fn reader_failing_after(bytes: &[u8]) -> impl BufRead + '_ {
    use std::io::{BufReader, Read};

    struct Failure;
    impl Read for Failure {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::other("injected read error"))
        }
    }

    BufReader::new(bytes.chain(Failure))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_empty_invalid_and_unterminated_lines_without_retaining_previous_bytes() {
        let mut lines = JsonLines::new(b"long first line\r\n\n\xff\nlast".as_slice());
        for expected in [b"long first line\r\n".as_slice(), b"\n", b"\xff\n", b"last"] {
            assert_eq!(lines.next_line().unwrap(), Some(expected));
        }
        assert_eq!(lines.next_line().unwrap(), None);
        assert_eq!(lines.next_line().unwrap(), None);
    }
}
