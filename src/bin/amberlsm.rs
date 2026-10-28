use amberlsm::{AmberDb, Options};
use std::env;
use std::process;

fn main() {
    if let Err(error) = run() {
        eprintln!("erro: {error:?}");
        process::exit(1);
    }
}

fn run() -> amberlsm::Result<()> {
    let mut args = env::args().skip(1);
    let path = args.next().ok_or_else(|| usage("faltou o diretório"))?;
    let command = args.next().ok_or_else(|| usage("faltou o comando"))?;
    let db = AmberDb::open(path, Options::default())?;

    match command.as_str() {
        "put" => {
            let key = args.next().ok_or_else(|| usage("faltou a chave"))?;
            let value = args.next().ok_or_else(|| usage("faltou o valor"))?;
            db.put(key.as_bytes(), value.as_bytes())?;
        }
        "get" => {
            let key = args.next().ok_or_else(|| usage("faltou a chave"))?;
            match db.get(key.as_bytes())? {
                Some(value) => println!("{}", String::from_utf8_lossy(&value)),
                None => println!("<não encontrado>"),
            }
        }
        "scan" => {
            for (key, value) in db.scan()? {
                println!(
                    "{}={}",
                    String::from_utf8_lossy(&key),
                    String::from_utf8_lossy(&value)
                );
            }
        }
        "delete" => {
            let key = args.next().ok_or_else(|| usage("faltou a chave"))?;
            db.delete(key.as_bytes())?;
        }
        "flush" => db.flush()?,
        "compact" => db.compact()?,
        "metrics" => println!("{:#?}", db.metrics()),
        "metrics-json" => println!("{}", db.metrics().to_json()),
        _ => return Err(usage("comando desconhecido")),
    }
    db.close()
}

fn usage(message: &str) -> amberlsm::Error {
    eprintln!("{message}");
    eprintln!("uso: amberlsm <diretório> <put|get|scan|delete|flush|compact|metrics|metrics-json> [argumentos]");
    amberlsm::Error::Corrupt("uso inválido da CLI".into())
}
