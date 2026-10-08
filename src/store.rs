//! Append-only day files of JSON lines plus the fixed-size index records that locate them.
//!
//! The day files are the record. An index is derived from them and rebuilt when it disagrees.

use crate::error::{Error, Result};
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};

/// Where one JSON line sits: its day file, byte offset, and length without the newline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rec {
    pub day: u32,
    pub off: u64,
    pub len: u32,
}

impl Rec {
    /// Offset just past this line and its newline.
    pub fn end(&self) -> u64 {
        self.off + u64::from(self.len) + 1
    }
}

pub struct DayStore {
    dir: PathBuf,
    /// Append handles with the file size this process last wrote.
    open: BTreeMap<u32, (File, u64)>,
}

impl DayStore {
    pub fn open(dir: &Path) -> Result<Self> {
        fs::create_dir_all(dir)?;
        Ok(Self { dir: dir.to_owned(), open: BTreeMap::new() })
    }

    fn path(&self, day: u32) -> PathBuf {
        let s = format!("{day:08}");
        self.dir.join(format!("{}-{}-{}.jsonl", &s[..4], &s[4..6], &s[6..]))
    }

    pub fn days(&self) -> Result<Vec<u32>> {
        let mut days = Vec::new();
        for e in fs::read_dir(&self.dir)? {
            let name = e?.file_name().to_string_lossy().into_owned();
            if let Some(stem) = name.strip_suffix(".jsonl") {
                if let Ok(d) = stem.replace('-', "").parse::<u32>() {
                    days.push(d);
                }
            }
        }
        days.sort_unstable();
        Ok(days)
    }

    /// Append one line, refusing when the file grew or shrank behind this process's back, and fsync it.
    pub fn append(&mut self, day: u32, line: &str) -> Result<Rec> {
        debug_assert!(!line.contains('\n'));
        if !self.open.contains_key(&day) {
            let path = self.path(day);
            let mut f = OpenOptions::new().create(true).read(true).append(true).open(&path)?;
            let mut size = f.metadata()?.len();
            if size > 0 {
                let mut last = [0u8; 1];
                f.read_exact_at(&mut last, size - 1)?;
                if last[0] != b'\n' {
                    f.write_all(b"\n")?;
                    f.sync_data()?;
                    size += 1;
                }
            }
            self.open.insert(day, (f, size));
        }
        let name = self.path(day).display().to_string();
        let (f, expected) = self.open.get_mut(&day).expect("just inserted");
        let found = f.metadata()?.len();
        if found != *expected {
            return Err(Error::ForeignWrite { file: name, expected: *expected, found });
        }
        let rec = Rec { day, off: *expected, len: line.len() as u32 };
        let mut buf = Vec::with_capacity(line.len() + 1);
        buf.extend_from_slice(line.as_bytes());
        buf.push(b'\n');
        f.write_all(&buf)?;
        f.sync_data()?;
        *expected += buf.len() as u64;
        Ok(rec)
    }

    pub fn read(&self, rec: Rec) -> Result<String> {
        let f = File::open(self.path(rec.day))?;
        let mut buf = vec![0u8; rec.len as usize];
        f.read_exact_at(&mut buf, rec.off)?;
        String::from_utf8(buf).map_err(|_| Error::Msg("log line is not UTF-8".into()))
    }

    /// Every line of a day file from `from`, complete or torn, with its location.
    pub fn scan(&self, day: u32, from: u64) -> Result<Vec<(Rec, String)>> {
        let data = match fs::read(self.path(day)) {
            Ok(d) => d,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        let mut out = Vec::new();
        let mut off = from as usize;
        while off < data.len() {
            let end = data[off..].iter().position(|&b| b == b'\n').map_or(data.len(), |p| off + p);
            let line = String::from_utf8_lossy(&data[off..end]).into_owned();
            out.push((Rec { day, off: off as u64, len: (end - off) as u32 }, line));
            off = end + 1;
        }
        Ok(out)
    }

    pub fn file_len(&self, day: u32) -> u64 {
        fs::metadata(self.path(day)).map_or(0, |m| m.len())
    }
}

/// A flat array of 16-byte records: offset (u64), length (u32), day (u32). A zero length is empty.
pub struct Idx {
    file: File,
}

const RECORD: u64 = 16;

impl Idx {
    pub fn open(path: &Path) -> Result<Self> {
        let file = OpenOptions::new().create(true).read(true).write(true).truncate(false).open(path)?;
        Ok(Self { file })
    }

    pub fn slots(&self) -> Result<u64> {
        Ok(self.file.metadata()?.len() / RECORD)
    }

    pub fn is_ragged(&self) -> Result<bool> {
        Ok(self.file.metadata()?.len() % RECORD != 0)
    }

    pub fn get(&self, slot: u64) -> Result<Option<Rec>> {
        let mut b = [0u8; RECORD as usize];
        match self.file.read_exact_at(&mut b, slot * RECORD) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
            Err(e) => return Err(e.into()),
        }
        let len = u32::from_le_bytes(b[8..12].try_into().expect("4 bytes"));
        if len == 0 {
            return Ok(None);
        }
        Ok(Some(Rec {
            off: u64::from_le_bytes(b[..8].try_into().expect("8 bytes")),
            len,
            day: u32::from_le_bytes(b[12..].try_into().expect("4 bytes")),
        }))
    }

    pub fn set(&self, slot: u64, rec: Rec) -> Result<()> {
        let mut b = [0u8; RECORD as usize];
        b[..8].copy_from_slice(&rec.off.to_le_bytes());
        b[8..12].copy_from_slice(&rec.len.to_le_bytes());
        b[12..].copy_from_slice(&rec.day.to_le_bytes());
        self.file.write_all_at(&b, slot * RECORD)?;
        Ok(())
    }

    pub fn read_all(&self) -> Result<Vec<Option<Rec>>> {
        let data = {
            let mut v = vec![0u8; self.file.metadata()?.len() as usize];
            self.file.read_exact_at(&mut v, 0)?;
            v
        };
        Ok(data
            .chunks_exact(RECORD as usize)
            .map(|b| {
                let len = u32::from_le_bytes(b[8..12].try_into().expect("4 bytes"));
                (len != 0).then(|| Rec {
                    off: u64::from_le_bytes(b[..8].try_into().expect("8 bytes")),
                    len,
                    day: u32::from_le_bytes(b[12..].try_into().expect("4 bytes")),
                })
            })
            .collect())
    }

    pub fn clear(&self) -> Result<()> {
        self.file.set_len(0)?;
        Ok(())
    }

    pub fn sync(&self) -> Result<()> {
        self.file.sync_data()?;
        Ok(())
    }
}
