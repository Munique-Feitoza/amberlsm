use std::collections::BTreeMap;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::{self, JoinHandle};

const WAL_MAGIC: &[u8; 4] = b"ALW1";
const SST_MAGIC: &[u8; 4] = b"ALS2";
const MANIFEST_NAME: &str = "MANIFEST";
const INDEX_STRIDE: usize = 16;
const PUT: u8 = 1;
const DELETE: u8 = 2;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
pub enum Error {
    Io(io::Error),
    Corrupt(String),
    KeyTooLarge,
    ValueTooLarge,
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "I/O: {error}"),
            Self::Corrupt(message) => write!(formatter, "dados corrompidos: {message}"),
            Self::KeyTooLarge => write!(formatter, "chave excede o limite configurado"),
            Self::ValueTooLarge => write!(formatter, "valor excede o limite configurado"),
        }
    }
}

impl std::error::Error for Error {}

impl From<io::Error> for Error {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

#[derive(Debug, Clone)]
pub struct Options {
    pub memtable_bytes: usize,
    pub sync_writes: bool,
    pub compaction_threshold: usize,
    pub max_key_bytes: usize,
    pub max_value_bytes: usize,
}

#[derive(Debug, Clone, Default)]
pub struct MetricsSnapshot {
    pub puts: u64,
    pub gets: u64,
    pub deletes: u64,
    pub flushes: u64,
    pub compactions: u64,
    pub bloom_negative: u64,
}

#[derive(Default)]
struct Metrics {
    puts: AtomicU64,
    gets: AtomicU64,
    deletes: AtomicU64,
    flushes: AtomicU64,
    compactions: AtomicU64,
    bloom_negative: AtomicU64,
}

impl Metrics {
    fn snapshot(&self) -> MetricsSnapshot {
        MetricsSnapshot {
            puts: self.puts.load(Ordering::Relaxed),
            gets: self.gets.load(Ordering::Relaxed),
            deletes: self.deletes.load(Ordering::Relaxed),
            flushes: self.flushes.load(Ordering::Relaxed),
            compactions: self.compactions.load(Ordering::Relaxed),
            bloom_negative: self.bloom_negative.load(Ordering::Relaxed),
        }
    }
}

impl MetricsSnapshot {
    pub fn to_json(&self) -> String {
        format!(
            "{{\"puts\":{},\"gets\":{},\"deletes\":{},\"flushes\":{},\"compactions\":{},\"bloom_negative\":{}}}",
            self.puts,
            self.gets,
            self.deletes,
            self.flushes,
            self.compactions,
            self.bloom_negative
        )
    }
}

impl Default for Options {
    fn default() -> Self {
        Self {
            memtable_bytes: 4 * 1024 * 1024,
            sync_writes: true,
            compaction_threshold: 4,
            max_key_bytes: 1024 * 1024,
            max_value_bytes: 64 * 1024 * 1024,
        }
    }
}

struct State {
    memtable: BTreeMap<Vec<u8>, Option<Vec<u8>>>,
    sstables: Vec<PathBuf>,
    estimated_bytes: usize,
    wal: File,
    next_sstable: u64,
}

pub struct AmberDb {
    path: PathBuf,
    options: Options,
    state: Arc<Mutex<State>>,
    compaction_tx: mpsc::Sender<Command>,
    worker: Option<JoinHandle<()>>,
    metrics: Arc<Metrics>,
    background_error: Arc<Mutex<Option<String>>>,
}

enum Command {
    Compact,
    Shutdown,
}

impl AmberDb {
    pub fn open(path: impl AsRef<Path>, options: Options) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        fs::create_dir_all(&path)?;
        let mut memtable = BTreeMap::new();
        let mut sstables = discover_sstables(&path)?;
        sstables.sort_by_key(|(number, _)| *number);
        let next_sstable = sstables.last().map_or(0, |(number, _)| number + 1);
        let sstable_paths = sstables.into_iter().map(|(_, path)| path).collect();

