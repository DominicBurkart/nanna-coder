use super::journal::{self, Claim, FileJournal, HubLock, Journal};
use super::StoreError;
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::fs;
use std::io;
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};

pub(super) trait Table: Clone + Default + Send {
    type Record: Serialize + DeserializeOwned;

    fn apply(&mut self, record: &Self::Record) -> Result<(), StoreError>;

    fn records(&self) -> Vec<Self::Record>;
}

struct Inner<T> {
    table: T,
    journal: Option<Box<dyn Journal>>,
}

pub(super) struct Engine<T: Table> {
    inner: Mutex<Inner<T>>,
    _claim: Option<Claim>,
}

fn encode<R: Serialize>(records: &[R]) -> Result<Vec<u8>, StoreError> {
    let mut bytes = Vec::new();
    for record in records {
        serde_json::to_writer(&mut bytes, record)
            .map_err(|_| invalid("record not serializable"))?;
        bytes.push(b'\n');
    }
    Ok(bytes)
}

pub(super) fn invalid(reason: &'static str) -> StoreError {
    StoreError::Io(io::Error::new(io::ErrorKind::InvalidData, reason))
}

fn replay<T: Table>(bytes: &[u8]) -> Result<T, StoreError> {
    let mut table = T::default();
    let mut segments = bytes.split(|b| *b == b'\n').enumerate().peekable();
    while let Some((index, segment)) = segments.next() {
        let blank = segment.iter().all(u8::is_ascii_whitespace);
        let line = index + 1;
        match serde_json::from_slice::<T::Record>(segment) {
            Ok(record) => {
                let applied = table.apply(&record);
                applied.map_err(|_| StoreError::Corrupt(line))?;
            }
            Err(_) if blank => {}
            Err(_) if segments.peek().is_none() => {
                tracing::warn!(line, "ignoring a truncated final record in the hub store");
            }
            Err(_) => return Err(StoreError::Corrupt(line)),
        }
    }
    Ok(table)
}

fn read_existing(path: &Path) -> Result<Vec<u8>, StoreError> {
    match fs::read(path) {
        Ok(bytes) => Ok(bytes),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(error.into()),
    }
}

impl<T: Table> Engine<T> {
    pub(super) fn memory() -> Self {
        Self {
            inner: Mutex::new(Inner {
                table: T::default(),
                journal: None,
            }),
            _claim: None,
        }
    }

    pub(super) fn open(path: &Path, lock: &Arc<HubLock>) -> Result<Self, StoreError> {
        let claim = lock.claim(path)?;
        let existing = read_existing(path)?;
        let table = replay::<T>(&existing)?;
        journal::rewrite_private(path, &encode(&table.records())?)?;
        let journal = FileJournal::new(journal::open_append(path)?);
        Ok(Self {
            inner: Mutex::new(Inner {
                table,
                journal: Some(Box::new(journal)),
            }),
            _claim: Some(claim),
        })
    }

    pub(super) fn open_read_only(path: &Path) -> Result<Self, StoreError> {
        let existing = read_existing(path)?;
        let table = replay::<T>(&existing)?;
        Ok(Self {
            inner: Mutex::new(Inner {
                table,
                journal: None,
            }),
            _claim: None,
        })
    }

    #[cfg(test)]
    pub(super) fn with_journal(journal: Box<dyn Journal>) -> Self {
        Self {
            inner: Mutex::new(Inner {
                table: T::default(),
                journal: Some(journal),
            }),
            _claim: None,
        }
    }

    pub(super) fn read<V>(&self, view: impl FnOnce(&T) -> V) -> V {
        let inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        view(&inner.table)
    }

    pub(super) fn commit<V>(
        &self,
        plan: impl FnOnce(&T) -> Result<(V, Vec<T::Record>), StoreError>,
    ) -> Result<V, StoreError> {
        let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        let (value, records) = plan(&inner.table)?;
        let mut next = inner.table.clone();
        for record in &records {
            next.apply(record)?;
        }
        if let Some(journal) = inner.journal.as_mut() {
            journal.append(&encode(&records)?)?;
        }
        inner.table = next;
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serializer};

    #[derive(Clone, Default)]
    struct Toy(Vec<u32>);

    #[derive(Debug, Deserialize)]
    struct Unwritable;

    impl Serialize for Unwritable {
        fn serialize<S: Serializer>(&self, _: S) -> Result<S::Ok, S::Error> {
            Err(serde::ser::Error::custom("cannot be written"))
        }
    }

    impl Table for Toy {
        type Record = Unwritable;

        fn apply(&mut self, _: &Unwritable) -> Result<(), StoreError> {
            self.0.push(1);
            Ok(())
        }

        fn records(&self) -> Vec<Unwritable> {
            self.0.iter().map(|_| Unwritable).collect()
        }
    }

    #[test]
    fn a_record_that_cannot_be_serialized_is_refused_and_not_adopted() {
        let engine = Engine::<Toy>::memory();
        let (journal, _fail, written) = crate::hub::store::contract::flaky();
        let journaled = Engine::<Toy>::with_journal(Box::new(journal));
        let result = journaled.commit(|_| Ok(((), vec![Unwritable])));
        assert!(matches!(result, Err(StoreError::Io(_))));
        assert_eq!(journaled.read(|toy| toy.0.len()), 0);
        assert!(written.lock().unwrap().is_empty());
        engine.commit(|_| Ok(((), vec![Unwritable]))).unwrap();
        assert_eq!(engine.read(|toy| toy.0.len()), 1);
    }

    #[test]
    fn a_planning_error_commits_nothing() {
        let engine = Engine::<Toy>::memory();
        let result: Result<(), StoreError> = engine.commit(|_| Err(StoreError::Locked));
        assert!(matches!(result, Err(StoreError::Locked)));
        assert_eq!(engine.read(|toy| toy.0.len()), 0);
    }

    #[test]
    fn a_poisoned_mutex_does_not_block_later_use() {
        let engine = std::sync::Arc::new(Engine::<Toy>::memory());
        let poisoner = engine.clone();
        let _ = std::thread::spawn(move || {
            poisoner.read::<()>(|_| panic!("poison the lock"));
        })
        .join();
        engine.commit(|_| Ok(((), vec![Unwritable]))).unwrap();
        assert_eq!(engine.read(|toy| toy.0.len()), 1);
    }
}
