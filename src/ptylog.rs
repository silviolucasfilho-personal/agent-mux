//! An opt-in recording of what a session's program wrote to its terminal,
//! for finding where agent-mux's screen and a real terminal's part ways
//! (a TUI drawing repeated or misplaced text inside agent-mux and not
//! outside it).
//!
//! `AGENT_MUX_PTY_LOG=<dir>` makes every session write
//! `<dir>/<time>-<profile>-<id>.ptylog`: the raw output bytes as agent-mux
//! read them, and every resize, each with the time since the session
//! started. What is typed into the session is not recorded; everything
//! the program shows is. `agent-mux pty replay <file>` feeds a recording
//! through agent-mux's emulator and prints the screen; `--raw` writes the
//! bytes alone, to play in a real terminal of the same size.
//!
//! The file is `agent-mux-ptylog 1\n`, then records:
//! `R <micros> <rows> <cols>\n` and `O <micros> <len>\n<len bytes>`.

use std::io::{BufRead, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

pub const ENV: &str = "AGENT_MUX_PTY_LOG";
const MAGIC: &str = "agent-mux-ptylog 1";

pub struct Recorder {
    file: std::fs::File,
    start: Instant,
    pub path: PathBuf,
}

impl std::fmt::Debug for Recorder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Recorder")
            .field("path", &self.path)
            .finish()
    }
}

impl Recorder {
    /// A recorder when `$AGENT_MUX_PTY_LOG` names a folder; `None`
    /// otherwise, or when the file cannot be made (recording never stops
    /// a session).
    pub fn from_env(profile: &str, id: usize, rows: u16, cols: u16) -> Option<Recorder> {
        let dir = std::env::var_os(ENV).filter(|v| !v.is_empty())?;
        Recorder::open_in(Path::new(&dir), profile, id, rows, cols).ok()
    }

    pub fn open_in(
        dir: &Path,
        profile: &str,
        id: usize,
        rows: u16,
        cols: u16,
    ) -> std::io::Result<Recorder> {
        std::fs::create_dir_all(dir)?;
        let now = time::OffsetDateTime::now_utc();
        let stamp = format!(
            "{:04}{:02}{:02}-{:02}{:02}{:02}",
            now.year(),
            u8::from(now.month()),
            now.day(),
            now.hour(),
            now.minute(),
            now.second()
        );
        let name: String = profile
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect();
        let path = dir.join(format!("{stamp}-{name}-{id}.ptylog"));
        let mut file = std::fs::File::create(&path)?;
        file.write_all(format!("{MAGIC}\n").as_bytes())?;
        let mut r = Recorder {
            file,
            start: Instant::now(),
            path,
        };
        r.resize(rows, cols);
        Ok(r)
    }

    fn micros(&self) -> u128 {
        self.start.elapsed().as_micros()
    }

    /// Bytes the program wrote, as read.
    pub fn output(&mut self, bytes: &[u8]) {
        let mut rec = format!("O {} {}\n", self.micros(), bytes.len()).into_bytes();
        rec.extend_from_slice(bytes);
        let _ = self.file.write_all(&rec);
    }

    pub fn resize(&mut self, rows: u16, cols: u16) {
        let _ = self
            .file
            .write_all(format!("R {} {rows} {cols}\n", self.micros()).as_bytes());
    }
}

/// One record of a recording.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Record {
    Output { micros: u128, bytes: Vec<u8> },
    Resize { micros: u128, rows: u16, cols: u16 },
}

/// The records of a recording, in order. A file cut short (agent-mux
/// killed mid-write) yields the records before the cut.
pub fn read(path: &Path) -> anyhow::Result<Vec<Record>> {
    let mut r = std::io::BufReader::new(std::fs::File::open(path)?);
    let mut line = String::new();
    r.read_line(&mut line)?;
    anyhow::ensure!(
        line.trim_end() == MAGIC,
        "{} is not a pty recording",
        path.display()
    );
    let mut out = Vec::new();
    loop {
        line.clear();
        if r.read_line(&mut line)? == 0 {
            break;
        }
        let parts: Vec<&str> = line.split_whitespace().collect();
        match parts.as_slice() {
            ["R", t, rows, cols] => out.push(Record::Resize {
                micros: t.parse()?,
                rows: rows.parse()?,
                cols: cols.parse()?,
            }),
            ["O", t, len] => {
                let mut bytes = vec![0; len.parse()?];
                if r.read_exact(&mut bytes).is_err() {
                    break;
                }
                out.push(Record::Output {
                    micros: t.parse()?,
                    bytes,
                });
            }
            _ => break,
        }
    }
    Ok(out)
}

