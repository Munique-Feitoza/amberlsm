use amberlsm::{AmberDb, Options};
use std::io::{self, BufRead, Write};

fn main() -> amberlsm::Result<()> {
    let path = std::env::args()
        .nth(1)
        .ok_or_else(|| amberlsm::Error::Corrupt("uso: amberlsm-protocol <diretório>".into()))?;
    let db = AmberDb::open(path, Options::default())?;
    let stdin = io::stdin();
    let mut output = io::BufWriter::new(io::stdout().lock());

    for line in stdin.lock().lines() {
        let line = line.map_err(amberlsm::Error::Io)?;
        let fields: Vec<_> = line.split_whitespace().collect();
        if fields.is_empty() {
            continue;
        }
        let result = match fields[0].to_ascii_uppercase().as_str() {
            "PUT" if fields.len() == 3 => {
                db.put(&decode_hex(fields[1])?, &decode_hex(fields[2])?)?;
                "OK".to_string()
            }
            "GET" if fields.len() == 2 => match db.get(&decode_hex(fields[1])?)? {
                Some(value) => format!("VALUE {}", encode_hex(&value)),
                None => "NOT_FOUND".into(),
            },
            "DELETE" if fields.len() == 2 => {
                db.delete(&decode_hex(fields[1])?)?;
                "OK".into()
            }
            "SCAN" if fields.len() == 1 => {
                for (key, value) in db.scan()? {
                    writeln!(output, "ITEM {} {}", encode_hex(&key), encode_hex(&value))?;
                }
                "END".into()
            }
            "FLUSH" if fields.len() == 1 => {
                db.flush()?;
                "OK".into()
            }
            "COMPACT" if fields.len() == 1 => {
                db.compact()?;
                "OK".into()
            }
            _ => "ERR invalid-command".into(),
        };
        writeln!(output, "{result}")?;
        output.flush()?;
    }
    db.close()
}

fn decode_hex(value: &str) -> amberlsm::Result<Vec<u8>> {
    if !value.len().is_multiple_of(2) {
        return Err(amberlsm::Error::Corrupt("hexadecimal ímpar".into()));
    }
    (0..value.len())
        .step_by(2)
        .map(|index| {
            u8::from_str_radix(&value[index..index + 2], 16)
                .map_err(|_| amberlsm::Error::Corrupt("hexadecimal inválido".into()))
        })
        .collect()
}

fn encode_hex(value: &[u8]) -> String {
    value.iter().map(|byte| format!("{byte:02x}")).collect()
}
