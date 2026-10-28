use amberlsm::{AmberDb, Options};
use std::time::Instant;

fn main() -> amberlsm::Result<()> {
    let count = std::env::args()
        .nth(1)
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(10_000);
    let path = std::env::temp_dir().join(format!("amberlsm-bench-{}", std::process::id()));
    let db = AmberDb::open(
        &path,
        Options {
            memtable_bytes: 1024 * 1024,
            sync_writes: false,
            compaction_threshold: 4,
            ..Options::default()
        },
    )?;
    let start = Instant::now();
    for number in 0..count {
        let key = format!("key-{number}");
        db.put(key.as_bytes(), b"benchmark-value")?;
    }
    db.flush()?;
    let write_elapsed = start.elapsed();
    let read_start = Instant::now();
    let mut found = 0;
    for number in 0..count {
        let key = format!("key-{number}");
        if db.get(key.as_bytes())?.is_some() {
            found += 1;
        }
    }
    let read_elapsed = read_start.elapsed();
    let compact_start = Instant::now();
    db.compact()?;
    let compact_elapsed = compact_start.elapsed();
    println!("operações: {count}");
    println!("escrita: {:.3}s", write_elapsed.as_secs_f64());
    println!(
        "throughput escrita: {:.0} ops/s",
        count as f64 / write_elapsed.as_secs_f64()
    );
    println!("leituras encontradas: {found}/{count}");
    println!("leitura: {:.3}s", read_elapsed.as_secs_f64());
    println!(
        "throughput leitura: {:.0} ops/s",
        count as f64 / read_elapsed.as_secs_f64()
    );
    println!("compactação: {:.3}s", compact_elapsed.as_secs_f64());
    println!("métricas: {:#?}", db.metrics());
    db.close()?;
    std::fs::remove_dir_all(path)?;
    Ok(())
}