        let wal_path = path.join("wal.log");
        replay_wal(&wal_path, &mut memtable)?;
        let wal = OpenOptions::new()
            .create(true)
            .append(true)
            .read(true)
            .open(wal_path)?;
        let state = Arc::new(Mutex::new(State {
            memtable,
            sstables: sstable_paths,
            estimated_bytes: 0,
            wal,
            next_sstable,
        }));
        let (compaction_tx, commands) = mpsc::channel();
        let metrics = Arc::new(Metrics::default());
        let worker_state = Arc::clone(&state);
        let worker_path = path.clone();
        let worker_metrics = Arc::clone(&metrics);
        let background_error = Arc::new(Mutex::new(None));
        let worker_error = Arc::clone(&background_error);
        let worker = thread::Builder::new()
            .name("amberlsm-compactor".into())
            .spawn(move || {
                compaction_worker(
                    worker_path,
                    worker_state,
                    worker_metrics,
                    worker_error,
                    commands,
                )
            })
            .map_err(Error::Io)?;
        Ok(Self {
            path,
            options,
            state,
            compaction_tx,
            worker: Some(worker),
            metrics,
            background_error,
        })
    }

    pub fn put(&self, key: &[u8], value: &[u8]) -> Result<()> {
        self.metrics.puts.fetch_add(1, Ordering::Relaxed);
        self.write(PUT, key, value)
    }

    pub fn delete(&self, key: &[u8]) -> Result<()> {
        self.metrics.deletes.fetch_add(1, Ordering::Relaxed);
        self.write(DELETE, key, &[])
    }

    pub fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>> {
        self.metrics.gets.fetch_add(1, Ordering::Relaxed);
        let state = self
            .state
            .lock()
            .map_err(|_| Error::Corrupt("mutex poisoned".into()))?;
        if let Some(value) = state.memtable.get(key) {
            return Ok(value.clone());
        }
        for table in state.sstables.iter().rev() {
            if let Some(value) = lookup_sstable(table, key)? {
                return Ok(value);
            }
        }
        Ok(None)
    }

    /// Retorna todos os pares vivos em ordem lexicográfica de chave.
    pub fn scan(&self) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
        let state = self
            .state
            .lock()
            .map_err(|_| Error::Corrupt("mutex poisoned".into()))?;
        let mut entries = BTreeMap::new();
        for table in &state.sstables {
            load_sstable(table, &mut entries)?;
        }
        for (key, value) in &state.memtable {
            entries.insert(key.clone(), value.clone());
        }
        Ok(entries
            .into_iter()
            .filter_map(|(key, value)| value.map(|value| (key, value)))
            .collect())
    }

    pub fn metrics(&self) -> MetricsSnapshot {
        self.metrics.snapshot()
    }

    pub fn background_error(&self) -> Option<String> {
        self.background_error
            .lock()
            .ok()
            .and_then(|error| error.clone())
    }

    pub fn flush(&self) -> Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| Error::Corrupt("mutex poisoned".into()))?;
        flush_locked(&self.path, &mut state)?;
        clear_wal(&mut state.wal)?;
        self.metrics.flushes.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    pub fn close(self) -> Result<()> {
        let AmberDb {
            path,
            options: _,
            state,
            compaction_tx,
            mut worker,
            metrics: _,
            background_error,
        } = self;
        let mut state = state
            .lock()
            .map_err(|_| Error::Corrupt("mutex poisoned".into()))?;
        flush_locked(&path, &mut state)?;
        clear_wal(&mut state.wal)?;
        drop(state);
        let _ = compaction_tx.send(Command::Shutdown);
        if let Some(handle) = worker.take() {
            let _ = handle.join();
        }
        match background_error.lock().ok().and_then(|error| error.clone()) {
            Some(error) => Err(Error::Corrupt(format!(
                "compactação em background: {error}"
            ))),
            None => Ok(()),
        }
    }

    /// Merges all immutable tables into one table and removes obsolete versions.
    pub fn compact(&self) -> Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| Error::Corrupt("mutex poisoned".into()))?;
        let result = compact_locked(&self.path, &mut state);
        if result.is_ok() {
            self.metrics.compactions.fetch_add(1, Ordering::Relaxed);
        }
        result
    }

    fn write(&self, kind: u8, key: &[u8], value: &[u8]) -> Result<()> {
        if key.len() > self.options.max_key_bytes || key.len() > u32::MAX as usize {
            return Err(Error::KeyTooLarge);
        }
        if value.len() > self.options.max_value_bytes || value.len() > u32::MAX as usize {
            return Err(Error::ValueTooLarge);
        }
        let mut state = self
            .state
            .lock()
            .map_err(|_| Error::Corrupt("mutex poisoned".into()))?;
        append_wal(&mut state.wal, kind, key, value, self.options.sync_writes)?;
        state.estimated_bytes += key.len() + value.len() + 9;
        state
            .memtable
            .insert(key.to_vec(), (kind == PUT).then(|| value.to_vec()));
        if state.estimated_bytes >= self.options.memtable_bytes {
            flush_locked(&self.path, &mut state)?;
            clear_wal(&mut state.wal)?;
            self.metrics.flushes.fetch_add(1, Ordering::Relaxed);
            if state.sstables.len() >= self.options.compaction_threshold {
                let _ = self.compaction_tx.send(Command::Compact);
            }
        }
        Ok(())
    }
}