/// The screen after the first `until` records (all of them when `None`),
/// through agent-mux's emulator.
pub fn replay(records: &[Record], until: Option<usize>) -> vt100::Parser {
    let (rows, cols) = records
        .iter()
        .find_map(|r| match r {
            Record::Resize { rows, cols, .. } => Some((*rows, *cols)),
            _ => None,
        })
        .unwrap_or((24, 80));
    let mut p = vt100::Parser::new(rows, cols, 1000);
    for r in records.iter().take(until.unwrap_or(usize::MAX)) {
        match r {
            Record::Output { bytes, .. } => p.process(bytes),
            Record::Resize { rows, cols, .. } => p.screen_mut().set_size(*rows, *cols),
        }
    }
    p
}

pub const USAGE: &str = "agent-mux pty replay <file> [--until N] [--list] [--raw]

  Replays a recording made with AGENT_MUX_PTY_LOG=<dir> through agent-mux's
  terminal emulator and prints the screen at the end (or after record N).
  --list   one line per record: number, time, output size or new size
  --raw    the output bytes alone, to play in a real terminal of the size
           the recording starts with (`--list` shows it)";

/// `agent-mux pty …`.
pub fn cli(args: &[String]) -> anyhow::Result<()> {
    if args.first().map(String::as_str) != Some("replay") {
        println!("{USAGE}");
        return Ok(());
    }
    let mut file = None;
    let (mut until, mut list, mut raw) = (None, false, false);
    let mut it = args[1..].iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--list" => list = true,
            "--raw" => raw = true,
            "--until" => {
                until = Some(
                    it.next()
                        .ok_or_else(|| anyhow::anyhow!("--until N"))?
                        .parse()?,
                )
            }
            _ => file = Some(PathBuf::from(a)),
        }
    }
    let file = file.ok_or_else(|| anyhow::anyhow!("which recording?\n\n{USAGE}"))?;
    let records = read(&file)?;
    let shown = &records[..until.unwrap_or(records.len()).min(records.len())];
    if list {
        for (i, r) in shown.iter().enumerate() {
            match r {
                Record::Output { micros, bytes } => {
                    println!(
                        "{:>6} {:>10.3}s out {} bytes",
                        i + 1,
                        *micros as f64 / 1e6,
                        bytes.len()
                    )
                }
                Record::Resize { micros, rows, cols } => {
                    println!(
                        "{:>6} {:>10.3}s size {rows}x{cols}",
                        i + 1,
                        *micros as f64 / 1e6
                    )
                }
            }
        }
        return Ok(());
    }
    if raw {
        let mut out = std::io::stdout().lock();
        for r in shown {
            if let Record::Output { bytes, .. } = r {
                out.write_all(bytes)?;
            }
        }
        return Ok(());
    }
    let p = replay(&records, until);
    let (rows, cols) = p.screen().size();
    println!(
        "screen {rows}x{cols} after {} of {} records:",
        shown.len(),
        records.len()
    );
    println!("{}", p.screen().contents());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_recording_round_trips_and_replays_with_its_resizes() {
        let d = tempfile::tempdir().unwrap();
        let mut r = Recorder::open_in(d.path(), "Antigravity CLI", 3, 2, 10).unwrap();
        r.output(b"hello\r\n");
        r.output(b"\x1b[2;1Hworld\n\x00\xff"); // any bytes, newlines included
        r.resize(3, 20);
        r.output(b"\x1b[3;12Hwide");
        assert!(
            r.path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .ends_with("-Antigravity-CLI-3.ptylog")
        );
        let recs = read(&r.path).unwrap();
        assert_eq!(recs.len(), 5);
        assert!(matches!(
            recs[0],
            Record::Resize {
                rows: 2,
                cols: 10,
                ..
            }
        ));
        assert!(matches!(&recs[2], Record::Output { bytes, .. } if bytes.ends_with(b"\n\x00\xff")));
        let screen = replay(&recs, None);
        assert_eq!(screen.screen().size(), (3, 20));
        assert!(screen.screen().contents().contains("wide"));
        let early = replay(&recs, Some(2));
        assert_eq!(early.screen().contents(), "hello");
    }

    #[test]
    fn a_cut_recording_keeps_what_came_before_the_cut() {
        let d = tempfile::tempdir().unwrap();
        let mut r = Recorder::open_in(d.path(), "p", 1, 2, 10).unwrap();
        r.output(b"ok");
        let path = r.path.clone();
        drop(r);
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        f.write_all(b"O 9 100\nshort").unwrap();
        assert_eq!(read(&path).unwrap().len(), 2);
        assert!(read(&d.path().join("missing")).is_err());
    }
}
