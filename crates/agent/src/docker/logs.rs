//! Container logs as lines. Without a terminal, Docker frames its log: an 8-byte header
//! (stream: 1 stdout, 2 stderr; then the payload length, big-endian) before each piece.
//! With one, the log is plain text.

use pocketgpu_proto::docker::LogLine;

/// Longer lines are cut, so one line can't fill a frame.
const MAX_LINE: usize = 4096;

pub struct Lines {
    tty: bool,
    framed: Vec<u8>,
    partial: [Vec<u8>; 2],
}

impl Lines {
    pub fn new(tty: bool) -> Self {
        Self {
            tty,
            framed: Vec::new(),
            partial: [Vec::new(), Vec::new()],
        }
    }

    /// The complete lines in `data`; a line cut off waits for the next piece.
    pub fn push(&mut self, data: &[u8]) -> Vec<LogLine> {
        let mut out = Vec::new();
        if self.tty {
            self.split(false, data, &mut out);
            return out;
        }
        self.framed.extend_from_slice(data);
        let mut at = 0;
        while self.framed.len() - at >= 8 {
            let head = &self.framed[at..at + 8];
            let size = u32::from_be_bytes([head[4], head[5], head[6], head[7]]) as usize;
            if self.framed.len() - at - 8 < size {
                break;
            }
            let stderr = head[0] == 2;
            let payload = self.framed[at + 8..at + 8 + size].to_vec();
            self.split(stderr, &payload, &mut out);
            at += 8 + size;
        }
        self.framed.drain(..at);
        out
    }

    /// What is left when the log ends.
    pub fn finish(&mut self) -> Vec<LogLine> {
        let mut out = Vec::new();
        for (i, rest) in self.partial.iter_mut().enumerate() {
            if !rest.is_empty() {
                out.push(line(i == 1, &std::mem::take(rest)));
            }
        }
        out
    }

    fn split(&mut self, stderr: bool, data: &[u8], out: &mut Vec<LogLine>) {
        let partial = &mut self.partial[usize::from(stderr)];
        for piece in data.split_inclusive(|b| *b == b'\n') {
            partial.extend_from_slice(piece);
            // A whole line, or one too long to wait for its end.
            if piece.ends_with(b"\n") || partial.len() > MAX_LINE {
                out.push(line(stderr, partial));
                partial.clear();
            }
        }
    }
}

fn line(stderr: bool, bytes: &[u8]) -> LogLine {
    let text = String::from_utf8_lossy(bytes);
    let text = text.trim_end_matches(['\n', '\r']);
    LogLine {
        stderr,
        text: text.chars().take(MAX_LINE).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(stream: u8, payload: &[u8]) -> Vec<u8> {
        let mut f = vec![stream, 0, 0, 0];
        f.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        f.extend_from_slice(payload);
        f
    }

    #[test]
    fn framed_logs_split_into_lines_by_stream() {
        let mut lines = Lines::new(false);
        let mut data = frame(1, b"step 1\nstep ");
        data.extend(frame(2, b"warning\n"));
        // A frame cut in half arrives in two pieces.
        let second = frame(1, b"2\r\n");
        data.extend_from_slice(&second[..5]);
        let got = lines.push(&data);
        assert_eq!(
            got,
            vec![
                LogLine {
                    stderr: false,
                    text: "step 1".into()
                },
                LogLine {
                    stderr: true,
                    text: "warning".into()
                },
            ]
        );
        let got = lines.push(&second[5..]);
        assert_eq!(
            got,
            vec![LogLine {
                stderr: false,
                text: "step 2".into()
            }]
        );
        assert!(lines.finish().is_empty());
    }

    #[test]
    fn terminal_logs_are_plain_text() {
        let mut lines = Lines::new(true);
        assert_eq!(lines.push(b"a\nb").len(), 1);
        assert_eq!(
            lines.finish(),
            vec![LogLine {
                stderr: false,
                text: "b".into()
            }]
        );
    }
}