fn compaction_worker(
    path: PathBuf,
    state: Arc<Mutex<State>>,
    metrics: Arc<Metrics>,
    background_error: Arc<Mutex<Option<String>>>,
    commands: mpsc::Receiver<Command>,
) {
    while let Ok(command) = commands.recv() {
        match command {
            Command::Compact => match state.lock() {
                Ok(mut state) => match compact_locked(&path, &mut state) {
                    Ok(()) => {
                        metrics.compactions.fetch_add(1, Ordering::Relaxed);
                    }
                    Err(error) => {
                        if let Ok(mut slot) = background_error.lock() {
                            *slot = Some(format!("{error:?}"));
                        }
                    }
                },
                Err(_) => {
                    if let Ok(mut slot) = background_error.lock() {
                        *slot = Some("mutex do estado envenenado".into());
                    }
                }
            },
            Command::Shutdown => break,
        }
    }
}

fn append_wal(file: &mut File, kind: u8, key: &[u8], value: &[u8], sync: bool) -> Result<()> {
    let mut record = Vec::with_capacity(17 + key.len() + value.len());
    record.extend_from_slice(WAL_MAGIC);
    record.push(kind);
    record.extend_from_slice(&(key.len() as u32).to_le_bytes());
    record.extend_from_slice(&(value.len() as u32).to_le_bytes());
    record.extend_from_slice(key);
    record.extend_from_slice(value);
    record.extend_from_slice(&checksum(&record).to_le_bytes());
    file.write_all(&record)?;
    if sync {
        file.sync_data()?;
    }
    Ok(())
}

fn replay_wal(path: &Path, map: &mut BTreeMap<Vec<u8>, Option<Vec<u8>>>) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    let mut bytes = Vec::new();
    File::open(path)?.read_to_end(&mut bytes)?;
    let mut offset = 0;
    while offset + 13 <= bytes.len() {
        if &bytes[offset..offset + 4] != WAL_MAGIC {
            break;
        }
        let kind = bytes[offset + 4];
        let key_len =
            u32::from_le_bytes(bytes[offset + 5..offset + 9].try_into().unwrap()) as usize;
        let value_len =
            u32::from_le_bytes(bytes[offset + 9..offset + 13].try_into().unwrap()) as usize;
        let end = offset
            .checked_add(21 + key_len + value_len)
            .ok_or_else(|| Error::Corrupt("WAL size overflow".into()))?;
        if end > bytes.len() {
            break;
        }
        let key_start = offset + 13;
        let value_start = key_start + key_len;
        let expected = u64::from_le_bytes(bytes[end - 8..end].try_into().unwrap());
        if checksum(&bytes[offset..end - 8]) != expected {
            break;
        }
        apply(
            map,
            kind,
            &bytes[key_start..value_start],
            &bytes[value_start..value_start + value_len],
        )?;
        offset = end;
    }
    Ok(())
}

fn flush_locked(path: &Path, state: &mut State) -> Result<()> {
    if state.memtable.is_empty() {
        return Ok(());
    }
    let final_path = path.join(format!("sst-{:020}.dat", state.next_sstable));
    let temp_path = path.join(format!(".sst-{:020}.tmp", state.next_sstable));
    write_sstable(&temp_path, &state.memtable)?;
    fs::rename(&temp_path, &final_path)?;
    state.sstables.push(final_path);
    write_manifest(path, &state.sstables)?;
    state.memtable.clear();
    state.estimated_bytes = 0;
    state.next_sstable += 1;
    Ok(())
}

