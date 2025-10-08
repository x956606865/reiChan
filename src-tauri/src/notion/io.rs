use serde_json::{Map, Value};
use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::Path;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum RecordStreamError {
    #[error("unsupported file type")]
    UnsupportedFileType,
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("csv error: {0}")]
    Csv(#[from] csv::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}

pub struct RecordStream {
    inner: RecordStreamInner,
}

enum RecordStreamInner {
    Csv {
        reader: csv::Reader<File>,
        headers: Vec<String>,
    },
    JsonLines {
        reader: BufReader<File>,
        line_buf: String,
    },
    JsonArray {
        data: Vec<Value>,
        index: usize,
    },
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StreamPosition {
    pub byte_offset: u64,
    pub record_index: usize,
}

impl RecordStream {
    pub fn open<P: AsRef<Path>>(
        path: P,
        position: StreamPosition,
    ) -> Result<(Self, StreamPosition), RecordStreamError> {
        let path = path.as_ref();
        let extension = path
            .extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ext.to_ascii_lowercase());

        match extension.as_deref() {
            Some("csv") => {
                let file = File::open(path)?;
                let target_index = position.record_index;
                let mut reader = csv::ReaderBuilder::new()
                    .has_headers(true)
                    .trim(csv::Trim::All)
                    .from_reader(file);
                let headers = reader
                    .headers()
                    .map(|h| h.iter().map(|s| s.to_string()).collect::<Vec<_>>())?;

                if target_index > 0 {
                    let mut seek_applied = false;
                    if position.byte_offset > 0 {
                        let mut csv_pos = csv::Position::new();
                        csv_pos.set_byte(position.byte_offset);
                        csv_pos.set_record(target_index as u64);
                        if reader.seek(csv_pos.clone()).is_ok() {
                            seek_applied = true;
                        }
                    }
                    if !seek_applied {
                        for _ in 0..target_index {
                            if reader.records().next().is_none() {
                                break;
                            }
                        }
                    }
                }

                Ok((
                    Self {
                        inner: RecordStreamInner::Csv { reader, headers },
                    },
                    StreamPosition {
                        byte_offset: 0,
                        record_index: target_index,
                    },
                ))
            }
            Some("jsonl") | Some("jsonlines") => {
                let file = File::open(path)?;
                let mut reader = BufReader::new(file);
                let mut skipped = 0usize;
                let mut buf = String::new();
                if position.byte_offset > 0 {
                    reader.seek(SeekFrom::Start(position.byte_offset))?;
                    skipped = position.record_index;
                } else {
                    while skipped < position.record_index {
                        buf.clear();
                        let bytes = reader.read_line(&mut buf)?;
                        if bytes == 0 {
                            break;
                        }
                        if buf.trim().is_empty() {
                            continue;
                        }
                        skipped += 1;
                    }
                }

                let byte_offset = reader.stream_position()?;
                Ok((
                    Self {
                        inner: RecordStreamInner::JsonLines {
                            reader,
                            line_buf: String::new(),
                        },
                    },
                    StreamPosition {
                        byte_offset,
                        record_index: skipped,
                    },
                ))
            }
            Some("json") => {
                let file = File::open(path)?;
                let data: Value = serde_json::from_reader(file)?;
                let values = match data {
                    Value::Array(arr) => arr,
                    other => vec![other],
                };
                let index = position.record_index.min(values.len());
                Ok((
                    Self {
                        inner: RecordStreamInner::JsonArray {
                            data: values,
                            index,
                        },
                    },
                    StreamPosition {
                        byte_offset: 0,
                        record_index: index,
                    },
                ))
            }
            _ => Err(RecordStreamError::UnsupportedFileType),
        }
    }

    pub fn next_batch(
        &mut self,
        batch_size: usize,
        position: &mut StreamPosition,
    ) -> Result<Option<Vec<Value>>, RecordStreamError> {
        if batch_size == 0 {
            return Ok(Some(Vec::new()));
        }

        match &mut self.inner {
            RecordStreamInner::Csv { reader, headers } => {
                let mut collected = Vec::new();
                for record_result in reader.records().take(batch_size) {
                    let record = record_result?;
                    if record.is_empty() {
                        continue;
                    }
                    let mut obj = Map::with_capacity(headers.len());
                    for (idx, header) in headers.iter().enumerate() {
                        let value = record.get(idx).unwrap_or("");
                        obj.insert(header.clone(), Value::String(value.to_string()));
                    }
                    collected.push(Value::Object(obj));
                }

                if collected.is_empty() {
                    return Ok(None);
                }
                position.record_index += collected.len();
                position.byte_offset = reader.position().byte();
                Ok(Some(collected))
            }
            RecordStreamInner::JsonLines { reader, line_buf } => {
                let mut collected = Vec::new();
                while collected.len() < batch_size {
                    line_buf.clear();
                    let bytes = reader.read_line(line_buf)?;
                    if bytes == 0 {
                        break;
                    }
                    let trimmed = line_buf.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    let value: Value = serde_json::from_str(trimmed)?;
                    collected.push(value);
                }

                if collected.is_empty() {
                    return Ok(None);
                }
                position.record_index += collected.len();
                position.byte_offset = reader.stream_position()?;
                Ok(Some(collected))
            }
            RecordStreamInner::JsonArray { data, index } => {
                if *index >= data.len() {
                    return Ok(None);
                }
                let end = (*index + batch_size).min(data.len());
                let slice = data[*index..end].iter().cloned().collect::<Vec<_>>();
                *index = end;
                position.record_index = *index;
                Ok(Some(slice))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn resumes_csv_stream_from_offset() {
        let mut file = tempfile::Builder::new()
            .prefix("record-stream")
            .suffix(".csv")
            .tempfile()
            .unwrap();
        writeln!(file, "name,age").unwrap();
        writeln!(file, "Alice,30").unwrap();
        writeln!(file, "Bob,25").unwrap();
        let path = file.path().to_path_buf();

        let mut pos = StreamPosition::default();
        let (mut stream, mut position) = RecordStream::open(&path, pos.clone()).expect("open csv");
        let batch = stream.next_batch(1, &mut position).expect("batch");
        assert!(batch.is_some(), "first batch should exist");
        pos = position.clone();

        let (mut resumed, mut resumed_pos) =
            RecordStream::open(&path, pos.clone()).expect("resume");
        let second = resumed
            .next_batch(2, &mut resumed_pos)
            .expect("second batch");
        let values = second.expect("should have rows");
        assert_eq!(values.len(), 1, "only one row expected after resume");
        let obj = values[0].as_object().unwrap();
        assert_eq!(obj.get("name").unwrap(), "Bob");
        assert_eq!(obj.get("age").unwrap(), "25");
        assert_eq!(resumed_pos.record_index, 2);
    }

    #[test]
    fn jsonl_stream_supports_resume() {
        let mut file = tempfile::Builder::new()
            .prefix("record-stream")
            .suffix(".jsonl")
            .tempfile()
            .unwrap();
        writeln!(file, "{{\"name\":\"Alice\"}}").unwrap();
        writeln!(file, "{{\"name\":\"Bob\"}}").unwrap();
        writeln!(file).unwrap();
        writeln!(file, "{{\"name\":\"Charlie\"}}").unwrap();
        file.flush().unwrap();
        let path = file.path().to_path_buf();

        let (mut stream, mut pos) =
            RecordStream::open(&path, StreamPosition::default()).expect("open jsonl");
        let first_batch = stream
            .next_batch(2, &mut pos)
            .expect("batch")
            .expect("rows present");
        assert_eq!(first_batch.len(), 2);
        assert_eq!(first_batch[0]["name"], "Alice");
        assert_eq!(first_batch[1]["name"], "Bob");

        let (mut resumed, mut resume_pos) =
            RecordStream::open(&path, pos.clone()).expect("reopen jsonl");
        let remaining = resumed
            .next_batch(10, &mut resume_pos)
            .expect("batch")
            .expect("remaining rows");
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0]["name"], "Charlie");
        assert_eq!(resume_pos.record_index, 3);
    }
}
