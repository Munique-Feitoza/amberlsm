# Arquitetura do AmberLSM

## Visão geral

O AmberLSM separa o caminho rápido de escrita do trabalho mais pesado de I/O.
A MemTable atende as operações recentes em memória; o WAL garante que uma
operação confirmada possa ser reconstruída; SSTables representam o estado
imutável no disco.

```mermaid
sequenceDiagram
    participant C as Cliente
    participant DB as AmberDb
    participant W as WAL
    participant M as MemTable
    participant S as SSTables

    C->>DB: put(chave, valor)
    DB->>W: append + checksum
    W-->>DB: sync_data()
    DB->>M: inserir valor
    DB-->>C: sucesso

    C->>DB: get(chave)
    DB->>M: consultar
    alt não encontrado na MemTable
        DB->>S: Bloom + índice + registro
        S-->>DB: valor ou tombstone
    end
    DB-->>C: resultado
```

## Estados de uma escrita

```mermaid
stateDiagram-v2
    [*] --> Recebida
    Recebida --> WAL: serialização válida
    WAL --> ConfirmadaNoDisco: sync habilitado
    WAL --> MemTable: modo agrupado
    ConfirmadaNoDisco --> MemTable
    MemTable --> Visível
    Visível --> Congelada: limite de bytes
    Congelada --> SSTableTemporária: flush
    SSTableTemporária --> SSTablePublicada: fsync + rename
    SSTablePublicada --> Compactação: limiar atingido
    Compactação --> SSTableConsolidada
```

## Consistência de leitura

A ordem de consulta é sempre da informação mais nova para a mais antiga:

1. MemTable ativa;
2. SSTable mais recente;
3. SSTables anteriores;
4. ausência da chave.

Um tombstone encontrado em uma camada mais nova encerra a busca e impede que
uma versão antiga seja devolvida.

## Concorrência

O estado compartilhado é protegido por `Mutex`. O worker de compactação recebe
comandos por um canal e aguarda o mesmo lock apenas durante a publicação da
compactação. Isso mantém a implementação simples para o protótipo e cria um
ponto claro para futura substituição por snapshots ou estruturas lock-free.

O `MANIFEST` registra os nomes das SSTables válidas. Ele é escrito em um arquivo
temporário, sincronizado e publicado por rename atômico, reduzindo o risco de
um reinício observar uma lista parcialmente atualizada.

## Recuperação

```mermaid
flowchart TD
    A[open diretório] --> B[enumerar SSTables]
    B --> C[ordenar por número]
    C --> D[abrir WAL]
    D --> E{registro completo e checksum válido?}
    E -->|sim| F[reaplicar operação]
    F --> E
    E -->|não| G[ignorar cauda incompleta]
    G --> H[banco disponível]
```

O WAL usa número mágico, tipo, tamanhos de chave e valor, payload e checksum.
Uma falha no final do arquivo não transforma bytes parciais em uma operação
visível.