fn compact_locked(path: &Path, state: &mut State) -> Result<()> {
    flush_locked(path, state)?;
    clear_wal(&mut state.wal)?;
    if state.sstables.len() < 2 {
        return Ok(());
    }

    let mut merged = BTreeMap::new();
    for table in &state.sstables {
        load_sstable(table, &mut merged)?;
    }
    merged.retain(|_, value| value.is_some());

    let compacted = path.join(format!("sst-{:020}.dat", state.next_sstable));
    let temporary = path.join(format!(".sst-{:020}.tmp", state.next_sstable));
    write_sstable(&temporary, &merged)?;
    fs::rename(&temporary, &compacted)?;
    for table in &state.sstables {
        fs::remove_file(table)?;
    }
    state.sstables = vec![compacted];
    write_manifest(path, &state.sstables)?;
    state.next_sstable += 1;
    Ok(())
}

fn write_sstable(path: &Path, entries: &BTreeMap<Vec<u8>, Option<Vec<u8>>>) -> Result<()> {
    let mut data = Vec::new();
    let mut index = Vec::new();
    let mut bloom = Bloom::new(entries.len().max(1));
    for (number, (key, value)) in entries.iter().enumerate() {
        bloom.insert(key);
        if number % INDEX_STRIDE == 0 {
            index.push((key.clone(), data.len() as u64));
        }
        let kind = if value.is_some() { PUT } else { DELETE };
        let bytes = value.as_deref().unwrap_or(&[]);
        data.push(kind);
        data.extend_from_slice(&(key.len() as u32).to_le_bytes());
        data.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
        data.extend_from_slice(key);
        data.extend_from_slice(bytes);
    }
    let mut header = Vec::new();
    header.extend_from_slice(SST_MAGIC);
    header.extend_from_slice(&(index.len() as u32).to_le_bytes());
    header.extend_from_slice(&(bloom.bits.len() as u32).to_le_bytes());
    header.push(bloom.hashes);
    for (key, offset) in &index {
        header.extend_from_slice(&(key.len() as u32).to_le_bytes());
        header.extend_from_slice(key);
        header.extend_from_slice(&offset.to_le_bytes());
    }
    header.extend_from_slice(&bloom.bits);
    let mut file = File::create(path)?;
    file.write_all(&header)?;
    file.write_all(&data)?;
    file.sync_all()?;
    Ok(())
}

fn clear_wal(wal: &mut File) -> Result<()> {
    wal.set_len(0)?;
    wal.sync_data()?;
    Ok(())
}

fn read_sstable_header(file: &mut File, path: &Path) -> Result<(usize, SparseIndex, Bloom)> {
    let mut magic = [0u8; 4];
    file.read_exact(&mut magic)?;
    if &magic != SST_MAGIC {
        return Err(Error::Corrupt(format!(
            "invalid SSTable: {}",
            path.display()
        )));
    }
    let index_count = read_u32(file)? as usize;
    let bloom_len = read_u32(file)? as usize;
    let mut hash_count = [0u8; 1];
    file.read_exact(&mut hash_count)?;
    if bloom_len == 0 || hash_count[0] == 0 {
        return Err(Error::Corrupt("invalid SSTable Bloom filter".into()));
    }
    let mut index = Vec::with_capacity(index_count);
    for _ in 0..index_count {
        let key_len = read_u32(file)? as usize;
        let mut key = vec![0; key_len];
        file.read_exact(&mut key)?;
        let data_offset = read_u64(file)?;
        index.push((key, data_offset));
    }
    let mut bits = vec![0; bloom_len];
    file.read_exact(&mut bits)?;
    let data_start = file.stream_position()?;
    let file_len = file.metadata()?.len();
    if index
        .iter()
        .any(|(_, offset)| *offset > file_len.saturating_sub(data_start))
    {
        return Err(Error::Corrupt("índice aponta para fora da SSTable".into()));
    }
    Ok((
        data_start as usize,
        index,
        Bloom {
            bits,
            hashes: hash_count[0],
        },
    ))
}

