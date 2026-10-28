# Benchmark do AmberLSM

O benchmark incluído no projeto mede escrita, leitura e compactação. A escrita
usa `sync_writes` desabilitado para medir capacidade de ingestão sem transformar
cada operação em um `fsync` individual.

Execute:

```bash
cargo run --release --bin amberlsm-bench -- 100000
```

Registre sempre:

- versão do Rust;
- CPU e memória;
- sistema operacional e filesystem;
- quantidade de operações;
- tamanho das chaves e valores;
- configuração de `sync_writes`;
- tempo total e operações por segundo;
- quantidade de leituras encontradas;
- tempo da compactação;
- métricas impressas pela aplicação.

Esse benchmark não deve ser comparado diretamente com bancos de produção sem
definir o mesmo nível de durabilidade. O próximo passo é adicionar cenários
separados para atualização, exclusão e escrita com `sync_writes=true`.
