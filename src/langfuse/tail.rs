//! Poll-based transcript tailer: offset tracking, a remainder buffer for
//! torn trailing lines, truncation detection, and the bounded prime pass for
//! resumed sessions.
//!
//! All three CLIs append-and-flush whole lines, so only newline-terminated
//! lines are ever surfaced; a partial trailing line waits in the remainder
//! buffer for the next poll.

use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

#[derive(Debug, PartialEq)]
pub enum TailPoll {
    /// File unchanged (or still absent).
    NoChange,
    /// Newly completed lines since the last poll.
    Lines(Vec<String>),
    /// The file shrank (unexpected truncation/rewrite). The caller must
    /// reset its assembler state and re-prime without emitting.
    Truncated,
}

pub struct Tailer {
    path: PathBuf,
    offset: u64,
    remainder: Vec<u8>,
}

impl Tailer {
    /// Tails from byte 0 — everything already in the file is surfaced by the
    /// first poll. This is the right constructor for watch-adopted files and
    /// fresh Claude sessions: their head must be exported, not swallowed.
    pub fn new(path: &Path) -> Tailer {
        Tailer {
            path: path.to_path_buf(),
            offset: 0,
            remainder: Vec::new(),
        }
    }

    /// The prime pass for `Known` resume correlations: reads the existing
    /// content (tail-biased, at most `max_bytes`) for the caller to replay
    /// through the assembler *without emitting*, and returns a tailer
    /// positioned at EOF. The bool is true when the read was truncated by
    /// `max_bytes` (the caller must salt its trace ids).
    pub fn prime(path: &Path, max_bytes: u64) -> std::io::Result<(Tailer, Vec<String>, bool)> {
        let mut file = std::fs::File::open(path)?;
        let len = file.metadata()?.len();
        let (start, truncated) = if len > max_bytes {
            (len - max_bytes, true)
        } else {
            (0, false)
        };
        file.seek(SeekFrom::Start(start))?;
        let mut reader = BufReader::new(file);
        let mut lines = Vec::new();
        let mut consumed = start;
        let mut buf = Vec::new();
        let mut first = true;
        loop {
            buf.clear();
            let n = reader.read_until(b'\n', &mut buf)?;
            if n == 0 {
                break;
            }
            let complete = buf.ends_with(b"\n");
            if !complete {
                // partial trailing line: leave it for the tailer's remainder
                break;
            }
            consumed += n as u64;
            // when tail-biased, the first "line" is almost surely a partial
            // line cut at the byte boundary — skip it
            if !(first && truncated) {
                lines.push(String::from_utf8_lossy(&buf[..buf.len() - 1]).into_owned());
            }
            first = false;
        }
        Ok((
            Tailer {
                path: path.to_path_buf(),
                offset: consumed,
                remainder: Vec::new(),
            },
            lines,
            truncated,
        ))
    }

    pub fn poll(&mut self) -> TailPoll {
        let Ok(meta) = std::fs::metadata(&self.path) else {
            return TailPoll::NoChange; // ENOENT is a normal tick
        };
        let len = meta.len();
        if len < self.offset {
            self.offset = 0;
            self.remainder.clear();
            return TailPoll::Truncated;
        }
        if len == self.offset {
            return TailPoll::NoChange;
        }
        let Ok(mut file) = std::fs::File::open(&self.path) else {
            return TailPoll::NoChange;
        };
        if file.seek(SeekFrom::Start(self.offset)).is_err() {
            return TailPoll::NoChange;
        }
        let mut new_bytes = Vec::with_capacity((len - self.offset) as usize);
        if file.take(len - self.offset).read_to_end(&mut new_bytes).is_err() {
            return TailPoll::NoChange;
        }
        self.offset += new_bytes.len() as u64;
        self.remainder.extend_from_slice(&new_bytes);
        let mut lines = Vec::new();
        while let Some(nl) = self.remainder.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.remainder.drain(..=nl).collect();
            lines.push(String::from_utf8_lossy(&line[..line.len() - 1]).into_owned());
        }
        if lines.is_empty() {
            TailPoll::NoChange
        } else {
            TailPoll::Lines(lines)
        }
    }

    /// One best-effort parse of a held partial line at session end.
    pub fn take_remainder(&mut self) -> Option<String> {
        if self.remainder.is_empty() {
            None
        } else {
            let bytes = std::mem::take(&mut self.remainder);
            Some(String::from_utf8_lossy(&bytes).into_owned())
        }
    }
}