fn read_u32(file: &mut File) -> Result<u32> {
    let mut bytes = [0u8; 4];
    file.read_exact(&mut bytes)?;
    Ok(u32::from_le_bytes(bytes))
}

fn read_u64(file: &mut File) -> Result<u64> {
    let mut bytes = [0u8; 8];
    file.read_exact(&mut bytes)?;
    Ok(u64::from_le_bytes(bytes))
}

fn discover_sstables(path: &Path) -> Result<Vec<(u64, PathBuf)>> {
    let manifest = path.join(MANIFEST_NAME);
    let mut tables = Vec::new();
    if manifest.exists() {
        let mut contents = String::new();
        File::open(&manifest)?.read_to_string(&mut contents)?;
        for line in contents.lines().filter(|line| !line.is_empty()) {
            let number = line
                .strip_prefix("sst-")
                .and_then(|name| name.strip_suffix(".dat"))
                .ok_or_else(|| Error::Corrupt("manifesto contém nome inválido".into()))?
                .parse::<u64>()
                .map_err(|_| Error::Corrupt("manifesto contém número inválido".into()))?;
            let table = path.join(line);
            if !table.is_file() {
                return Err(Error::Corrupt(format!(
                    "SSTable ausente no manifesto: {line}"
                )));
            }
            tables.push((number, table));
        }
    } else {
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if let Some(number) = name
                .strip_prefix("sst-")
                .and_then(|n| n.strip_suffix(".dat"))
            {
                tables.push((number.parse::<u64>().unwrap_or(0), entry.path()));
            }
        }
    }
    tables.sort_by_key(|(number, _)| *number);
    Ok(tables)
}

fn write_manifest(path: &Path, tables: &[PathBuf]) -> Result<()> {
    let temporary = path.join(".MANIFEST.tmp");
    let final_path = path.join(MANIFEST_NAME);
    let mut file = File::create(&temporary)?;
    for table in tables {
        let name = table
            .file_name()
            .ok_or_else(|| Error::Corrupt("SSTable sem nome de arquivo".into()))?;
        writeln!(file, "{}", name.to_string_lossy())?;
    }
    file.sync_all()?;
    drop(file);
    fs::rename(temporary, final_path)?;
    Ok(())
}

fn lookup_sstable(path: &Path, wanted: &[u8]) -> Result<Option<Option<Vec<u8>>>> {
    let mut file = File::open(path)?;
    let (data_start, index, bloom) = match read_sstable_header(&mut file, path) {
        Err(Error::Io(error)) if error.kind() == io::ErrorKind::UnexpectedEof => {
            return Err(Error::Corrupt(format!(
                "truncated SSTable: {}",
                path.display()
            )))
        }
        result => result?,
    };
    if !bloom.may_contain(wanted) {
        return Ok(None);
    }
    let start = index
        .partition_point(|(key, _)| key.as_slice() <= wanted)
        .saturating_sub(1);
    let relative = index.get(start).map_or(0, |(_, offset)| *offset as usize);
    let offset = data_start
        .checked_add(relative)
        .ok_or_else(|| Error::Corrupt("offset de índice excede o arquivo".into()))?;
    if offset as u64 >= file.metadata()?.len() {
        return Err(Error::Corrupt("offset de índice excede o arquivo".into()));
    }
    file.seek(SeekFrom::Start(offset as u64))?;
    loop {
        let header_position = file.stream_position()?;
        let file_length = file.metadata()?.len();
        let mut record_header = [0u8; 9];
        match file.read_exact(&mut record_header) {
            Ok(()) => {}
            Err(error)
                if error.kind() == io::ErrorKind::UnexpectedEof
                    && header_position == file_length =>
            {
                return Ok(None)
            }
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => {
                return Err(Error::Corrupt(format!(
                    "truncated SSTable: {}",
                    path.display()
                )))
            }
            Err(error) => return Err(error.into()),
        }
        let kind = record_header[0];
        let key_len = u32::from_le_bytes(record_header[1..5].try_into().unwrap()) as usize;
        let value_len = u32::from_le_bytes(record_header[5..9].try_into().unwrap()) as usize;
        let payload_size = key_len
            .checked_add(value_len)
            .ok_or_else(|| Error::Corrupt("tamanho de registro excede o arquivo".into()))?;
        let position = file.stream_position()?;
        if position > file.metadata()?.len().saturating_sub(payload_size as u64) {
            return Err(Error::Corrupt(format!(
                "truncated SSTable: {}",
                path.display()
            )));
        }
        let mut key = vec![0; key_len];
        let mut value = vec![0; value_len];
        file.read_exact(&mut key)?;
        file.read_exact(&mut value)?;
        if key == wanted {
            return Ok(Some(if kind == PUT {
                Some(value)
            } else if kind == DELETE {
                None
            } else {
                return Err(Error::Corrupt("unknown record type".into()));
            }));
        }
    }
}

fn load_sstable(path: &Path, map: &mut BTreeMap<Vec<u8>, Option<Vec<u8>>>) -> Result<()> {
    let mut bytes = Vec::new();
    File::open(path)?.read_to_end(&mut bytes)?;
    let (data_start, _, _) = parse_sstable(&bytes, path)?;
    let mut offset = data_start;
    while offset + 9 <= bytes.len() {
        let kind = bytes[offset];
        let key_len =
            u32::from_le_bytes(bytes[offset + 1..offset + 5].try_into().unwrap()) as usize;
        let value_len =
            u32::from_le_bytes(bytes[offset + 5..offset + 9].try_into().unwrap()) as usize;
        let end = offset
            .checked_add(9)
            .and_then(|value| value.checked_add(key_len))
            .and_then(|value| value.checked_add(value_len))
            .ok_or_else(|| Error::Corrupt("tamanho de registro excede o arquivo".into()))?;
        if end > bytes.len() {
            return Err(Error::Corrupt(format!(
                "truncated SSTable: {}",
                path.display()
            )));
        }
        apply(
            map,
            kind,
            &bytes[offset + 9..offset + 9 + key_len],
            &bytes[offset + 9 + key_len..end],
        )?;
        offset = end;
    }
    Ok(())
}

#[derive(Clone)]
struct Bloom {
    bits: Vec<u8>,
    hashes: u8,
}

impl Bloom {
    fn new(entries: usize) -> Self {
        Self {
            bits: vec![0; (entries * 10).max(64).div_ceil(8)],
            hashes: 7,
        }
    }

    fn insert(&mut self, key: &[u8]) {
        for hash in self.positions(key) {
            self.bits[hash / 8] |= 1 << (hash % 8);
        }
    }

    fn may_contain(&self, key: &[u8]) -> bool {
        self.positions(key)
            .into_iter()
            .all(|hash| self.bits[hash / 8] & (1 << (hash % 8)) != 0)
    }

    fn positions(&self, key: &[u8]) -> Vec<usize> {
        (0..self.hashes)
            .map(|seed| (checksum_seed(key, u64::from(seed)) as usize) % (self.bits.len() * 8))
            .collect()
    }
}

fn checksum_seed(bytes: &[u8], seed: u64) -> u64 {
    bytes
        .iter()
        .fold(0xcbf29ce484222325u64 ^ seed, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
        })
}

type SparseIndex = Vec<(Vec<u8>, u64)>;

fn parse_sstable(bytes: &[u8], path: &Path) -> Result<(usize, SparseIndex, Bloom)> {
    if bytes.len() < 13 || &bytes[..4] != SST_MAGIC {
        return Err(Error::Corrupt(format!(
            "invalid SSTable: {}",
            path.display()
        )));
    }
    let index_count = u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize;
    let bloom_len = u32::from_le_bytes(bytes[8..12].try_into().unwrap()) as usize;
    let hashes = bytes[12];
    let mut offset = 13;
    let mut index = Vec::with_capacity(index_count);
    for _ in 0..index_count {
        if offset + 4 > bytes.len() {
            return Err(Error::Corrupt("truncated SSTable index".into()));
        }
        let key_len = u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize;
        offset += 4;
        if offset + key_len + 8 > bytes.len() {
            return Err(Error::Corrupt("truncated SSTable index entry".into()));
        }
        let key = bytes[offset..offset + key_len].to_vec();
        offset += key_len;
        let data_offset = u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap());
        offset += 8;
        index.push((key, data_offset));
    }
    if offset + bloom_len > bytes.len() || bloom_len == 0 || hashes == 0 {
        return Err(Error::Corrupt("invalid SSTable Bloom filter".into()));
    }
    let data_start = offset + bloom_len;
    if index
        .iter()
        .any(|(_, data_offset)| *data_offset as usize > bytes.len() - data_start)
    {
        return Err(Error::Corrupt("índice aponta para fora da SSTable".into()));
    }
    Ok((
        data_start,
        index,
        Bloom {
            bits: bytes[offset..data_start].to_vec(),
            hashes,
        },
    ))
}

fn apply(
    map: &mut BTreeMap<Vec<u8>, Option<Vec<u8>>>,
    kind: u8,
    key: &[u8],
    value: &[u8],
) -> Result<()> {
    match kind {
        PUT => {
            map.insert(key.to_vec(), Some(value.to_vec()));
        }
        DELETE => {
            map.insert(key.to_vec(), None);
        }
        _ => return Err(Error::Corrupt("unknown record type".into())),
    }
    Ok(())
}

fn checksum(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf29ce484222325u64, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir() -> PathBuf {
        let id = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("amberlsm-test-{id}"))
    }

    #[test]
    fn put_get_delete_and_reopen() {
        let path = temp_dir();
        let db = AmberDb::open(
            &path,
            Options {
                memtable_bytes: usize::MAX,
                sync_writes: true,
                compaction_threshold: 4,
                ..Options::default()
            },
        )
        .unwrap();
        db.put(b"name", b"Amber").unwrap();
        assert_eq!(db.get(b"name").unwrap(), Some(b"Amber".to_vec()));
        db.delete(b"name").unwrap();
        assert_eq!(db.get(b"name").unwrap(), None);
        db.put(b"name", b"LSM").unwrap();
        db.close().unwrap();
        let reopened = AmberDb::open(&path, Options::default()).unwrap();
        assert_eq!(reopened.get(b"name").unwrap(), Some(b"LSM".to_vec()));
        fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn automatic_flush_survives_reopen() {
        let path = temp_dir();
        let db = AmberDb::open(
            &path,
            Options {
                memtable_bytes: 1,
                sync_writes: true,
                compaction_threshold: 4,
                ..Options::default()
            },
        )
        .unwrap();
        db.put(b"key", b"value").unwrap();
        assert_eq!(db.get(b"key").unwrap(), Some(b"value".to_vec()));
        drop(db);
        let reopened = AmberDb::open(&path, Options::default()).unwrap();
        assert_eq!(reopened.get(b"key").unwrap(), Some(b"value".to_vec()));
        fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn compaction_keeps_latest_values_and_removes_tombstones() {
        let path = temp_dir();
        let db = AmberDb::open(
            &path,
            Options {
                memtable_bytes: 1,
                sync_writes: true,
                compaction_threshold: 4,
                ..Options::default()
            },
        )
        .unwrap();
        db.put(b"key", b"old").unwrap();
        db.put(b"key", b"new").unwrap();
        db.delete(b"gone").unwrap();
        db.compact().unwrap();
        assert_eq!(db.get(b"key").unwrap(), Some(b"new".to_vec()));
        assert_eq!(db.get(b"gone").unwrap(), None);
        drop(db);

        let reopened = AmberDb::open(&path, Options::default()).unwrap();
        assert_eq!(reopened.get(b"key").unwrap(), Some(b"new".to_vec()));
        assert_eq!(reopened.get(b"gone").unwrap(), None);
        fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn concurrent_writes_are_serialized_and_counted() {
        let path = temp_dir();
        let db = Arc::new(AmberDb::open(&path, Options::default()).unwrap());
        let mut workers = Vec::new();
        for thread_id in 0..4 {
            let db = Arc::clone(&db);
            workers.push(std::thread::spawn(move || {
                for item in 0..25 {
                    let key = format!("{thread_id}:{item}");
                    db.put(key.as_bytes(), b"value").unwrap();
                }
            }));
        }
        for worker in workers {
            worker.join().unwrap();
        }
        assert_eq!(db.metrics().puts, 100);
        assert_eq!(db.get(b"3:24").unwrap(), Some(b"value".to_vec()));
        let db = match Arc::try_unwrap(db) {
            Ok(db) => db,
            Err(_) => panic!("referência compartilhada ainda ativa"),
        };
        db.close().unwrap();
        fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn corrupted_sstable_returns_an_error() {
        let path = temp_dir();
        let db = AmberDb::open(&path, Options::default()).unwrap();
        db.put(b"key", b"value").unwrap();
        db.close().unwrap();
        let table = path.join("sst-00000000000000000000.dat");
        let mut bytes = fs::read(&table).unwrap();
        bytes.truncate(5);
        fs::write(&table, bytes).unwrap();
        let reopened = AmberDb::open(&path, Options::default()).unwrap();
        assert!(matches!(reopened.get(b"key"), Err(Error::Corrupt(_))));
        fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn configured_record_limits_are_enforced() {
        let path = temp_dir();
        let db = AmberDb::open(
            &path,
            Options {
                max_key_bytes: 3,
                max_value_bytes: 4,
                ..Options::default()
            },
        )
        .unwrap();
        assert!(matches!(db.put(b"long", b"ok"), Err(Error::KeyTooLarge)));
        assert!(matches!(db.put(b"ok", b"large"), Err(Error::ValueTooLarge)));
        db.close().unwrap();
        fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn manifest_detects_missing_sstable() {
        let path = temp_dir();
        let db = AmberDb::open(&path, Options::default()).unwrap();
        db.put(b"key", b"value").unwrap();
        db.close().unwrap();
        fs::remove_file(path.join("sst-00000000000000000000.dat")).unwrap();
        assert!(matches!(
            AmberDb::open(&path, Options::default()),
            Err(Error::Corrupt(_))
        ));
        fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn truncated_wal_tail_is_ignored_after_restart() {
        let path = temp_dir();
        let db = AmberDb::open(&path, Options::default()).unwrap();
        db.put(b"durable", b"yes").unwrap();
        drop(db);
        let wal_path = path.join("wal.log");
        let mut wal = fs::read(&wal_path).unwrap();
        wal.extend_from_slice(b"ALW1\x01\x04");
        fs::write(&wal_path, wal).unwrap();
        let reopened = AmberDb::open(&path, Options::default()).unwrap();
        assert_eq!(reopened.get(b"durable").unwrap(), Some(b"yes".to_vec()));
        fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn concurrent_updates_and_deletes_are_consistent_per_key() {
        let path = temp_dir();
        let db = Arc::new(AmberDb::open(&path, Options::default()).unwrap());
        let mut workers = Vec::new();
        for thread_id in 0..4 {
            let db = Arc::clone(&db);
            workers.push(std::thread::spawn(move || {
                for item in 0..20 {
                    let key = format!("key-{thread_id}-{item}");
                    db.put(key.as_bytes(), b"first").unwrap();
                    db.put(key.as_bytes(), b"last").unwrap();
                    if item % 2 == 0 {
                        db.delete(key.as_bytes()).unwrap();
                    }
                }
            }));
        }
        for worker in workers {
            worker.join().unwrap();
        }
        for thread_id in 0..4 {
            for item in 0..20 {
                let key = format!("key-{thread_id}-{item}");
                let expected = if item % 2 == 0 {
                    None
                } else {
                    Some(b"last".to_vec())
                };
                assert_eq!(db.get(key.as_bytes()).unwrap(), expected);
            }
        }
        let db = Arc::try_unwrap(db).unwrap_or_else(|_| panic!("referência ativa"));
        db.close().unwrap();
        fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn scan_merges_tables_and_returns_sorted_live_values() {
        let path = temp_dir();
        let db = AmberDb::open(
            &path,
            Options {
                memtable_bytes: 1,
                ..Options::default()
            },
        )
        .unwrap();
        db.put(b"b", b"old").unwrap();
        db.put(b"a", b"one").unwrap();
        db.put(b"b", b"new").unwrap();
        db.delete(b"a").unwrap();
        db.put(b"c", b"three").unwrap();
        assert_eq!(
            db.scan().unwrap(),
            vec![
                (b"b".to_vec(), b"new".to_vec()),
                (b"c".to_vec(), b"three".to_vec())
            ]
        );
        db.close().unwrap();
        fs::remove_dir_all(path).unwrap();
    }
}
